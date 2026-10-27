//! Other sessions, as the agent sees them: find them — this session's forks
//! and the one it came from first — see which are open and working, read what
//! was said there, and message their agents.
//!
//! Reach is this project by default. Another project's sessions take an
//! explicit `scope: "all"`, which the tools tell the model to use only when the
//! user asked to reach beyond this project; a message is an action with effects
//! outside the workspace, so AUTO's reviewer judges it like one.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::config::AbacusPaths;
use crate::presence::{Live, live_sessions};
use crate::schema::function;
use crate::session::{SessionStore, SessionSummary};

/// Rows `session_list` shows per project before saying how many it left out.
const LIST_LIMIT: usize = 25;
/// Messages `session_read` returns unless asked for a different number.
const READ_DEFAULT: usize = 20;
const READ_MAX: usize = 60;
/// Each message in a read is held to this many characters, and the whole read
/// to `READ_BUDGET`, newest kept: another session's transcript is context to
/// pick up from, not something to re-read in full.
const READ_MESSAGE_CHARS: usize = 1_500;
const READ_BUDGET: usize = 16_000;
/// Related sessions the context names at most; the nearest come first.
const KIN_LIMIT: usize = 12;

/// How the agent works beside its related sessions, said after their list.
const KIN_GUIDANCE: &str = "These share this project's files with you. While one of them is working, it may \
     be changing the same files: before you change files it may be changing, tell it with session_message what \
     you are about to change, and leave alone what it is changing — never overwrite another session's \
     uncommitted work. When you finish a piece of work, tell the session this one was forked from, and any \
     other it concerns, what you did and where it stands, with session_message; a closed one reads it when it \
     is next opened. Message for those two reasons only, not to report routine progress.";

/// The session tools for one conversation: where its sessions are kept, which
/// project it is in, and which session it is.
#[derive(Clone, Default)]
pub struct Peers {
    context: Option<Arc<Context>>,
}

struct Context {
    paths: AbacusPaths,
    workspace: PathBuf,
    /// None until the conversation is saved, which its first prompt does.
    session: Option<Uuid>,
    /// The related sessions as the context names them, worked out once: a
    /// turn builds its own `Peers`, and listing reads every session here.
    kin: std::sync::OnceLock<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Project,
    All,
}

/// A session the tools found, and the project it belongs to.
struct Found {
    summary: SessionSummary,
    workspace: PathBuf,
}

impl Peers {
    pub fn new(paths: AbacusPaths, workspace: PathBuf, session: Option<Uuid>) -> Self {
        Self { context: Some(Arc::new(Context { paths, workspace, session, kin: Default::default() })) }
    }

    /// The sessions related to this one in its project — the one it was
    /// forked from, its forks and theirs, and those forked from the same
    /// session — as a context layer: who they are, whether they are open or
    /// working, and how to work beside them. Empty when there are none.
    pub fn kin_layer(&self) -> String {
        self.context
            .as_ref()
            .map(|context| context.kin.get_or_init(|| context.describe_kin()).clone())
            .unwrap_or_default()
    }

    /// Whether a `session_message` call goes to a session related to this
    /// one, in this project. That is the coordination the kin layer asks for,
    /// so AUTO sends it without a review: it stays on this machine, and the
    /// session receiving it judges what it does about it.
    pub fn messages_kin(&self, arguments: &str) -> bool {
        let Some(context) = &self.context else { return false };
        let arguments = serde_json::from_str::<Value>(arguments).unwrap_or(Value::Null);
        let Some(reference) = arguments["session"].as_str() else { return false };
        let Ok(found) = context.find(reference, Scope::Project) else { return false };
        let sessions = context.store(&context.workspace).list().unwrap_or_default();
        Family::of(context.session, &sessions).relation(found.summary.id).is_kin()
    }

    /// Whether this conversation can reach other sessions: it has a session
    /// store to look in. The tools are offered only then.
    pub fn is_available(&self) -> bool {
        self.context.is_some()
    }

