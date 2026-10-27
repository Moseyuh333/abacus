mod support;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use abacus_agent::{
    agent::{AgentEvent, ApprovalDecision, DoneReason, TurnOptions},
    config::ProviderProtocol,
    provider::Provider,
    tools::tool_specs,
};
use serde_json::json;
use support::{Mock, Reply, calls, contents, project, says, turn};

const EDIT: &str = r#"{"path":"value.txt","old_text":"old\n","new_text":"new\n"}"#;

fn edit_call() -> String {
    calls(&[("edit_1", "edit_file", serde_json::from_str(EDIT).unwrap())])
}

/// The session's YOLO switch, off: AUTO mode.
fn auto() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// A command with an effect a test can see, and that no rule settles —
/// `touch` is neither pure inspection nor destructive — so AUTO puts it to
/// the reviewer.
fn touch_marker() -> String {
    calls(&[("c1", "run_command", json!({"command": "touch marker.txt"}))])
}

#[tokio::test]
async fn streamed_agent_searches_workspace_and_finishes() {
    let project = project(&[("main.rs", "fn main() { /* needle */ }\n")]);
    let mock = Mock::script([
        calls(&[("call_1", "grep", json!({"query": "needle"}))]),
        says("Found the reference in main.rs."),
    ])
    .await;

    let mut searched = false;
    let (completed, _) = turn(
        project.provider(&mock),
        project.asks("Find needle"),
        TurnOptions { allow_subagents: true, ..project.options() },
        |event| searched |= matches!(event, AgentEvent::ToolStarted { name, .. } if name == "grep"),
    )
    .await;
    mock.finish().await;

    assert!(searched);
    assert!(completed.iter().any(|message| {
        message["role"] == "tool" && message["content"].as_str().is_some_and(|content| content.contains("main.rs:1"))
    }));
    assert_eq!(completed.last().unwrap()["content"], "Found the reference in main.rs.");
}

/// The bug this pins: interrupting used to abort the agent task, and since
/// `messages` lived inside that task, every tool result from the turn was
/// discarded. The edits stayed on disk while the model lost all memory of
/// making them.
#[tokio::test]
async fn a_cancelled_turn_keeps_the_work_it_already_did() {
    let project = project(&[("main.rs", "fn main() { /* needle */ }\n")]);
    // The second request is accepted but never answered: the stalled-stream
    // case, where cancellation has to be noticed without a chunk arriving.
    let mut search = Some(calls(&[("call_1", "grep", json!({"query": "needle"}))]));
    let mock = Mock::answering(move |_| search.take().map_or(Reply::Hang, Reply::Send)).await;

    let cancel = Arc::new(AtomicBool::new(false));
    let raised = cancel.clone();
    let (completed, reason) = turn(
        project.provider(&mock),
        project.asks("Find needle"),
        TurnOptions { cancel, allow_subagents: true, ..project.options() },
        // Cancel the moment the first tool has run, mimicking a user pressing
        // esc partway through.
        |event| {
            if matches!(event, AgentEvent::ToolFinished { .. }) {
                raised.store(true, Ordering::Relaxed);
            }
        },
    )
    .await;
    mock.abort();

    assert_eq!(reason, DoneReason::Interrupted);
    // The assistant's tool call and the tool's result both survive, so the
    // next turn knows the search happened.
    assert!(
        completed.iter().any(|message| message["role"] == "assistant" && message["tool_calls"].is_array()),
        "the assistant's tool call should be in history"
    );
    let tool_result =
        completed.iter().find(|message| message["role"] == "tool").expect("the tool result should be in history");
    assert_eq!(tool_result["name"], "grep");
}

