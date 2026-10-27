//! Workers: the subagents a session has started, while they run and after.
//!
//! The **board** is shared state the subagent runtime writes and the front
//! ends read: every worker's name, state, and latest activity, pinned above
//! the composer while they run and expandable into a detail overlay. The
//! **registry** reaches a worker by name, which is how messages travel: from
//! the agent that started it, from the other workers, and back to `main`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// Individual worker lines shown when at most this many run at once; more
/// cluster into a single summary line.
pub const CLUSTER_THRESHOLD: usize = 3;

/// What a subagent calls the agent that started it.
pub const MAIN: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    Running,
    Done,
    Failed,
    /// Stopped by the person before it finished.
    Stopped,
}

impl WorkerState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerStatus {
    pub id: u64,
    pub name: String,
    /// The worker's role: `general` or `explore`, or `resumed` for one
    /// continued by message.
    pub role: String,
    pub state: WorkerState,
    /// The worker's most recent visible activity — a tool call or the tail of
    /// its streamed answer — clipped to one line.
    pub activity: String,
    pub started: Instant,
    /// Wall-clock start and end, for a front end that shows elapsed time.
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Live token ledger for this worker's own provider clone.
    pub tokens: Arc<crate::provider::TokenLedger>,
    /// The model the worker runs on, and what it was asked to do.
    pub model: String,
    pub prompt: String,
    /// The tool running right now, if the worker is in a tool call.
    pub tool: Option<String>,
    pub tool_uses: u32,
    /// Raised to stop the worker; its turn winds down at the next check.
    pub cancel: Arc<AtomicBool>,
    /// What the worker has said and done so far, readable while it runs.
    pub log: Arc<Mutex<WorkerLog>>,
}

impl WorkerStatus {
    pub fn tokens_used(&self) -> u64 {
        self.tokens.total()
    }
}

/// Finished workers kept after their batch is replaced, for a front end that
/// lists past work. The TUI strip only ever shows the current batch.
const HISTORY: usize = 50;

#[derive(Default)]
struct BoardInner {
    workers: Vec<WorkerStatus>,
    history: VecDeque<WorkerStatus>,
    next_id: u64,
    /// Bumped on every visible mutation so the UI can redraw the strip when
    /// workers move even though no turn is running (nothing else marks the
    /// frame dirty while the transcript is idle).
    version: u64,
}

/// Live worker state, written by the subagent runtime and read by the UI.
#[derive(Clone, Default)]
pub struct WorkerBoard {
    inner: Arc<RwLock<BoardInner>>,
}

impl WorkerBoard {
    /// Register a worker; a new batch starting while only finished workers
    /// remain replaces them, so the strip always shows the current batch.
    pub fn begin(&self, name: &str, role: &str, tokens: Arc<crate::provider::TokenLedger>) -> u64 {
        let mut inner = self.inner.write().expect("board lock");
        if inner.workers.iter().all(|worker| worker.state != WorkerState::Running) {
            let settled = std::mem::take(&mut inner.workers);
            for worker in settled {
                inner.history.push_front(worker);
            }
            inner.history.truncate(HISTORY);
        }
        inner.next_id += 1;
        inner.version = inner.version.wrapping_add(1);
        let id = inner.next_id;
        inner.workers.push(WorkerStatus {
            id,
            name: name.to_owned(),
            role: role.to_owned(),
            state: WorkerState::Running,
            activity: "starting".to_owned(),
            started: Instant::now(),
            started_at: Utc::now(),
            finished_at: None,
            tokens,
            model: String::new(),
            prompt: String::new(),
            tool: None,
            tool_uses: 0,
            cancel: Arc::default(),
            log: Arc::default(),
        });
        id
    }

    /// Name the model a worker runs on and the task it was given; its
    /// transcript opens with the task.
    pub fn describe(&self, id: u64, model: &str, prompt: &str) {
        let mut inner = self.inner.write().expect("board lock");
        if let Some(worker) = inner.workers.iter_mut().find(|worker| worker.id == id) {
            worker.model = model.to_owned();
            worker.prompt = prompt.to_owned();
            worker.log.lock().expect("worker log lock").prompt(prompt);
            inner.version = inner.version.wrapping_add(1);
        }
    }