    pub fn tool_specs() -> Vec<Value> {
        let scope = json!({
            "type": "string",
            "enum": ["project", "all"],
            "description": "\"project\" (default): this project's sessions. \"all\": every project's — only when the user asked you to reach beyond this project."
        });
        let session = json!({"type": "string", "description": "The session's id, or a unique prefix of it as session_list shows it"});
        vec![
            function(
                "session_list",
                "List other abacus sessions: this session's forks and the session it was forked from first, then \
                 the rest of this project's, newest first. Each row says whether that session is open right now — \
                 working on a turn, or idle — and where.",
                json!({
                    "type": "object",
                    "properties": {
                        "scope": scope,
                        "query": {"type": "string", "description": "Only sessions whose title contains this, ignoring case"}
                    }
                }),
            ),
            function(
                "session_read",
                "Read another session's recent conversation: what the user asked there and what its agent \
                 answered, without tool output. Use it to pick up what a fork found, or before messaging one.",
                json!({
                    "type": "object",
                    "properties": {
                        "session": session,
                        "messages": {"type": "integer", "minimum": 1, "maximum": READ_MAX, "description": "How many of the latest messages (default 20)"},
                        "scope": scope
                    },
                    "required": ["session"]
                }),
            ),
            function(
                "session_message",
                "Send a message to another session's agent. If that session is open and idle it starts a turn \
                 on your message; if it is working, it reads the message at its next step; if nobody has it open, \
                 it waits until someone does. The receiver sees your session's title, not this conversation, so \
                 say what you need and why. This reaches an agent, not a person — to tell the user something, \
                 say it here.",
                json!({
                    "type": "object",
                    "properties": {
                        "session": session,
                        "message": {"type": "string"},
                        "scope": scope
                    },
                    "required": ["session", "message"]
                }),
            ),
        ]
    }

    /// Answer a session tool call, or None for any other tool.
    pub fn execute(&self, name: &str, arguments: &str) -> Option<String> {
        if !matches!(name, "session_list" | "session_read" | "session_message") {
            return None;
        }
        let Some(context) = &self.context else {
            return Some("Error: sessions are off in this run, so there are no others to reach.".to_owned());
        };
        let arguments = serde_json::from_str::<Value>(arguments).unwrap_or(Value::Null);
        let scope = if arguments["scope"] == "all" { Scope::All } else { Scope::Project };
        let result = match name {
            "session_list" => Ok(context.list(scope, arguments["query"].as_str().unwrap_or_default())),
            "session_read" => {
                let count = arguments["messages"].as_u64().map_or(READ_DEFAULT, |count| count as usize);
                context.read(arguments["session"].as_str().unwrap_or_default(), count.clamp(1, READ_MAX), scope)
            }
            _ => context.message(
                arguments["session"].as_str().unwrap_or_default(),
                arguments["message"].as_str().unwrap_or_default(),
                scope,
            ),
        };
        Some(result.unwrap_or_else(|error| format!("Error: {error}")))
    }
}

impl Context {
    fn store(&self, workspace: &std::path::Path) -> SessionStore {
        SessionStore::new(&self.paths, workspace.to_path_buf())
    }

    fn describe_kin(&self) -> String {
        if self.session.is_none() {
            return String::new();
        }
        let sessions = self.store(&self.workspace).list().unwrap_or_default();
        let family = Family::of(self.session, &sessions);
        let mut kin: Vec<(Relation, &SessionSummary)> = sessions
            .iter()
            .map(|summary| (family.relation(summary.id), summary))
            .filter(|(relation, _)| relation.is_kin())
            .collect();
        if kin.is_empty() {
            return String::new();
        }
        kin.sort_by_key(|(relation, _)| *relation);
        let live = live_sessions(&self.paths);
        let rows: Vec<String> = kin
            .into_iter()
            .take(KIN_LIMIT)
            .map(|(relation, summary)| {
                let state = live.get(&summary.id).map_or_else(|| "closed".to_owned(), Live::describe);
                format!(
                    "- {} \"{}\" — {} · {state}",
                    short(summary.id),
                    summary.title,
                    relation.label().unwrap_or_default()
                )
            })
            .collect();
        format!("Sessions related to this one:\n{}\n\n{KIN_GUIDANCE}", rows.join("\n"))
    }

