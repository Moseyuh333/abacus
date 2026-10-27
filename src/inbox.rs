//! Messages between sessions.
//!
//! A session's inbox is a directory, `~/.abacus/inbox/<session id>/`, holding
//! one file per message. Sending writes a file. The front end that has the
//! session open takes them, oldest first, and hands them to its agent: between
//! tool calls while a turn runs, or as a turn of their own when it is idle.
//! Taking renames a file before reading it, so a session open in two places
//! reads each message once.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{AbacusPaths, atomic_write};
use crate::presence::{FrontEnd, Presence};

/// How often a front end looks in the open session's inbox.
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// Turns in a row that messages may start in an idle session before the
/// person says something. Two agents that keep answering each other would
/// otherwise run up a bill unattended; past this, messages wait.
pub const UNATTENDED_TURN_LIMIT: usize = 6;

/// A front end's side of the open session: its presence record, its inbox, and
/// how many turns in a row messages have started without the person.
pub struct Mailbox {
    paths: AbacusPaths,
    front_end: FrontEnd,
    presence: Option<Presence>,
    checked: Option<Instant>,
    unattended_turns: usize,
    /// How many waiting messages the person was last told about, so a held
    /// inbox is announced once, and again only when more arrive.
    announced: usize,
}

/// What a look in the inbox found.
#[derive(Debug)]
pub enum Collected {
    Nothing,
    /// Hand these to the agent: steering while a turn runs, or the next turn.
    Messages(Vec<Message>),
    /// This many wait, held until the person says something.
    Held(usize),
}

impl Mailbox {
    pub fn new(paths: AbacusPaths, front_end: FrontEnd) -> Self {
        Self { paths, front_end, presence: None, checked: None, unattended_turns: 0, announced: 0 }
    }

    /// Follow the session open here, or none. Messages already waiting for a
    /// session when it is opened are held: the person opening it is in charge,
    /// so they reach the agent with what the person sends first.
    pub fn open(&mut self, session: Option<Uuid>, workspace: &Path) {
        if self.presence.as_ref().map(Presence::session) == session {
            return;
        }
        self.presence = session.map(|id| Presence::open(&self.paths, id, self.front_end, workspace));
        self.checked = None;
        let waited = session.is_some_and(|id| waiting(&self.paths, id) > 0);
        self.unattended_turns = if waited { UNATTENDED_TURN_LIMIT } else { 0 };
        self.announced = 0;
    }

    pub fn set_working(&self, working: bool) {
        if let Some(presence) = &self.presence {
            presence.set_working(working);
        }
    }

    /// Let the next `collect` look at once, for a test that cannot wait out
    /// [`CHECK_EVERY`].
    #[cfg(test)]
    pub(crate) fn look_again(&mut self) {
        self.checked = None;
    }

    /// The person sent something: messages may start turns again.
    pub fn attended(&mut self) {
        self.unattended_turns = 0;
        self.announced = 0;
    }

    /// Look in the inbox, at most once a [`CHECK_EVERY`]. `idle` says no turn
    /// is running, so what is handed over starts one.
    pub fn collect(&mut self, idle: bool) -> Collected {
        let Some(session) = self.presence.as_ref().map(Presence::session) else {
            return Collected::Nothing;
        };
        if self.checked.is_some_and(|checked| checked.elapsed() < CHECK_EVERY) {
            return Collected::Nothing;
        }
        self.checked = Some(Instant::now());
        if idle && self.unattended_turns >= UNATTENDED_TURN_LIMIT {
            let count = waiting(&self.paths, session);
            if count == 0 || count == self.announced {
                return Collected::Nothing;
            }
            self.announced = count;
            return Collected::Held(count);
        }
        let messages = take(&self.paths, session);
        if messages.is_empty() {
            return Collected::Nothing;
        }
        if idle {
            self.unattended_turns += 1;
        }
        self.announced = 0;
        Collected::Messages(messages)
    }
}

/// What the person is told when messages wait for them.
pub fn held_notice(count: usize) -> String {
    if count == 1 {
        "A message from another session is waiting — it reaches the agent with what you send next.".to_owned()
    } else {
        format!("{count} messages from other sessions are waiting — they reach the agent with what you send next.")
    }
}