#[tokio::test]
async fn responses_protocol_uses_responses_endpoint_and_stream_format() {
    let project = project(&[]);
    let mock = Mock::script([concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ready\"}\n\n",
        "data: {\"type\":\"response.completed\"}\n\n"
    )
    .to_owned()])
    .await;
    let mut config = project.config(mock.address);
    config.connection.protocol = ProviderProtocol::Responses;
    let (deltas, mut streamed) = tokio::sync::mpsc::unbounded_channel();
    let completion = Provider::new(&config)
        .unwrap()
        .complete(&[json!({"role":"user","content":"hello"})], &tool_specs(), deltas, &AtomicBool::new(false))
        .await
        .unwrap();

    assert_eq!(completion.content, "ready");
    assert_eq!(streamed.try_recv().unwrap(), abacus_agent::provider::Chunk::Text("ready".to_owned()));
    let request = &mock.finish().await[0];
    assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(request.contains("\"input\""));
    assert!(request.contains("\"name\":\"grep\""));
}

/// An edit the executor keeps inside the workspace is what the user asked an
/// agent for: AUTO runs it at once, with neither a reviewer call nor a prompt.
/// The mock serves two requests; a review in between would take the second
/// and leave the turn hanging.
#[tokio::test]
async fn auto_runs_workspace_edits_without_a_review_or_a_prompt() {
    let project = project(&[("value.txt", "old\n")]);
    let mock = Mock::script([edit_call(), says("Updated value.txt.")]).await;

    turn(
        project.provider(&mock),
        project.asks("update the value"),
        TurnOptions { yolo: auto(), allow_subagents: true, ..project.options() },
        |event| {
            if let AgentEvent::Approval(request) = event {
                panic!("AUTO asked about a workspace edit: {}", request.tool);
            }
        },
    )
    .await;
    let requests = mock.finish().await;

    assert_eq!(project.read("value.txt"), "new\n");
    assert_eq!(requests.len(), 2, "the turn's two requests and no review");
}