    /// Every session in `scope`, by project, this project first.
    fn projects(&self, scope: Scope) -> Vec<(PathBuf, Vec<SessionSummary>)> {
        let here = (self.workspace.clone(), self.store(&self.workspace).list().unwrap_or_default());
        if scope == Scope::Project {
            return vec![here];
        }
        let others = crate::session::list_all(&self.paths)
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.workspace != self.workspace)
            .map(|entry| (entry.workspace, entry.sessions));
        std::iter::once(here).chain(others).collect()
    }

    fn list(&self, scope: Scope, query: &str) -> String {
        let live = live_sessions(&self.paths);
        let needle = query.trim().to_lowercase();
        let mut out = String::new();
        for (workspace, sessions) in self.projects(scope) {
            let here = workspace == self.workspace;
            let family = if here { Family::of(self.session, &sessions) } else { Family::default() };
            let mut rows: Vec<&SessionSummary> = sessions
                .iter()
                .filter(|summary| needle.is_empty() || summary.title.to_lowercase().contains(&needle))
                .collect();
            // Kin first, in the order a person would ask about them; the rest
            // keep the listing's newest-first order.
            rows.sort_by_key(|summary| family.relation(summary.id));
            if rows.is_empty() {
                continue;
            }
            let shown = rows.len().min(LIST_LIMIT);
            out.push_str(&format!("{} ({}):\n", if here { "This project" } else { "Project" }, workspace.display()));
            for summary in &rows[..shown] {
                out.push_str(&row(summary, family.relation(summary.id), live.get(&summary.id)));
                out.push('\n');
            }
            if rows.len() > shown {
                out.push_str(&format!("…and {} more; narrow with query.\n", rows.len() - shown));
            }
            out.push('\n');
        }
        if out.is_empty() {
            return if needle.is_empty() {
                "No other sessions in this project yet.".to_owned()
            } else {
                format!("No session title contains \"{query}\".")
            };
        }
        out.trim_end().to_owned()
    }

    fn read(&self, reference: &str, count: usize, scope: Scope) -> Result<String, String> {
        let found = self.find(reference, scope)?;
        let session =
            self.store(&found.workspace).load(&found.summary.id.to_string()).map_err(|error| error.to_string())?;
        let live = live_sessions(&self.paths).remove(&found.summary.id);
        let mut lines: Vec<String> = Vec::new();
        for message in session.messages.iter().rev() {
            let text = crate::text::content_text(&message["content"]);
            let line = match message["role"].as_str() {
                Some("user") if !text.trim().is_empty() => format!("[user] {}", text.trim()),
                Some("assistant") => {
                    let calls: Vec<&str> = message["tool_calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|call| call.pointer("/function/name").and_then(Value::as_str))
                        .collect();
                    match (text.trim(), calls.is_empty()) {
                        ("", true) => continue,
                        ("", false) => format!("[agent used {}]", calls.join(", ")),
                        (said, true) => format!("[agent] {said}"),
                        (said, false) => format!("[agent] {said}\n[agent used {}]", calls.join(", ")),
                    }
                }
                _ => continue,
            };
            lines.push(crate::text::clip(&line, READ_MESSAGE_CHARS, " …[cut]"));
            if lines.len() == count {
                break;
            }
        }
        // Newest kept within the budget, then put back in reading order.
        let mut used = 0;
        let kept = lines.iter().take_while(|line| {
            used += line.len() + 1;
            used <= READ_BUDGET
        });
        let mut kept: Vec<&String> = kept.collect();
        kept.reverse();
        let mut out = format!("{}\n", heading(&found, live.as_ref(), self));
        if kept.is_empty() {
            out.push_str("Nothing has been said there yet.");
        } else {
            out.push_str(&format!("Its last {} message(s), oldest first:\n", kept.len()));
            for line in kept {
                out.push_str(line);
                out.push('\n');
            }
        }
        Ok(out.trim_end().to_owned())
    }

    fn message(&self, reference: &str, text: &str, scope: Scope) -> Result<String, String> {
        if text.trim().is_empty() {
            return Err("the message is empty".to_owned());
        }
        let found = self.find(reference, scope)?;
        if Some(found.summary.id) == self.session {
            return Err("that is this session — say it here instead".to_owned());
        }
        let title = self
            .session
            .and_then(|id| self.store(&self.workspace).list().ok()?.into_iter().find(|summary| summary.id == id))
            .map_or_else(|| "an unsaved session".to_owned(), |summary| summary.title);
        let from = crate::inbox::Sender { session: self.session, title, workspace: self.workspace.clone() };
        crate::inbox::send(&self.paths, found.summary.id, from, text.trim()).map_err(|error| format!("{error:#}"))?;
        let name = format!("\"{}\" ({})", found.summary.title, short(found.summary.id));
        Ok(match live_sessions(&self.paths).remove(&found.summary.id) {
            Some(live) if live.working => {
                format!(
                    "Sent to {name}. It is working in {}, and reads your message at its next step.",
                    live.front_end.place()
                )
            }
            Some(live) => {
                format!(
                    "Sent to {name}. It is open and idle in {}, so it starts a turn on your message now.",
                    live.front_end.place()
                )
            }
            None => format!(
                "Sent to {name}. Nobody has it open, so the message waits until someone opens that session. \
                 If you need an answer now, tell the user."
            ),
        })
    }

    /// The one session `reference` names: a full id or a unique prefix.
    fn find(&self, reference: &str, scope: Scope) -> Result<Found, String> {
        let reference = reference.trim().trim_matches('"').to_lowercase();
        if reference.len() < 4 {
            return Err("name the session by its id, or at least its first 4 characters".to_owned());
        }
        let matches = |scope| -> Vec<Found> {
            self.projects(scope)
                .into_iter()
                .flat_map(|(workspace, sessions)| {
                    sessions.into_iter().map(move |summary| Found { summary, workspace: workspace.clone() })
                })
                .filter(|found| found.summary.id.to_string().starts_with(&reference))
                .collect()
        };
        let mut found = matches(scope);
        if found.is_empty() && scope == Scope::Project {
            // Say where it is rather than "no such session", so the model can
            // ask the user instead of concluding it does not exist.
            if let Some(elsewhere) = matches(Scope::All).first() {
                return Err(format!(
                    "session {reference} belongs to another project, {}. Reach it with scope \"all\" only if the \
                     user asked you to work across projects.",
                    elsewhere.workspace.display()
                ));
            }
        }
        match found.len() {
            0 => Err(format!("no session id starts with \"{reference}\" — session_list shows them")),
            1 => Ok(found.remove(0)),
            _ => Err(format!("\"{reference}\" matches {} sessions; give more of the id", found.len())),
        }
    }
}