/// Who a message is from: a session, by id, title and project. `session` is
/// None for a message the person sent from outside any session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    pub session: Option<Uuid>,
    pub title: String,
    pub workspace: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub from: Sender,
    pub text: String,
    pub sent_at: DateTime<Utc>,
}

/// The first line of a message as the receiving model reads it, and how one
/// is recognised again in a transcript rebuilt from history.
const PROMPT_HEADER: &str = "[Message from another abacus session — not from the user]";

impl Message {
    /// The message as the receiving model reads it: who sent it, from where,
    /// and how to answer — the receiver has no other way of knowing.
    pub fn prompt(&self, workspace: &Path) -> String {
        let id = self.from.session.map(|id| format!(" ({id})")).unwrap_or_default();
        let project = if self.from.workspace == workspace {
            String::new()
        } else {
            format!(" in {}", self.from.workspace.display())
        };
        let reply = match self.from.session {
            Some(id) => format!("To answer, use session_message with session \"{id}\"."),
            None => "It did not come from a session, so there is nobody to answer.".to_owned(),
        };
        format!(
            "{PROMPT_HEADER}\nFrom: \"{}\"{id}{project}\nWeigh it as you would a colleague's note: act on it if \
             it fits what the user asked of you.\n\n{}\n\n{reply}",
            self.from.title, self.text
        )
    }

    /// How a transcript labels it: `from "Fix importer"`.
    pub fn origin(&self) -> String {
        format!("from \"{}\"", self.from.title)
    }
}

/// Who sent a message and what it said, from the text [`Message::prompt`]
/// put in a conversation; None for anything else. What a transcript rebuilt
/// from history shows instead of the whole prompt.
pub fn shown(prompt: &str) -> Option<(String, String)> {
    let rest = prompt.strip_prefix(PROMPT_HEADER)?.strip_prefix('\n')?;
    let (from, rest) = rest.split_once('\n')?;
    let from = from.strip_prefix("From: \"")?;
    let title = &from[..from.rfind('"')?];
    // The guidance line, then the text, then the line on how to answer.
    let (_, body) = rest.split_once("\n\n")?;
    let text = body.rsplit_once("\n\n").map_or(body, |(text, _)| text);
    Some((title.to_owned(), text.to_owned()))
}

fn inbox_dir(paths: &AbacusPaths, session: Uuid) -> PathBuf {
    paths.root.join("inbox").join(session.to_string())
}

/// Leave `text` in `to`'s inbox.
pub fn send(paths: &AbacusPaths, to: Uuid, from: Sender, text: &str) -> Result<Message> {
    let message = Message { id: Uuid::new_v4(), from, text: text.to_owned(), sent_at: Utc::now() };
    let directory = inbox_dir(paths, to);
    fs::create_dir_all(&directory).with_context(|| format!("could not create {}", directory.display()))?;
    // Named by time first, so a directory listing is the order they were sent.
    let file = directory.join(format!("{:013}-{}.json", message.sent_at.timestamp_millis(), message.id));
    atomic_write(&file, &serde_json::to_vec(&message)?, true)?;
    Ok(message)
}

/// How many messages wait for `session`.
pub fn waiting(paths: &AbacusPaths, session: Uuid) -> usize {
    message_files(paths, session).len()
}

/// Take every waiting message, oldest first. Each is claimed by renaming it
/// first, so when two front ends have the session open only one gets it.
pub fn take(paths: &AbacusPaths, session: Uuid) -> Vec<Message> {
    let mut taken = Vec::new();
    for file in message_files(paths, session) {
        let claimed = file.with_extension(format!("taken-{}", std::process::id()));
        if fs::rename(&file, &claimed).is_err() {
            continue;
        }
        let message = fs::read(&claimed).ok().and_then(|bytes| serde_json::from_slice::<Message>(&bytes).ok());
        let _ = fs::remove_file(&claimed);
        taken.extend(message);
    }
    taken
}

