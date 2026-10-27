//! Which sessions are open right now, in every abacus on this machine.
//!
//! A front end with a session open — the TUI, the desktop's app server, a
//! headless run — keeps one small file in `~/.abacus/live/` saying so, and
//! whether a turn is running. The file is rewritten on a heartbeat and removed
//! when the session closes, so a record whose heartbeat is older than
//! [`STALE_AFTER`] belongs to a process that died without cleaning up, and is
//! ignored (and swept).
//!
//! This is what lets a session ask which of its forks are being worked on, and
//! what tells a sender whether its message will be read now or later.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{AbacusPaths, atomic_write};

/// How often an open session's record is rewritten.
const HEARTBEAT: Duration = Duration::from_secs(20);
/// A record this old outlived its process: a crash, a kill, a laptop lid.
const STALE_AFTER: Duration = Duration::from_secs(75);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontEnd {
    Tui,
    Desktop,
    Headless,
}

impl FrontEnd {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Desktop => "desktop",
            Self::Headless => "headless",
        }
    }

    /// Where the session is open, as a sentence names it.
    pub fn place(self) -> &'static str {
        match self {
            Self::Tui => "a terminal",
            Self::Desktop => "the desktop app",
            Self::Headless => "a headless run",
        }
    }
}

/// One process's record of one open session.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    session: Uuid,
    pid: u32,
    front_end: FrontEnd,
    workspace: PathBuf,
    working: bool,
    heartbeat: DateTime<Utc>,
}

/// A session open somewhere, as a reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    /// A turn is running in at least one of the places it is open.
    pub working: bool,
    /// Where it is open; the one working, when one is.
    pub front_end: FrontEnd,
    pub workspace: PathBuf,
}

impl Live {
    /// `working in a terminal`, `open in the desktop app`.
    pub fn describe(&self) -> String {
        let state = if self.working { "working" } else { "open" };
        format!("{state} in {}", self.front_end.place())
    }

    /// The shape front ends read: `{"state": "working", "frontEnd": "tui"}`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": if self.working { "working" } else { "idle" },
            "frontEnd": self.front_end.label(),
        })
    }
}

fn live_dir(paths: &AbacusPaths) -> PathBuf {
    paths.root.join("live")
}

/// This process's claim on one open session. Dropping it says the session
/// closed here.
pub struct Presence {
    file: PathBuf,
    record: Arc<Mutex<Record>>,
    beat: Option<tokio::task::JoinHandle<()>>,
}

impl Presence {
    /// Announce `session` as open here. The heartbeat runs on the current
    /// tokio runtime; outside one, the record is written once and goes stale.
    pub fn open(paths: &AbacusPaths, session: Uuid, front_end: FrontEnd, workspace: &Path) -> Self {
        // Keyed by process as well as session, so a session open in two
        // places keeps two records and closing one leaves the other.
        let file = live_dir(paths).join(format!("{session}-{}.json", std::process::id()));
        let record = Arc::new(Mutex::new(Record {
            session,
            pid: std::process::id(),
            front_end,
            workspace: workspace.to_path_buf(),
            working: false,
            heartbeat: Utc::now(),
        }));
        write(&file, &record);
        let beat = tokio::runtime::Handle::try_current().ok().map(|runtime| {
            let (file, record) = (file.clone(), record.clone());
            runtime.spawn(async move {
                let mut ticks = tokio::time::interval(HEARTBEAT);
                ticks.tick().await;
                loop {
                    ticks.tick().await;
                    lock(&record).heartbeat = Utc::now();
                    write(&file, &record);
                }
            })
        });
        Self { file, record, beat }
    }

    pub fn session(&self) -> Uuid {
        lock(&self.record).session
    }