    /// Fold one of a worker's events into the board: its activity line, the
    /// tool it is in, and its transcript.
    pub fn record(&self, id: u64, event: &crate::agent::AgentEvent) {
        use crate::agent::AgentEvent;
        let log = {
            let mut inner = self.inner.write().expect("board lock");
            let Some(worker) = inner.workers.iter_mut().find(|worker| worker.id == id) else {
                return;
            };
            match event {
                AgentEvent::ToolStarted { name, .. } => {
                    worker.tool = Some(name.clone());
                    worker.tool_uses += 1;
                    inner.version = inner.version.wrapping_add(1);
                }
                AgentEvent::ToolFinished { .. } => {
                    worker.tool = None;
                    inner.version = inner.version.wrapping_add(1);
                }
                _ => {}
            }
            inner.workers.iter().find(|worker| worker.id == id).map(|worker| worker.log.clone())
        };
        match event {
            AgentEvent::ToolStarted { name, summary } => self.activity(id, &format!("{name} {summary}")),
            AgentEvent::Delta(text) => self.activity(id, text),
            _ => {}
        }
        if let Some(log) = log {
            log.lock().expect("worker log lock").event(event);
        }
    }

    /// Ask a running worker to stop. False when there is no such worker or it
    /// already finished.
    pub fn stop(&self, id: u64) -> bool {
        let inner = self.inner.read().expect("board lock");
        match inner.workers.iter().find(|worker| worker.id == id && worker.state == WorkerState::Running) {
            Some(worker) => {
                worker.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// The flag a worker's turn watches; raised by [`Self::stop`].
    pub fn cancel_flag(&self, id: u64) -> Arc<AtomicBool> {
        let inner = self.inner.read().expect("board lock");
        inner.workers.iter().find(|worker| worker.id == id).map(|worker| worker.cancel.clone()).unwrap_or_default()
    }

    /// Whether the person stopped this worker.
    pub fn was_stopped(&self, id: u64) -> bool {
        self.cancel_flag(id).load(Ordering::Relaxed)
    }

    /// Every worker this session has kept: the current batch, then earlier
    /// finished ones, newest first.
    pub fn all(&self) -> Vec<WorkerStatus> {
        let inner = self.inner.read().expect("board lock");
        let mut workers: Vec<WorkerStatus> = inner.workers.iter().rev().cloned().collect();
        workers.extend(inner.history.iter().cloned());
        workers
    }

    /// A worker's transcript as client items, current or past.
    pub fn transcript(&self, id: u64) -> Option<Vec<Value>> {
        let inner = self.inner.read().expect("board lock");
        let worker = inner.workers.iter().chain(inner.history.iter()).find(|worker| worker.id == id)?;
        Some(worker.log.lock().expect("worker log lock").items())
    }

    /// Forget finished workers; running ones stay.
    pub fn clear_finished(&self) {
        let mut inner = self.inner.write().expect("board lock");
        inner.history.clear();
        inner.workers.retain(|worker| worker.state == WorkerState::Running);
        inner.version = inner.version.wrapping_add(1);
    }

    pub fn activity(&self, id: u64, activity: &str) {
        let mut inner = self.inner.write().expect("board lock");
        if let Some(worker) = inner.workers.iter_mut().find(|worker| worker.id == id) {
            // Trailing punctuation fragments of a streamed answer ("…", ")")
            // are not activity; require something readable.
            let line = activity
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| line.chars().filter(|ch| ch.is_alphanumeric()).count() >= 3)
                .unwrap_or("")
                .to_owned();
            if !line.is_empty() {
                worker.activity = line;
                inner.version = inner.version.wrapping_add(1);
            }
        }
    }

    pub fn finish(&self, id: u64, ok: bool) {
        let mut inner = self.inner.write().expect("board lock");
        if let Some(worker) = inner.workers.iter_mut().find(|worker| worker.id == id) {
            worker.state = if worker.cancel.load(Ordering::Relaxed) {
                WorkerState::Stopped
            } else if ok {
                WorkerState::Done
            } else {
                WorkerState::Failed
            };
            worker.finished_at = Some(Utc::now());
            worker.tool = None;
            // A tool cut off by the end of the worker never reports back.
            worker.log.lock().expect("worker log lock").settle();
            inner.version = inner.version.wrapping_add(1);
        }
    }

    /// Counter of visible board changes; a larger value than the last one the
    /// UI saw means the worker strip should be redrawn.
    pub fn version(&self) -> u64 {
        self.inner.read().expect("board lock").version
    }

    pub fn snapshot(&self) -> Vec<WorkerStatus> {
        self.inner.read().expect("board lock").workers.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().expect("board lock").workers.is_empty()
    }

    /// Rows the pinned strip needs right now: one per worker up to the
    /// cluster threshold, one summary row past it, zero when idle.
    pub fn strip_rows(&self) -> u16 {
        let count = self.inner.read().expect("board lock").workers.len();
        match count {
            0 => 0,
            n if n <= CLUSTER_THRESHOLD => n as u16,
            _ => 1,
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.write().expect("board lock");
        if !inner.workers.is_empty() {
            inner.version = inner.version.wrapping_add(1);
        }
        inner.workers.clear();
    }
}

/// Items a worker's transcript keeps (the task stays first); output kept per
/// tool call. Bounded because a long worker can run hundreds of steps.
const LOG_ITEMS: usize = 400;
const LOG_TOOL_OUTPUT: usize = 4_000;

/// A worker's transcript in the app server's item shape, built from its
/// events so a front end can read it while the worker runs.
#[derive(Debug, Default)]
pub struct WorkerLog {
    items: Vec<Value>,
    next: u64,
    /// The text and reasoning items still being streamed into, by id.
    open_text: Option<String>,
    open_reasoning: Option<String>,
}

impl WorkerLog {
    fn push(&mut self, mut item: Value) -> String {
        self.next += 1;
        let id = format!("worker_item_{}", self.next);
        item["id"] = json!(id);
        self.items.push(item);
        if self.items.len() > LOG_ITEMS {
            // Drop the oldest step, never the task.
            self.items.remove(1);
        }
        id
    }

    fn find(&mut self, id: &str) -> Option<&mut Value> {
        self.items.iter_mut().rev().find(|item| item["id"] == id)
    }

    fn stream(&mut self, kind: &str, piece: &str) {
        let open = if kind == "agentMessage" { self.open_text.clone() } else { self.open_reasoning.clone() };
        if let Some(id) = open
            && let Some(item) = self.find(&id)
        {
            let text = format!("{}{piece}", item["text"].as_str().unwrap_or_default());
            item["text"] = json!(text);
            return;
        }
        let id = self.push(json!({"type": kind, "text": piece}));
        if kind == "agentMessage" {
            self.open_text = Some(id);
        } else {
            self.open_reasoning = Some(id);
        }
    }

    pub fn prompt(&mut self, text: &str) {
        self.push(json!({"type": "userMessage", "text": text}));
    }

    pub fn event(&mut self, event: &crate::agent::AgentEvent) {
        use crate::agent::AgentEvent;
        match event {
            AgentEvent::Delta(text) => self.stream("agentMessage", text),
            AgentEvent::Reasoning(text) => self.stream("reasoning", text),
            AgentEvent::ToolStarted { name, summary } => {
                // A tool call ends whatever was streaming.
                self.open_text = None;
                self.open_reasoning = None;
                self.push(json!({
                    "type": crate::app_server::item_type(name),
                    "name": name,
                    "command": summary,
                    "status": "inProgress",
                }));
            }
            AgentEvent::ToolFinished { name, output, refused } => {
                if let Some(item) =
                    self.items.iter_mut().rev().find(|item| item["name"] == *name && item["status"] == "inProgress")
                {
                    item["status"] = json!(if *refused { "declined" } else { "completed" });
                    item["output"] = json!(crate::text::clip(output, LOG_TOOL_OUTPUT, "\n…"));
                }
            }
            // A message from a sibling or `main` lands where the worker read
            // it, like the main thread's.
            AgentEvent::AgentMessage { from, text } => {
                self.open_text = None;
                self.open_reasoning = None;
                self.push(crate::app_server::agent_item(from, text));
            }
            AgentEvent::MessageSent { text, .. } => {
                if let Some(item) = self
                    .items
                    .iter_mut()
                    .rev()
                    .find(|item| item["name"] == "send_message" && item["status"] == "inProgress")
                {
                    item["message"] = json!(text);
                }
            }
            _ => {}
        }
    }

    pub fn items(&self) -> Vec<Value> {
        self.items.clone()
    }

    /// Mark tool calls that never finished as aborted.
    fn settle(&mut self) {
        for item in &mut self.items {
            if item["status"] == "inProgress" {
                item["status"] = json!("aborted");
            }
        }
        self.open_text = None;
        self.open_reasoning = None;
    }
}

/// How many finished workers stay resumable. Transcripts are held in memory,
/// so the window is bounded; older workers fall out oldest-first.
const RESUMABLE_WORKERS: usize = 8;

/// What a message to a worker reaches.
pub enum WorkerChannel {
    /// The worker is running, or waiting for its turn to start: a message
    /// reaches it after its current step.
    Live(crate::agent::InjectionQueue),
    /// The worker finished. Its conversation is kept so a follow-up can
    /// continue with context intact, in a fresh worktree.
    Finished(Vec<serde_json::Value>),
}

struct WorkerEntry {
    running: bool,
    injections: crate::agent::InjectionQueue,
    transcript: Vec<serde_json::Value>,
    /// Monotonic sequence for oldest-first eviction.
    seq: u64,
}

/// Workers by name, for the agent that started them and for each other. It
/// lives on the [`Workers`] handle, so it survives across turns: a worker
/// started in one turn is still reachable from the next.
#[derive(Clone, Default)]
pub struct WorkerRegistry {
    inner: Arc<RwLock<std::collections::BTreeMap<String, WorkerEntry>>>,
    next_seq: Arc<AtomicU64>,
}

impl WorkerRegistry {
    /// Register a starting worker and hand back the queue its turn drains.
    /// A repeated name replaces the older entry: the live worker is the one a
    /// sender means.
    pub fn open(&self, name: &str) -> crate::agent::InjectionQueue {
        let injections = crate::agent::InjectionQueue::default();
        let mut inner = self.inner.write().expect("worker registry lock");
        inner.insert(
            name.to_owned(),
            WorkerEntry {
                running: true,
                injections: injections.clone(),
                transcript: Vec::new(),
                seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
            },
        );
        injections
    }

    /// Mark a worker finished and keep its conversation for a follow-up.
    pub fn close(&self, name: &str, transcript: Vec<serde_json::Value>) {
        let mut inner = self.inner.write().expect("worker registry lock");
        if let Some(entry) = inner.get_mut(name) {
            entry.running = false;
            entry.transcript = transcript;
        }
        // Bound the retained set, dropping the oldest finished workers first.
        while inner.values().filter(|entry| !entry.running).count() > RESUMABLE_WORKERS {
            let Some(oldest) = inner
                .iter()
                .filter(|(_, entry)| !entry.running)
                .min_by_key(|(_, entry)| entry.seq)
                .map(|(name, _)| name.clone())
            else {
                break;
            };
            inner.remove(&oldest);
        }
    }

    /// Reach a worker by name, if it is known.
    pub fn channel(&self, name: &str) -> Option<WorkerChannel> {
        let inner = self.inner.read().expect("worker registry lock");
        let entry = inner.get(name)?;
        Some(if entry.running {
            WorkerChannel::Live(entry.injections.clone())
        } else {
            WorkerChannel::Finished(entry.transcript.clone())
        })
    }

    /// Known worker names with their state, for error messages and listings.
    pub fn roster(&self) -> Vec<(String, bool)> {
        let inner = self.inner.read().expect("worker registry lock");
        inner.iter().map(|(name, entry)| (name.clone(), entry.running)).collect()
    }

    /// Why a message to `name` reaches nobody, with the names it could have
    /// meant.
    pub fn unknown(&self, name: &str) -> String {
        let roster = self.roster();
        if roster.is_empty() {
            return format!("no worker named `{name}` — none have been started yet");
        }
        let known = roster
            .iter()
            .map(|(worker, running)| format!("{worker} ({})", if *running { "running" } else { "finished" }))
            .collect::<Vec<_>>()
            .join(", ");
        format!("no worker named `{name}`. Known workers: {known}")
    }
}

/// The two halves of a `send_message` call: who it is for, and what it says.
pub fn parse_message(arguments: &str) -> Result<(String, String)> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Arguments {
        to: String,
        message: String,
    }
    let arguments: Arguments = serde_json::from_str(arguments).context("invalid send_message arguments")?;
    if arguments.message.trim().is_empty() {
        bail!("message must not be empty");
    }
    Ok((arguments.to.trim().to_owned(), arguments.message))
}

/// How a subagent sends: as itself, to the other workers and to `main`.
#[derive(Clone)]
pub struct Mailbox {
    /// The worker's own name, which its messages are signed with.
    pub name: String,
    pub registry: WorkerRegistry,
    /// The queue of the agent that started it, which `main` reaches.
    pub parent: crate::agent::InjectionQueue,
}

impl Mailbox {
    /// Deliver a `send_message` call: to `main`, or to another worker that is
    /// still running. Nothing waits for an answer; one arrives as a message.
    pub fn send(&self, arguments: &str) -> Result<String> {
        let (to, text) = parse_message(arguments)?;
        let message = crate::agent::Injection::AgentMessage { from: self.name.clone(), text };
        if to == MAIN {
            self.parent.push(message);
            return Ok(format!(
                "Delivered to `{MAIN}`, the agent that started you; it reads it after its current step."
            ));
        }
        if to == self.name {
            bail!("`{to}` is you; message another worker, or `{MAIN}`");
        }
        match self.registry.channel(&to) {
            Some(WorkerChannel::Live(queue)) => {
                queue.push(message);
                Ok(format!("Delivered to `{to}`; it reads it after its current step."))
            }
            Some(WorkerChannel::Finished(_)) => {
                bail!("`{to}` has finished, and its report went to `{MAIN}`; message `{MAIN}` instead")
            }
            None => bail!("{}", self.registry.unknown(&to)),
        }
    }
}

/// Shared handle on the live board and the worker registry, riding
/// `TurnOptions`. A front end keeps one for the process, so background workers
/// stay visible across turns.
#[derive(Clone, Default)]
pub struct Workers {
    pub board: WorkerBoard,
    pub registry: WorkerRegistry,
}

impl Workers {
    /// The workers still running, as a context layer: empty when none are.
    ///
    /// Spawning does not block: the call returns "started in background" and
    /// results arrive after a later tool call. So a model that is not told
    /// what it already launched will launch the same work again — three
    /// spawns of three became nine workers for one task. Naming them is what
    /// closes that loop.
    pub fn in_flight(&self) -> String {
        let running: Vec<String> = self
            .board
            .snapshot()
            .into_iter()
            .filter(|worker| worker.state == WorkerState::Running)
            // Name and role only. The live activity line changes on every tool
            // call a worker makes, and this is conversation context: a text
            // that changes that often is re-sent on every step.
            .map(|worker| format!("{} ({})", worker.name, worker.role))
            .collect();
        if running.is_empty() {
            return String::new();
        }
        format!(
            "ALREADY RUNNING — {} worker(s) you have already started, still working: {}. Their results reach \
             you after your next tool call. Do NOT spawn more workers for this same work, and do not redo it \
             yourself; carry on with something else or wait for them.",
            running.len(),
            running.join("; ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawning does not block, so without this the model cannot tell that it
    /// already started the work it is about to start again.
    #[test]
    fn the_workers_already_in_flight_are_named() {
        let workers = Workers::default();
        assert_eq!(workers.in_flight(), "", "nothing running, nothing to say");

        let tokens = Arc::new(crate::provider::TokenLedger::default());
        let id = workers.board.begin("recon", "explore", tokens.clone());
        workers.board.activity(id, "reading src/agent.rs");
        workers.board.begin("builder", "general", tokens);

        let layer = workers.in_flight();
        assert!(layer.contains("ALREADY RUNNING"), "{layer}");
        assert!(layer.contains("recon"), "names them: {layer}");
        assert!(layer.contains("builder"), "{layer}");
        assert!(layer.contains("explore"), "with their roles");
        assert!(layer.contains("Do NOT spawn more workers"), "and says what to do about it: {layer}");
    }

    #[test]
    fn a_worker_messages_a_running_peer_and_main_but_not_one_that_finished() {
        let registry = WorkerRegistry::default();
        let parent = crate::agent::InjectionQueue::default();
        let mailbox =
            |name: &str| Mailbox { name: name.to_owned(), registry: registry.clone(), parent: parent.clone() };
        let recon = registry.open("recon");
        registry.open("builder");

        let sent = mailbox("builder").send(r#"{"to":"recon","message":"the schema lives in db/"}"#).unwrap();
        assert!(sent.starts_with("Delivered to `recon`"), "{sent}");
        let delivered = recon.drain();
        assert!(
            matches!(delivered.as_slice(), [crate::agent::Injection::AgentMessage { from, text }]
                if from == "builder" && text == "the schema lives in db/"),
            "signed by its sender: {delivered:?}"
        );

        mailbox("builder").send(r#"{"to":"main","message":"done with db/"}"#).unwrap();
        assert!(
            matches!(parent.drain().as_slice(), [crate::agent::Injection::AgentMessage { from, .. }] if from == "builder")
        );

        registry.close("recon", Vec::new());
        let finished = mailbox("builder").send(r#"{"to":"recon","message":"one more"}"#).unwrap_err().to_string();
        assert!(finished.contains("has finished"), "{finished}");
        let unknown = mailbox("builder").send(r#"{"to":"ghost","message":"hello"}"#).unwrap_err().to_string();
        assert!(unknown.contains("recon (finished)") && unknown.contains("builder (running)"), "{unknown}");
        assert!(mailbox("builder").send(r#"{"to":"builder","message":"hi"}"#).is_err(), "not to itself");
    }

    #[test]
    fn registry_routes_to_a_live_worker_then_to_its_transcript() {
        let registry = WorkerRegistry::default();
        assert!(registry.channel("nobody").is_none());

        let queue = registry.open("recon");
        // Running: a message steers the live worker.
        match registry.channel("recon") {
            Some(WorkerChannel::Live(live)) => {
                live.push(crate::agent::Injection::UserMessage("also check tests".into()));
            }
            other => panic!("expected a live channel, got {}", other.is_some()),
        }
        assert_eq!(queue.drain().len(), 1, "the worker's own turn receives it");

        // Finished: the channel switches to its conversation.
        registry.close("recon", vec![serde_json::json!({"role":"assistant","content":"found it"})]);
        match registry.channel("recon") {
            Some(WorkerChannel::Finished(transcript)) => {
                assert_eq!(transcript.len(), 1, "context is kept for a follow-up");
            }
            _ => panic!("expected a finished channel"),
        }
        assert_eq!(registry.roster(), vec![("recon".to_owned(), false)]);
    }

    #[test]
    fn registry_bounds_retained_transcripts_oldest_first() {
        let registry = WorkerRegistry::default();
        for index in 0..(RESUMABLE_WORKERS + 3) {
            let name = format!("w{index}");
            registry.open(&name);
            registry.close(&name, vec![serde_json::json!({"role":"assistant"})]);
        }
        let roster = registry.roster();
        assert_eq!(roster.len(), RESUMABLE_WORKERS, "retention is bounded");
        assert!(registry.channel("w0").is_none(), "oldest evicted first");
        assert!(registry.channel(&format!("w{}", RESUMABLE_WORKERS + 2)).is_some(), "newest retained");
    }

    #[test]
    fn a_running_worker_is_never_evicted() {
        let registry = WorkerRegistry::default();
        registry.open("long-runner");
        for index in 0..(RESUMABLE_WORKERS + 4) {
            let name = format!("w{index}");
            registry.open(&name);
            registry.close(&name, vec![serde_json::json!({"role":"assistant"})]);
        }
        assert!(
            matches!(registry.channel("long-runner"), Some(WorkerChannel::Live(_))),
            "eviction only takes finished workers"
        );
    }

    #[test]
    fn a_workers_transcript_and_counts_follow_its_events() {
        use crate::agent::AgentEvent;
        let board = WorkerBoard::default();
        let id = board.begin("recon", "explore", Arc::default());
        board.describe(id, "kimi-k3", "find the balancing loss");
        board.record(id, &AgentEvent::Reasoning("look at the router".into()));
        board.record(id, &AgentEvent::ToolStarted { name: "grep".into(), summary: "aux_loss in src".into() });
        assert_eq!(board.all()[0].tool.as_deref(), Some("grep"), "the tool it is in");
        board.record(
            id,
            &AgentEvent::ToolFinished { name: "grep".into(), output: "src/moe.rs:12".into(), refused: false },
        );
        board.record(id, &AgentEvent::Delta("Found it ".into()));
        board.record(id, &AgentEvent::Delta("in src/moe.rs.".into()));

        let worker = &board.all()[0];
        assert_eq!((worker.model.as_str(), worker.tool_uses, worker.tool.is_none()), ("kimi-k3", 1, true));
        let items = board.transcript(id).unwrap();
        let kinds: Vec<&str> = items.iter().map(|item| item["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["userMessage", "reasoning", "functionCall", "agentMessage"]);
        assert_eq!(items[0]["text"], "find the balancing loss");
        assert_eq!(items[2]["status"], "completed");
        assert_eq!(items[2]["output"], "src/moe.rs:12");
        assert_eq!(items[3]["text"], "Found it in src/moe.rs.", "streamed text joins into one item");
    }

    /// What a worker hears from the others shows where it read it, and what
    /// it sends stays on its send_message call.
    #[test]
    fn a_workers_transcript_keeps_its_messages() {
        use crate::agent::AgentEvent;
        let board = WorkerBoard::default();
        let id = board.begin("perf", "general", Arc::default());
        board.describe(id, "qwen3-coder", "make the report fast");
        board.record(id, &AgentEvent::Delta("Reading first.".into()));
        board.record(id, &AgentEvent::AgentMessage { from: "callers".into(), text: "keep the keys".into() });
        board.record(id, &AgentEvent::ToolStarted { name: "send_message".into(), summary: "main".into() });
        board.record(id, &AgentEvent::MessageSent { to: "main".into(), text: "one query now".into() });
        board.record(
            id,
            &AgentEvent::ToolFinished { name: "send_message".into(), output: "Sent.".into(), refused: false },
        );

        let items = board.transcript(id).unwrap();
        let kinds: Vec<&str> = items.iter().map(|item| item["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["userMessage", "agentMessage", "userMessage", "functionCall"]);
        assert_eq!(items[2]["text"], "keep the keys");
        assert_eq!(items[2]["from"], serde_json::json!({"title": "callers", "agent": true}));
        assert_eq!(items[3]["message"], "one query now");
        assert_eq!(items[3]["status"], "completed");
    }

    #[test]
    fn stopped_workers_say_so_and_finished_ones_outlive_their_batch() {
        let board = WorkerBoard::default();
        let first = board.begin("alpha", "worker", Arc::default());
        assert!(board.stop(first));
        assert!(board.cancel_flag(first).load(Ordering::Relaxed), "the worker's turn sees the stop");
        board.record(
            first,
            &crate::agent::AgentEvent::ToolStarted { name: "run_command".into(), summary: "sleep 45".into() },
        );
        board.finish(first, true);
        assert_eq!(board.all()[0].state, WorkerState::Stopped);
        assert_eq!(board.transcript(first).unwrap()[0]["status"], "aborted", "the cut-off command shows as such");
        assert!(board.all()[0].finished_at.is_some());
        assert!(!board.stop(first), "a finished worker cannot be stopped");

        // A new batch replaces the strip's workers, but the list keeps the old ones.
        let second = board.begin("beta", "worker", Arc::default());
        assert_eq!(board.snapshot().len(), 1);
        let names: Vec<String> = board.all().into_iter().map(|worker| worker.name).collect();
        assert_eq!(names, ["beta", "alpha"]);
        assert!(board.transcript(first).is_some(), "a past worker's transcript is still readable");

        board.clear_finished();
        let names: Vec<String> = board.all().into_iter().map(|worker| worker.name).collect();
        assert_eq!(names, ["beta"], "clearing keeps what still runs");
        board.finish(second, false);
        assert_eq!(board.all()[0].state, WorkerState::Failed);
    }

    #[test]
    fn board_tracks_workers_and_clusters_at_scale() {
        let board = WorkerBoard::default();
        assert_eq!(board.strip_rows(), 0);
        let a = board.begin("alpha", "explore", Arc::new(crate::provider::TokenLedger::default()));
        let b = board.begin("beta", "general", Arc::new(crate::provider::TokenLedger::default()));
        assert_eq!(board.strip_rows(), 2, "a small batch lists individually");
        board.activity(a, "running tests\npass 3 of 9");
        assert_eq!(board.snapshot()[0].activity, "pass 3 of 9");
        board.finish(a, true);
        board.finish(b, false);
        let states: Vec<WorkerState> = board.snapshot().iter().map(|w| w.state).collect();
        assert_eq!(states, vec![WorkerState::Done, WorkerState::Failed]);

        for index in 0..5 {
            board.begin(&format!("worker-{index}"), "worker", Arc::new(crate::provider::TokenLedger::default()));
        }
        // The new batch replaced the settled one, and five workers cluster.
        assert_eq!(board.snapshot().len(), 5);
        assert_eq!(board.strip_rows(), 1);
        board.clear();
        assert!(board.is_empty());
    }
}