/// This session's kin within its project: the session it was forked from, its
/// forks (and theirs), and its siblings.
#[derive(Default)]
struct Family {
    current: Option<Uuid>,
    parent: Option<Uuid>,
    /// Session -> the session it was forked from.
    forked_from: HashMap<Uuid, Uuid>,
}

impl Family {
    fn of(current: Option<Uuid>, sessions: &[SessionSummary]) -> Self {
        let forked_from: HashMap<Uuid, Uuid> =
            sessions.iter().filter_map(|summary| Some((summary.id, summary.forked_from?))).collect();
        let parent = current.and_then(|id| forked_from.get(&id).copied());
        Self { current, parent, forked_from }
    }

    /// How many forks lead from this session down to `id`, if any do.
    fn descent(&self, id: Uuid) -> Option<usize> {
        let current = self.current?;
        let mut at = id;
        for depth in 1..=64 {
            at = *self.forked_from.get(&at)?;
            if at == current {
                return Some(depth);
            }
        }
        None
    }

    fn relation(&self, id: Uuid) -> Relation {
        if Some(id) == self.current {
            return Relation::Itself;
        }
        if Some(id) == self.parent {
            return Relation::Parent;
        }
        match self.descent(id) {
            Some(1) => return Relation::Fork,
            Some(_) => return Relation::ForkOfFork,
            None => {}
        }
        if self.parent.is_some() && self.forked_from.get(&id) == self.parent.as_ref() {
            Relation::Sibling
        } else {
            Relation::Unrelated
        }
    }
}

/// How a session is related to this one. The list shows kin in this order; a
/// stable sort keeps each band in the listing's newest-first order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Relation {
    Itself,
    Parent,
    Fork,
    ForkOfFork,
    Sibling,
    Unrelated,
}

impl Relation {
    /// The one it was forked from, its forks and theirs, and its siblings.
    fn is_kin(self) -> bool {
        matches!(self, Self::Parent | Self::Fork | Self::ForkOfFork | Self::Sibling)
    }

    fn label(self) -> Option<&'static str> {
        match self {
            Self::Itself => Some("this session"),
            Self::Parent => Some("this session was forked from it"),
            Self::Fork => Some("fork of this session"),
            Self::ForkOfFork => Some("fork of one of this session's forks"),
            Self::Sibling => Some("forked from the same session as this one"),
            Self::Unrelated => None,
        }
    }
}

fn row(summary: &SessionSummary, relation: Relation, live: Option<&Live>) -> String {
    let mut facts: Vec<String> = Vec::new();
    facts.extend(relation.label().map(str::to_owned));
    facts.push(live.map_or_else(|| "closed".to_owned(), Live::describe));
    facts.push(format!("updated {}", ago(summary.updated_at)));
    facts.push(format!("{} messages", summary.message_count));
    format!("- {} \"{}\" — {}", short(summary.id), summary.title, facts.join(" · "))
}