/// A model served without native function-calling emits a Hermes-format tool
/// call as assistant TEXT (no `tool_calls` field). With `tool_format` set, the
/// provider must parse it and the agent must dispatch the tool.
#[tokio::test]
async fn text_emitted_tool_calls_are_parsed_when_native_calls_absent() {
    use abacus_agent::tool_format::{ToolFormat, render_hermes_call};
    let project = project(&[("target.txt", "hello\n")]);
    let call = render_hermes_call("read_file", r#"{"path":"target.txt"}"#);
    let mock = Mock::script([says(&format!("Reading.\n{call}")), says("Done, target.txt contains hello.")]).await;
    let config = abacus_agent::config::Config { tool_format: ToolFormat::Hermes, ..project.config(mock.address) };

    let (mut saw_read, mut saw_result) = (false, false);
    turn(
        Provider::new(&config).unwrap(),
        project.asks("read target.txt"),
        TurnOptions { yolo: auto(), allow_subagents: true, ..project.options() },
        |event| match event {
            AgentEvent::ToolStarted { name, summary } => {
                assert_eq!(name, "read_file");
                assert!(summary.contains("target.txt"));
                saw_read = true;
            }
            AgentEvent::ToolFinished { name, output } => {
                assert_eq!(name, "read_file");
                assert!(output.contains("hello"), "tool output should contain file content");
                saw_result = true;
            }
            _ => {}
        },
    )
    .await;
    mock.finish().await;

    assert!(saw_read, "text-emitted read_file call must be parsed and dispatched");
    assert!(saw_result, "read_file must return the file contents");
}

/// The reviewer sees what the user said and the action, and its BLOCK stops
/// the action: the agent reads why, and nothing ran.
#[tokio::test]
async fn auto_review_blocks_an_action_the_user_did_not_ask_for() {
    let project = project(&[]);
    let mock = Mock::script([
        touch_marker(),
        says("BLOCK: creating files is not part of explaining the code."),
        says("I will leave the files alone."),
    ])
    .await;

    let mut output = String::new();
    turn(
        project.provider(&mock),
        project.asks("explain what this repository does"),
        TurnOptions { yolo: auto(), ..project.options() },
        |event| match event {
            AgentEvent::ToolFinished { name, output: text } if name == "run_command" => output = text,
            AgentEvent::Approval(_) => panic!("a decided review must not ask the person"),
            _ => {}
        },
    )
    .await;
    let requests = mock.finish().await;

    assert!(output.starts_with("Blocked by AUTO mode"), "{output}");
    assert!(output.contains("not part of explaining the code"), "the reviewer's reason reaches the agent: {output}");
    assert!(!project.workspace.join("marker.txt").exists(), "a blocked command does not run");
    let review = &requests[1];
    assert!(review.contains("permission check"), "the second request is the review: {review}");
    assert!(review.contains("explain what this repository does"), "the reviewer sees the user's words");
    assert!(review.contains("touch marker.txt"), "and the action");
}

#[tokio::test]
async fn auto_review_lets_an_allowed_action_run() {
    let project = project(&[]);
    let mock = Mock::script([touch_marker(), says("ALLOW"), says("Created it.")]).await;

    turn(
        project.provider(&mock),
        project.asks("create marker.txt"),
        TurnOptions { yolo: auto(), ..project.options() },
        |event| {
            if let AgentEvent::Approval(_) = event {
                panic!("an allowed action must not ask the person");
            }
        },
    )
    .await;
    mock.finish().await;

    assert!(project.workspace.join("marker.txt").exists(), "the allowed command ran");
}

/// A reviewer that answers with no verdict leaves the person as the only
/// other authority: AUTO asks them instead of guessing either way.
#[tokio::test]
async fn an_undecided_review_asks_the_person() {
    let project = project(&[]);
    let mock = Mock::script([touch_marker(), says("Hard to say without more context."), says("done")]).await;

    let mut asked = false;
    turn(
        project.provider(&mock),
        project.asks("create marker.txt"),
        TurnOptions { yolo: auto(), ..project.options() },
        |event| {
            if let AgentEvent::Approval(request) = event {
                assert_eq!(request.tool, "run_command");
                assert!(request.reason.contains("could not decide"), "{}", request.reason);
                // The operation travels apart from the reason, as it would be run.
                assert_eq!(request.details.trim(), "$ touch marker.txt");
                request.respond.send(ApprovalDecision::Once).unwrap();
                asked = true;
            }
        },
    )
    .await;
    mock.finish().await;

    assert!(asked);
    assert!(project.workspace.join("marker.txt").exists(), "the person allowed it");
}

/// YOLO is no review at all: the command runs, and the mock's two requests
/// are the turn's own.
#[tokio::test]
async fn yolo_runs_everything_without_review() {
    let project = project(&[]);
    let mock = Mock::script([touch_marker(), says("done")]).await;

    turn(project.provider(&mock), project.asks("create marker.txt"), project.options(), |event| {
        if let AgentEvent::Approval(_) = event {
            panic!("YOLO never asks");
        }
    })
    .await;
    let requests = mock.finish().await;

    assert!(project.workspace.join("marker.txt").exists());
    assert_eq!(requests.len(), 2, "no review request in between");
}

/// A scout is read-only mechanically: a command that changes something is
/// refused even in YOLO, after the classifier calls it a mutation.
#[tokio::test]
async fn a_read_only_task_refuses_a_change() {
    let project = project(&[]);
    let mock = Mock::script([touch_marker(), says("MUTATE"), says("done")]).await;

    let mut output = String::new();
    turn(
        project.provider(&mock),
        project.asks("look around"),
        TurnOptions { read_only: true, ..project.options() },
        |event| {
            if let AgentEvent::ToolFinished { output: text, .. } = event {
                output = text;
            }
        },
    )
    .await;
    mock.finish().await;

    assert!(output.contains("this task is read-only"), "{output}");
    assert!(!project.workspace.join("marker.txt").exists());
}

/// What kind of background request the agent is making. A summariser or
/// reflector that is the conversation's own model asks from inside the live
/// context; one on another model builds a detached prompt. Both are recognised.
#[derive(PartialEq)]
enum Asked {
    Summary,
    ReviewGate,
    RefinePlan,
    Turn,
}

const IN_CONTEXT_SUMMARY: &str = "Your context is full";

fn asked(request: &str) -> Asked {
    let any = |phrases: &[&str]| phrases.iter().any(|phrase| request.contains(phrase));
    if any(&[IN_CONTEXT_SUMMARY, "context-aware state summary"]) {
        Asked::Summary
    } else if any(&["refinement review gate", "Looking back at the turn you have just finished"]) {
        Asked::ReviewGate
    } else if any(&["continual-harness refiner", "to your own reusable state"]) {
        Asked::RefinePlan
    } else {
        Asked::Turn
    }
}

/// A conversation over the compaction threshold (400k chars), carried by a
/// large assistant message that microcompaction cannot shrink away — which
/// forces the rolling-summary path.
fn oversized(project: &support::Project) -> Vec<serde_json::Value> {
    let mut messages = project.asks("please do the thing");
    messages.push(json!({"role":"assistant","content":format!("BIGBLOB{}", "x".repeat(420_000))}));
    messages.push(json!({"role":"user","content":"continue"}));
    messages
}

#[tokio::test]
async fn rolling_summary_compaction_fires_on_large_context() {
    let project = project(&[]);
    let mock = Mock::answering(|request| match asked(request) {
        Asked::Summary => Reply::Send(says(
            "1. Primary Request and Intent: do the thing. \
             9. Required Files:\n- src/main.rs\n10. Next Step: continue.",
        )),
        // The review gate runs before summary compaction; refuse it so no
        // planning call fires and the flow continues.
        Asked::ReviewGate => Reply::Send(says(r#"{"should_refine": false, "rationale": "nothing to keep"}"#)),
        _ => Reply::Last(says("all done")),
    })
    .await;

    let mut messages = oversized(&project);
    messages.push(json!({"role":"assistant","content":"working on it"}));
    let (completed, _) =
        turn(project.provider(&mock), messages, TurnOptions { allow_subagents: true, ..project.options() }, |_| {})
            .await;
    let requests = mock.finish().await;

    let summary = requests.iter().find(|request| asked(request) == Asked::Summary);
    let summary = summary.expect("compaction summarization call was not made");
    // With no compaction model assigned the summariser is the conversation's
    // own model, so the summary is asked for inside the live context — the
    // request whose prefix the provider already has cached.
    assert!(
        summary.contains(IN_CONTEXT_SUMMARY) && summary.contains("BIGBLOB"),
        "summary was not requested in-context"
    );
    let contents = contents(&completed);
    // The LLM path was taken, not the drop-only fallback and its system note.
    assert!(
        !contents.iter().any(|content| content.contains("were omitted")),
        "fallback drop-only path was used instead of LLM summarization"
    );
    assert!(!contents.iter().any(|content| content.contains("BIGBLOB")), "compacted middle was not dropped");
    assert!(contents.contains(&"working on it"), "recent tail was not preserved");
    assert_eq!(completed.last().unwrap()["content"], "all done");
}

/// The tether's intent snapshot used to run *after* the answer, so every first
/// turn ended with a two-second stall. It now runs beside the turn: this proves
/// the intent request reaches the server while the main stream is still open,
/// and that the snapshot still lands on the tether by the time the turn is done.
#[tokio::test]
async fn intent_snapshot_runs_beside_the_turn_not_after_it() {
    let project = project(&[]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    // Milliseconds since the server started at which the intent request
    // arrived, and at which the main answer finished streaming.
    let intent_arrived: Arc<Mutex<Option<u128>>> = Arc::default();
    let answer_finished: Arc<Mutex<Option<u128>>> = Arc::default();
    let (intent_probe, answer_probe) = (intent_arrived.clone(), answer_finished.clone());
    let server = tokio::spawn(async move {
        let start = std::time::Instant::now();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (intent_probe, answer_probe) = (intent_probe.clone(), answer_probe.clone());
            tokio::spawn(async move {
                if support::read_request(&mut stream).await.contains("INTENT") {
                    *intent_probe.lock().unwrap() = Some(start.elapsed().as_millis());
                    return support::respond(&mut stream, &says("Ship the importer fix.")).await;
                }
                // The main answer is served slowly, so an intent call that only
                // started afterwards could not possibly arrive before it ends.
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                support::respond(&mut stream, &says("all done")).await;
                *answer_probe.lock().unwrap() = Some(start.elapsed().as_millis());
            });
        }
    });

    let tether = abacus_agent::tether::TetherState::default();
    let mut tethered = false;
    turn(
        Provider::new(&project.config(address)).unwrap(),
        project.asks("fix the importer"),
        TurnOptions {
            // A session id is what makes the tether run at all.
            session_id: Some("session-under-test".into()),
            tether: tether.clone(),
            ..project.options()
        },
        |event| tethered |= matches!(event, AgentEvent::Notice(text) if text.starts_with("tethered")),
    )
    .await;
    server.await.unwrap();

    let intent_at = intent_arrived.lock().unwrap().expect("an intent call");
    let answer_at = answer_finished.lock().unwrap().expect("an answered turn");
    assert!(
        intent_at < answer_at,
        "the intent call must overlap the answer, not follow it \
         (intent at {intent_at}ms, answer finished at {answer_at}ms)"
    );
    assert_eq!(tether.intent().as_deref(), Some("Ship the importer fix."));
    assert!(tethered, "the user is told what the session is tethered to");
}

/// Refinement is two calls, not one: a cheap gate decides whether the turn
/// taught anything before the planning call runs. A "no" must cost exactly one
/// call — the old rethink pass planned unconditionally, and most long turns
/// have nothing worth keeping.
#[tokio::test]
async fn a_refusing_review_gate_skips_the_planning_call() {
    let project = project(&[]);
    let mock = Mock::answering(|request| match asked(request) {
        Asked::ReviewGate => Reply::Send(says(r#"{"should_refine": false, "rationale": "routine work"}"#)),
        Asked::RefinePlan => Reply::Send(says(r#"{"summary":"s","rationale":"r","expected_outcome":"e","edits":[]}"#)),
        Asked::Summary => Reply::Send(says("1. Primary Request and Intent: do the thing. 10. Next Step: continue.")),
        Asked::Turn => Reply::Last(says("all done")),
    })
    .await;

    let harness = abacus_agent::harness::HarnessStore::default();
    turn(
        project.provider(&mock),
        // Over the rolling-summary threshold, which is what runs the reflection.
        oversized(&project),
        TurnOptions { harness: harness.clone(), ..project.options() },
        |_| {},
    )
    .await;
    let requests = mock.finish().await;

    let count = |kind| requests.iter().filter(|request| asked(request) == kind).count();
    assert_eq!(count(Asked::ReviewGate), 1, "the gate is consulted once");
    assert_eq!(count(Asked::RefinePlan), 0, "a refused gate must not pay for the planning call");
    assert!(harness.snapshot().is_empty(), "nothing is written when the gate refuses");
}

/// The Grok example is a copy-paste starting point, so it is worth proving it
/// end to end rather than just parsing it: load the shipped file, point it at
/// a mock, and check what actually goes out on the wire.
#[tokio::test]
async fn shipped_grok_example_sends_a_bearer_key_to_an_openai_shaped_endpoint() {
    let project = project(&[]);
    let mock = Mock::script([says("hello from grok")]).await;

    // The shipped example, verbatim except for the host and a key source the
    // test can control — everything else (protocol, auth shape) is the file
    // as users copy it.
    #[derive(serde::Deserialize)]
    struct Example {
        providers: std::collections::BTreeMap<String, abacus_agent::providers::ProviderConfig>,
    }
    let shipped = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/providers/grok.example.toml"),
    )
    .unwrap();
    let mut grok = toml::from_str::<Example>(&shipped).unwrap().providers.remove("grok").expect("the grok provider");
    let key_file = project.directory.path().join("xai-key");
    std::fs::write(&key_file, "xai-test-key-123\n").unwrap();
    grok.base_url = format!("http://{}/v1", mock.address);
    grok.api_key_env = None;
    grok.token_file = Some(key_file);

    let mut config = project.config(mock.address);
    config.connection =
        abacus_agent::providers::Connection::resolve("grok", &grok, &abacus_agent::config::Credentials::default());
    config.model = "grok-4.5".to_owned();

    let opening = vec![json!({"role":"user","content":"hi"})];
    turn(Provider::new(&config).unwrap(), opening, project.options(), |_| {}).await;
    let request = mock.finish().await.remove(0);

    let (headers, body) = request.split_once("\r\n\r\n").expect("a request body");
    let lowered = headers.to_ascii_lowercase();
    assert!(headers.contains("POST /v1/chat/completions"), "chat completions under the base URL: {headers}");
    assert!(lowered.contains("authorization: bearer xai-test-key-123"), "the key goes out as a bearer: {headers}");
    // OpenAI-shaped, not Anthropic: messages at the top level, no anthropic-version.
    let body: serde_json::Value = serde_json::from_str(body).expect("json body");
    assert_eq!(body["model"], "grok-4.5");
    assert!(body["messages"].is_array(), "chat-completions shape: {body}");
    assert!(!lowered.contains("anthropic-version"), "no anthropic headers on an OpenAI endpoint: {headers}");
}

fn grep_command() -> String {
    calls(&[("c1", "run_command", json!({"command": "grep -rn needle ."}))])
}

/// A command that only looks never waits on the reviewer: the mock serves the
/// turn only, and a third request would mean a review happened.
#[tokio::test]
async fn auto_runs_inspection_without_a_review_call() {
    let project = project(&[("notes.txt", "the needle is here\n")]);
    let mock = Mock::script([grep_command(), says("found it")]).await;

    let mut output = String::new();
    turn(
        project.provider(&mock),
        project.asks("where is the needle?"),
        // Nothing here could answer a prompt, and nothing should need to.
        TurnOptions { yolo: auto(), ..project.options() },
        |event| {
            if let AgentEvent::ToolFinished { output: text, .. } = event {
                output.push_str(&text);
            }
        },
    )
    .await;
    let requests = mock.finish().await;

    assert!(!output.contains("Blocked"), "grep only inspects: {output}");
    assert!(!output.contains("rejected"), "inspection must not wait on an approval nobody can give: {output}");
    assert!(output.contains("needle"), "the command actually ran: {output}");
    assert_eq!(requests.len(), 2, "two turn requests and no review in between");
}

/// Reading outside the workspace used to be impossible, so models detoured
/// through an interpreter to reach a sibling checkout — extra latency to end
/// up in the same place. It is now allowed under the safety layer, while
/// credentials stay refused whatever anyone thinks.
#[tokio::test]
async fn an_outside_read_is_cleared_but_a_credential_is_not() {
    let project = project(&[]);
    // A sibling checkout, and a private key, both outside the workspace.
    let outside = |directory: &str, name: &str, content: &str| {
        let directory = project.directory.path().join(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(name), content).unwrap();
        directory.join(name).canonicalize().unwrap()
    };
    let sibling = outside("other/src", "lib.rs", "pub fn shared() {}\n");
    let key = outside(".ssh", "id_ed25519", "PRIVATE KEY MATERIAL\n");
    let mock = Mock::script([
        calls(&[("c1", "read_file", json!({"path": sibling}))]),
        calls(&[("c2", "read_file", json!({"path": key}))]),
        says("done"),
    ])
    .await;

    let mut outputs = Vec::new();
    turn(
        project.provider(&mock),
        project.asks("look at the sibling project"),
        TurnOptions { yolo: auto(), ..project.options() },
        |event| {
            if let AgentEvent::ToolFinished { output, .. } = event {
                outputs.push(output);
            }
        },
    )
    .await;
    mock.finish().await;

    assert_eq!(outputs.len(), 2, "both reads were attempted: {outputs:?}");
    assert!(outputs[0].contains("pub fn shared()"), "the sibling checkout is readable: {}", outputs[0]);
    assert!(!outputs[1].contains("PRIVATE KEY MATERIAL"), "the key must never be returned: {}", outputs[1]);
    assert!(outputs[1].contains("private data"), "and it says why: {}", outputs[1]);
}

/// A tool result too large to be worth reading whole is replaced by a handle
/// the model can interrogate — checked through the real loop rather than
/// against the store in isolation.
#[tokio::test]
async fn an_oversized_tool_result_is_bound_instead_of_flooding_the_context() {
    // read_file returns at most 400 lines by default, so the lines have to be
    // long enough that a bounded read still clears the bind threshold.
    let huge: String = (0..4_000).map(|index| format!("line {index}: {}\n", "filler ".repeat(40))).collect();
    let project = project(&[("big.log", &huge)]);
    let mock = Mock::script([calls(&[("t1", "read_file", json!({"path": "big.log"}))]), says("done")]).await;

    let handles = abacus_agent::handles::HandleStore::default();
    let (completed, _) = turn(
        project.provider(&mock),
        project.asks("read big.log"),
        TurnOptions { handles: handles.clone(), ..project.options() },
        |_| {},
    )
    .await;
    mock.finish().await;

    let tool_result = completed
        .iter()
        .find(|message| message["role"] == "tool")
        .and_then(|message| message["content"].as_str())
        .expect("a tool result was recorded");
    assert!(tool_result.contains("[bound to $h1"), "{tool_result}");
    assert!(tool_result.contains("read_file"), "the source is named");
    // The payload itself never entered the conversation.
    assert!(!tool_result.contains("filler filler"), "{tool_result}");
    assert!(tool_result.len() < 1_000, "the stand-in is small: {}", tool_result.len());
    // And it is still reachable in full.
    let bound = handles.get("h1").expect("content is retained");
    assert!(bound.content.contains("filler filler"));
    assert!(bound.chars() > 20_000, "the full payload is kept");
}

/// LocalAI renders a strict chat template (Qwen 3.x) and reports its "System
/// message must be at the beginning" as an error chunk on a 200 stream, not as
/// a failed request. The rejection must still be learned: the retry carries
/// one leading system message, and the session keeps doing so.
#[tokio::test]
async fn a_streamed_leading_system_rejection_is_learned_and_retried() {
    let project = project(&[]);
    let rejection = "data: {\"error\":{\"code\":\"server_error\",\"message\":\"rpc error: Jinja Exception: \
                     System message must be at the beginning.\",\"type\":\"server_error\"}}\n\ndata: [DONE]\n\n";
    let mock = Mock::script([rejection.to_owned(), says("Hello from Qwen.")]).await;

    // The shape Abacus sends: the base prompt first, and the volatile context
    // (summary, goal, mode) as a trailing system block.
    let messages = vec![
        json!({"role":"system","content":"You are Abacus."}),
        json!({"role":"user","content":"Hi"}),
        json!({"role":"system","content":"AUTO mode: risky actions are reviewed."}),
    ];
    let completion = project.provider(&mock).ask(&messages, &[], &AtomicBool::new(false)).await.unwrap();
    assert_eq!(completion.content, "Hello from Qwen.");

    let roles = |request: &str| -> Vec<String> {
        let body = &request[request.find("\r\n\r\n").map_or(0, |at| at + 4)..];
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        sent["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap().to_owned()).collect()
    };
    let requests = mock.finish().await;
    assert_eq!(roles(&requests[0]), ["system", "user", "system"], "the first request is sent as built");
    assert_eq!(roles(&requests[1]), ["system", "user"], "the retry merges the system blocks at the front");
}