fn message_files(paths: &AbacusPaths, session: Uuid) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(inbox_dir(paths, session)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender(title: &str) -> Sender {
        Sender { session: Some(Uuid::new_v4()), title: title.to_owned(), workspace: PathBuf::from("/work/api") }
    }

    #[test]
    fn messages_arrive_once_in_the_order_sent() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AbacusPaths::under(directory.path().to_path_buf());
        let to = Uuid::new_v4();
        send(&paths, to, sender("Fix importer"), "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        send(&paths, to, sender("Fix importer"), "second").unwrap();
        assert_eq!(waiting(&paths, to), 2);

        let texts: Vec<String> = take(&paths, to).into_iter().map(|message| message.text).collect();
        assert_eq!(texts, ["first", "second"]);
        assert!(take(&paths, to).is_empty(), "a message is read once");
        assert_eq!(waiting(&paths, to), 0);
    }

    #[tokio::test]
    async fn an_open_session_holds_what_waited_and_takes_what_arrives() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AbacusPaths::under(directory.path().to_path_buf());
        let session = Uuid::new_v4();
        send(&paths, session, sender("Fork A"), "waited").unwrap();

        let mut mailbox = Mailbox::new(paths.clone(), FrontEnd::Tui);
        mailbox.open(Some(session), Path::new("/work/api"));
        // Waiting from before it was opened: held for the person.
        assert!(matches!(mailbox.collect(true), Collected::Held(1)));
        mailbox.look_again();
        assert!(matches!(mailbox.collect(true), Collected::Nothing), "announced once");

        // The person speaks; a turn of theirs runs and takes the message.
        mailbox.attended();
        mailbox.look_again();
        assert!(matches!(mailbox.collect(false), Collected::Messages(ref taken) if taken[0].text == "waited"));
    }

    #[tokio::test]
    async fn messages_stop_starting_turns_after_the_limit() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AbacusPaths::under(directory.path().to_path_buf());
        let session = Uuid::new_v4();
        let mut mailbox = Mailbox::new(paths.clone(), FrontEnd::Desktop);
        mailbox.open(Some(session), Path::new("/work/api"));
        mailbox.attended();
        for turn in 0..UNATTENDED_TURN_LIMIT {
            send(&paths, session, sender("Fork A"), &format!("ping {turn}")).unwrap();
            mailbox.look_again();
            assert!(matches!(mailbox.collect(true), Collected::Messages(_)), "turn {turn} starts");
        }
        send(&paths, session, sender("Fork A"), "one too many").unwrap();
        mailbox.look_again();
        assert!(matches!(mailbox.collect(true), Collected::Held(1)));
        // Mid-turn steering is not a turn of its own, so it still flows.
        mailbox.look_again();
        assert!(matches!(mailbox.collect(false), Collected::Messages(_)));
    }

    #[test]
    fn the_held_notice_reads_for_one_and_for_many() {
        assert_eq!(
            held_notice(1),
            "A message from another session is waiting — it reaches the agent with what you send next."
        );
        assert!(held_notice(3).starts_with("3 messages from other sessions are waiting — they reach"));
    }

    #[test]
    fn the_prompt_says_who_wrote_and_how_to_answer() {
        let from = sender("Fix importer");
        let message = Message {
            id: Uuid::new_v4(),
            from: from.clone(),
            text: "tests pass on my side".into(),
            sent_at: Utc::now(),
        };
        let here = message.prompt(Path::new("/work/api"));
        assert!(here.contains("\"Fix importer\""));
        assert!(here.contains("not from the user"));
        assert!(here.contains(&from.session.unwrap().to_string()));
        assert!(!here.contains("/work/api"), "the same project goes unsaid");
        assert!(message.prompt(Path::new("/work/site")).contains("in /work/api"));
    }

    #[test]
    fn a_message_is_recognised_in_history() {
        let message = Message {
            id: Uuid::new_v4(),
            from: sender("Fix \"importer\""),
            text: "two paragraphs\n\nof findings".into(),
            sent_at: Utc::now(),
        };
        let shown = shown(&message.prompt(Path::new("/work/site")));
        assert_eq!(shown, Some(("Fix \"importer\"".to_owned(), "two paragraphs\n\nof findings".to_owned())));
        assert_eq!(super::shown("an ordinary prompt"), None);
    }
}