fn heading(found: &Found, live: Option<&Live>, context: &Context) -> String {
    let project =
        if found.workspace == context.workspace { String::new() } else { format!(" in {}", found.workspace.display()) };
    format!(
        "\"{}\" ({}){project} — {} · updated {}",
        found.summary.title,
        short(found.summary.id),
        live.map_or_else(|| "closed".to_owned(), Live::describe),
        ago(found.summary.updated_at)
    )
}

/// The first 8 characters of an id: unique in practice, and what the tools accept.
fn short(id: Uuid) -> String {
    id.to_string()[..8].to_owned()
}

fn ago(at: DateTime<Utc>) -> String {
    let seconds = Utc::now().signed_duration_since(at).num_seconds().max(0);
    match seconds {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::{FrontEnd, Presence};
    use serde_json::json;

    /// A workspace with a session tree: main, two forks of it, and a fork of
    /// the first fork; plus an unrelated session and one in another project.
    struct World {
        _directory: tempfile::TempDir,
        paths: AbacusPaths,
        api: PathBuf,
        ids: HashMap<&'static str, Uuid>,
    }

    fn world() -> World {
        let directory = tempfile::tempdir().unwrap();
        let paths = AbacusPaths::under(directory.path().join("home"));
        let api = directory.path().join("api");
        let site = directory.path().join("site");
        let mut ids = HashMap::new();
        let make = |name: &'static str, workspace: &PathBuf, parent: Option<&'static str>, ids: &mut HashMap<_, _>| {
            let store = SessionStore::new(&paths, workspace.clone());
            let mut session = store
                .create(
                    "default".into(),
                    "m".into(),
                    vec![
                        json!({"role": "system", "content": "rules"}),
                        json!({"role": "user", "content": format!("work on {name}")}),
                        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}]}),
                        json!({"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": "a very long file body"}),
                        json!({"role": "assistant", "content": format!("{name} is done")}),
                    ],
                )
                .unwrap();
            session.title = name.to_owned();
            session.forked_from = parent.map(|parent: &str| ids[parent]);
            store.save(&session).unwrap();
            ids.insert(name, session.id);
        };
        make("main", &api, None, &mut ids);
        make("fork-a", &api, Some("main"), &mut ids);
        make("fork-b", &api, Some("main"), &mut ids);
        make("fork-of-a", &api, Some("fork-a"), &mut ids);
        make("unrelated", &api, None, &mut ids);
        make("elsewhere", &site, None, &mut ids);
        World { _directory: directory, paths, api, ids }
    }

    fn peers(world: &World, as_session: &str) -> Peers {
        Peers::new(world.paths.clone(), world.api.clone(), Some(world.ids[as_session]))
    }

    fn call(peers: &Peers, name: &str, arguments: Value) -> String {
        peers.execute(name, &arguments.to_string()).unwrap()
    }

    #[test]
    fn the_list_leads_with_this_sessions_family() {
        let world = world();
        let listed = call(&peers(&world, "fork-a"), "session_list", json!({}));
        let order: Vec<&str> = ["\"fork-a\"", "\"main\"", "\"fork-of-a\"", "\"fork-b\"", "\"unrelated\""]
            .into_iter()
            .map(|title| &listed[listed.find(title).unwrap_or_else(|| panic!("{title} missing:\n{listed}"))..])
            .collect();
        assert!(order.windows(2).all(|pair| pair[0].len() > pair[1].len()), "kin first:\n{listed}");
        assert!(listed.contains("\"main\" — this session was forked from it"));
        assert!(listed.contains("\"fork-of-a\" — fork of this session"));
        assert!(listed.contains("\"fork-b\" — forked from the same session as this one"));
        assert!(!listed.contains("elsewhere"), "other projects stay out by default");
        assert!(call(&peers(&world, "fork-a"), "session_list", json!({"scope": "all"})).contains("\"elsewhere\""));
    }

    #[tokio::test]
    async fn the_list_says_which_sessions_are_open() {
        let world = world();
        let presence = Presence::open(&world.paths, world.ids["fork-b"], FrontEnd::Tui, &world.api);
        presence.set_working(true);
        let listed = call(&peers(&world, "main"), "session_list", json!({}));
        assert!(listed.contains("\"fork-b\" — fork of this session · working in a terminal"), "{listed}");
        assert!(listed.contains("\"unrelated\" — closed"));
    }

    #[test]
    fn a_read_shows_what_was_said_not_tool_output() {
        let world = world();
        let id = short(world.ids["fork-b"]);
        let read = call(&peers(&world, "main"), "session_read", json!({"session": id}));
        assert!(read.contains("[user] work on fork-b"));
        assert!(read.contains("[agent used read_file]"));
        assert!(read.contains("[agent] fork-b is done"));
        assert!(!read.contains("a very long file body"));
        assert!(read.find("[user]").unwrap() < read.find("[agent] fork-b").unwrap(), "oldest first");
    }

    #[test]
    fn a_message_lands_in_the_receivers_inbox_signed_by_the_sender() {
        let world = world();
        let reply = call(
            &peers(&world, "fork-a"),
            "session_message",
            json!({"session": world.ids["main"].to_string(), "message": "the importer fix is on fork-a"}),
        );
        assert!(reply.contains("Nobody has it open"), "{reply}");
        let delivered = crate::inbox::take(&world.paths, world.ids["main"]);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].text, "the importer fix is on fork-a");
        assert_eq!(delivered[0].from.title, "fork-a");
        assert_eq!(delivered[0].from.session, Some(world.ids["fork-a"]));
    }

    #[test]
    fn reaching_another_project_takes_an_explicit_scope() {
        let world = world();
        let elsewhere = short(world.ids["elsewhere"]);
        let refused = call(&peers(&world, "main"), "session_message", json!({"session": elsewhere, "message": "hi"}));
        assert!(refused.starts_with("Error:") && refused.contains("another project"), "{refused}");
        let sent = call(
            &peers(&world, "main"),
            "session_message",
            json!({"session": elsewhere, "message": "hi", "scope": "all"}),
        );
        assert!(sent.starts_with("Sent to"), "{sent}");
    }

    #[test]
    fn a_session_cannot_message_itself() {
        let world = world();
        let own = world.ids["main"].to_string();
        let refused = call(&peers(&world, "main"), "session_message", json!({"session": own, "message": "hi"}));
        assert!(refused.contains("this session"), "{refused}");
    }

    #[tokio::test]
    async fn a_session_is_told_its_kin_and_how_to_work_beside_them() {
        let world = world();
        let presence = Presence::open(&world.paths, world.ids["main"], FrontEnd::Tui, &world.api);
        presence.set_working(true);
        let layer = peers(&world, "fork-a").kin_layer();
        assert!(layer.contains("\"main\" — this session was forked from it · working in a terminal"), "{layer}");
        assert!(layer.contains("\"fork-of-a\" — fork of this session · closed"), "{layer}");
        assert!(layer.contains("\"fork-b\" — forked from the same session as this one"), "{layer}");
        assert!(!layer.contains("unrelated") && !layer.contains("elsewhere"), "kin only: {layer}");
        assert!(layer.find("\"main\"").unwrap() < layer.find("\"fork-b\"").unwrap(), "nearest first");
        assert!(layer.contains("before you change files it may be changing"), "{layer}");
        assert!(layer.contains("When you finish a piece of work"), "{layer}");

        assert_eq!(peers(&world, "unrelated").kin_layer(), "", "no kin, nothing to say");
        assert_eq!(Peers::new(world.paths.clone(), world.api.clone(), None).kin_layer(), "", "not saved yet");
    }

    #[test]
    fn only_a_message_to_kin_skips_the_review() {
        let world = world();
        let to = |name: &str| json!({"session": short(world.ids[name]), "message": "done", "scope": "all"}).to_string();
        let fork_a = peers(&world, "fork-a");
        assert!(fork_a.messages_kin(&to("main")), "the session it was forked from");
        assert!(fork_a.messages_kin(&to("fork-of-a")), "its fork");
        assert!(fork_a.messages_kin(&to("fork-b")), "a sibling");
        assert!(!fork_a.messages_kin(&to("unrelated")));
        assert!(!fork_a.messages_kin(&to("elsewhere")), "another project is never kin");
        assert!(!fork_a.messages_kin("not json"));
        assert!(!Peers::default().messages_kin(&to("main")));
    }

    #[test]
    fn other_tools_are_not_answered_here() {
        assert!(Peers::default().execute("read_file", "{}").is_none());
        assert!(Peers::default().execute("session_list", "{}").unwrap().starts_with("Error:"));
    }
}