    /// Say whether a turn is running, so a sender knows its message lands
    /// mid-turn rather than starting one.
    pub fn set_working(&self, working: bool) {
        {
            let mut record = lock(&self.record);
            if record.working == working {
                return;
            }
            record.working = working;
            record.heartbeat = Utc::now();
        }
        write(&self.file, &self.record);
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        if let Some(beat) = self.beat.take() {
            beat.abort();
        }
        let _ = fs::remove_file(&self.file);
    }
}

fn lock(record: &Mutex<Record>) -> std::sync::MutexGuard<'_, Record> {
    record.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Best effort: a record that cannot be written makes the session look closed,
/// which costs a sender nothing worse than a "not open" answer.
fn write(file: &Path, record: &Mutex<Record>) {
    let Ok(content) = serde_json::to_vec(&*lock(record)) else { return };
    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = atomic_write(file, &content, true);
}

/// Every session open somewhere on this machine, by id. Stale records are
/// skipped and removed.
pub fn live_sessions(paths: &AbacusPaths) -> HashMap<Uuid, Live> {
    let mut live: HashMap<Uuid, Live> = HashMap::new();
    let Ok(entries) = fs::read_dir(live_dir(paths)) else {
        return live;
    };
    let now = Utc::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(record) = fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok()) else {
            continue;
        };
        let age = now.signed_duration_since(record.heartbeat).to_std().unwrap_or_default();
        if age > STALE_AFTER {
            let _ = fs::remove_file(&path);
            continue;
        }
        let entry = Live { working: record.working, front_end: record.front_end, workspace: record.workspace };
        match live.get_mut(&record.session) {
            // Working anywhere wins: that is the place a message lands mid-turn.
            Some(existing) if entry.working && !existing.working => *existing = entry,
            Some(_) => {}
            None => {
                live.insert(record.session, entry);
            }
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, AbacusPaths) {
        let directory = tempfile::tempdir().unwrap();
        let paths = AbacusPaths::under(directory.path().to_path_buf());
        (directory, paths)
    }

    #[tokio::test]
    async fn an_open_session_is_live_until_it_closes() {
        let (_directory, paths) = paths();
        let id = Uuid::new_v4();
        let presence = Presence::open(&paths, id, FrontEnd::Tui, Path::new("/work/api"));
        let seen = live_sessions(&paths);
        assert_eq!(seen[&id].front_end, FrontEnd::Tui);
        assert!(!seen[&id].working);

        presence.set_working(true);
        assert!(live_sessions(&paths)[&id].working);
        assert_eq!(live_sessions(&paths)[&id].describe(), "working in a terminal");

        drop(presence);
        assert!(!live_sessions(&paths).contains_key(&id));
    }

    #[test]
    fn a_record_left_by_a_dead_process_is_swept() {
        let (_directory, paths) = paths();
        let id = Uuid::new_v4();
        let file = live_dir(&paths).join(format!("{id}-1.json"));
        let record = Mutex::new(Record {
            session: id,
            pid: 1,
            front_end: FrontEnd::Desktop,
            workspace: PathBuf::from("/work/api"),
            working: true,
            heartbeat: Utc::now() - chrono::Duration::minutes(10),
        });
        write(&file, &record);
        assert!(live_sessions(&paths).is_empty());
        assert!(!file.exists(), "the stale record is removed");
    }

    #[test]
    fn working_anywhere_wins_for_a_session_open_twice() {
        let (_directory, paths) = paths();
        let id = Uuid::new_v4();
        for (pid, front_end, working) in [(11, FrontEnd::Desktop, false), (12, FrontEnd::Tui, true)] {
            let record = Mutex::new(Record {
                session: id,
                pid,
                front_end,
                workspace: PathBuf::from("/work/api"),
                working,
                heartbeat: Utc::now(),
            });
            write(&live_dir(&paths).join(format!("{id}-{pid}.json")), &record);
        }
        let seen = &live_sessions(&paths)[&id];
        assert!(seen.working);
        assert_eq!(seen.front_end, FrontEnd::Tui);
    }
}
