//! Memories: what the agent writes down for its future self.
//!
//! Architecture facts, decisions and their reasons, conventions, things
//! figured out the hard way. The model keeps them with `memory_record` and
//! `memory_forget`, and they reach every later session in the same workspace
//! (every workspace, for a global one) as a context layer, newest first and
//! bounded, so they inform a turn without crowding it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::schema::{opt, req, string, tool};

/// Ceiling on the injected context layer.
const INJECT_BUDGET_CHARS: usize = 1_800;
/// Newest-first cap on how many memories reach the prompt.
const INJECT_LIMIT: usize = 10;
/// One memory cannot monopolise the budget.
const MAX_BODY_CHARS: usize = 1_200;
/// Marks a `memories.json` this store wrote. Before the versioned harness the
/// file held a bare list, which the harness then took over.
const FILE_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub title: String,
    pub body: String,
    /// `None` applies everywhere; otherwise the canonical workspace path.
    #[serde(default)]
    pub workspace: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Memory {
    fn in_scope(&self, workspace: &str) -> bool {
        self.workspace.as_deref().is_none_or(|scope| scope == workspace)
    }
}

#[derive(Serialize, Deserialize)]
struct MemoryFile {
    version: u32,
    memories: Vec<Memory>,
}

#[derive(Default)]
struct Inner {
    memories: Vec<Memory>,
    /// `None` keeps them in memory only: the inert store tests start from.
    file: Option<PathBuf>,
}

/// Shared, cloneable handle. `Default` yields an inert in-memory store.
#[derive(Clone, Default)]
pub struct MemoryStore {
    inner: Arc<RwLock<Inner>>,
    workspace: String,
}

impl MemoryStore {
    /// Load the memories kept in `file`. A file this store did not write yet
    /// is imported once: from the versioned harness under `harness_dir` when
    /// there is one, since it took the old list over and kept every memory
    /// since, or else from the old list itself. Neither old file is changed.
    pub fn load(file: PathBuf, harness_dir: &Path, workspace: &Path) -> Self {
        let read = std::fs::read_to_string(&file).ok();
        let memories = match read.as_deref().and_then(|content| serde_json::from_str::<MemoryFile>(content).ok()) {
            Some(stored) if stored.version == FILE_VERSION => stored.memories,
            _ => {
                let imported = import(read.as_deref(), harness_dir);
                if !imported.is_empty() {
                    save(&file, &imported);
                }
                imported
            }
        };
        Self {
            inner: Arc::new(RwLock::new(Inner { memories, file: Some(file) })),
            workspace: workspace.to_string_lossy().into_owned(),
        }
    }

    /// The memories applying to this workspace, newest first, for `/memory`.
    pub fn snapshot(&self) -> Vec<Memory> {
        let inner = self.inner.read().expect("memory lock");
        let mut memories: Vec<Memory> =
            inner.memories.iter().filter(|memory| memory.in_scope(&self.workspace)).cloned().collect();
        memories.sort_by_key(|memory| std::cmp::Reverse(memory.updated_at));
        memories
    }

    /// Delete one by id, for `/memory forget`.
    pub fn remove(&self, id: Uuid) -> bool {
        let mut inner = self.inner.write().expect("memory lock");
        let before = inner.memories.len();
        inner.memories.retain(|memory| memory.id != id);
        let removed = inner.memories.len() != before;
        if removed {
            Self::save_locked(&inner);
        }
        removed
    }

    fn save_locked(inner: &Inner) {
        if let Some(file) = &inner.file {
            save(file, &inner.memories);
        }
    }

    pub fn tool_specs() -> Vec<Value> {
        vec![
            tool(
                "memory_record",
                "Save a durable memory for future sessions: an architecture fact, a decision and its reason, a convention, a roadmap change, or something figured out the hard way. Re-recording an existing title replaces its body — use that to keep memories current.",
                [
                    req("title", string("Short stable name, e.g. 'auth flow uses one-shot tokens'")),
                    req("body", string("The memory itself, concise and self-contained (max ~1200 chars)")),
                    opt(
                        "scope",
                        json!({"type": "string", "enum": ["workspace", "global"], "description": "Which projects it applies to: workspace (default) or global"}),
                    ),
                ],
            ),
            tool(
                "memory_forget",
                "Delete a memory that is stale or wrong, by its exact title.",
                [req("title", string("Exact title of the memory to delete"))],
            ),
        ]
    }

    pub fn execute(&self, name: &str, arguments: &str) -> Option<String> {
        match name {
            "memory_record" => Some(crate::tools::reply(self.record(arguments))),
            "memory_forget" => Some(crate::tools::reply(self.forget(arguments))),
            _ => None,
        }
    }

    fn record(&self, arguments: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Arguments {
            title: String,
            body: String,
            scope: Option<String>,
        }
        let arguments: Arguments = serde_json::from_str(arguments).context("invalid memory_record arguments")?;
        let title = arguments.title.trim().to_owned();
        if title.is_empty() || title.chars().count() > 120 {
            bail!("title must be 1-120 characters");
        }
        let body = arguments.body.trim();
        if body.is_empty() {
            bail!("body must not be empty");
        }
        let body = crate::text::clip(body, MAX_BODY_CHARS, "…");
        let workspace = match arguments.scope.as_deref() {
            Some("global") => None,
            _ => Some(self.workspace.clone()),
        };

        let mut inner = self.inner.write().expect("memory lock");
        let now = Utc::now();
        let existing = inner
            .memories
            .iter_mut()
            .find(|memory| memory.title.eq_ignore_ascii_case(&title) && memory.workspace == workspace);
        let reply = match existing {
            Some(memory) => {
                memory.body = body;
                memory.updated_at = now;
                format!("Memory \"{}\" updated.", memory.title)
            }
            None => {
                let reply = format!("Memory \"{title}\" recorded.");
                inner.memories.push(Memory {
                    id: Uuid::new_v4(),
                    title,
                    body,
                    workspace,
                    created_at: now,
                    updated_at: now,
                });
                reply
            }
        };
        Self::save_locked(&inner);
        Ok(reply)
    }

    fn forget(&self, arguments: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Arguments {
            title: String,
        }
        let arguments: Arguments = serde_json::from_str(arguments).context("invalid memory_forget arguments")?;
        let title = arguments.title.trim();
        let mut inner = self.inner.write().expect("memory lock");
        let before = inner.memories.len();
        inner.memories.retain(|memory| !(memory.title.eq_ignore_ascii_case(title) && memory.in_scope(&self.workspace)));
        if inner.memories.len() == before {
            bail!("no memory titled `{title}` in this workspace");
        }
        Self::save_locked(&inner);
        Ok(format!("Memory \"{title}\" forgotten."))
    }

    /// The context layer the model reads them from.
    pub fn prompt_context(&self) -> String {
        let memories = self.snapshot();
        if memories.is_empty() {
            return String::new();
        }
        let mut context =
            String::from("Memories from earlier sessions (keep them current with memory_record / memory_forget):");
        let mut used = 0_usize;
        for memory in memories.into_iter().take(INJECT_LIMIT) {
            let line = format!("\n- {}: {}", memory.title, memory.body);
            if used + line.len() > INJECT_BUDGET_CHARS {
                break;
            }
            used += line.len();
            context.push_str(&line);
        }
        context
    }
}

fn save(file: &Path, memories: &[Memory]) {
    crate::config::write_json(file, &MemoryFile { version: FILE_VERSION, memories: memories.to_vec() });
}

/// The memories to start from when `memories.json` is not this store's yet.
fn import(legacy: Option<&str>, harness_dir: &Path) -> Vec<Memory> {
    #[derive(Deserialize)]
    struct HarnessMemory {
        title: String,
        content: String,
        #[serde(default)]
        workspace: Option<String>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    }
    #[derive(Deserialize)]
    struct HarnessEntries {
        #[serde(default)]
        memory: BTreeMap<String, HarnessMemory>,
    }
    #[derive(Deserialize)]
    struct HarnessState {
        entries: HarnessEntries,
    }
    let harness = std::fs::read_to_string(harness_dir.join("state.json"))
        .ok()
        .and_then(|content| serde_json::from_str::<HarnessState>(&content).ok());
    match harness {
        Some(state) => state
            .entries
            .memory
            .into_values()
            .map(|entry| Memory {
                id: Uuid::new_v4(),
                title: entry.title,
                body: entry.content,
                workspace: entry.workspace,
                created_at: entry.created_at,
                updated_at: entry.updated_at,
            })
            .collect(),
        None => legacy.and_then(|content| serde_json::from_str(content).ok()).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path, workspace: &str) -> MemoryStore {
        MemoryStore::load(dir.join("memories.json"), &dir.join("harness"), Path::new(workspace))
    }

    #[test]
    fn a_memory_is_kept_for_later_sessions_and_updated_by_title() {
        let dir = tempfile::tempdir().unwrap();
        let first = store(dir.path(), "/work/app");
        let reply = first.execute("memory_record", r#"{"title":"Auth flow","body":"one-shot tokens"}"#).unwrap();
        assert_eq!(reply, "Memory \"Auth flow\" recorded.");

        let later = store(dir.path(), "/work/app");
        assert!(later.prompt_context().contains("Auth flow: one-shot tokens"), "{}", later.prompt_context());
        let reply = later.execute("memory_record", r#"{"title":"auth flow","body":"tokens expire in 60s"}"#).unwrap();
        assert_eq!(reply, "Memory \"Auth flow\" updated.");
        assert_eq!(later.snapshot().len(), 1);
        assert_eq!(later.snapshot()[0].body, "tokens expire in 60s");
    }

    #[test]
    fn a_workspace_memory_stays_in_its_workspace_and_a_global_one_goes_everywhere() {
        let dir = tempfile::tempdir().unwrap();
        let app = store(dir.path(), "/work/app");
        app.execute("memory_record", r#"{"title":"Local","body":"only here"}"#);
        app.execute("memory_record", r#"{"title":"Style","body":"British spelling","scope":"global"}"#);

        let other = store(dir.path(), "/work/other");
        let titles: Vec<String> = other.snapshot().into_iter().map(|memory| memory.title).collect();
        assert_eq!(titles, ["Style"]);
        assert!(other.execute("memory_forget", r#"{"title":"Local"}"#).unwrap().starts_with("Error"));
        assert_eq!(app.execute("memory_forget", r#"{"title":"Local"}"#).unwrap(), "Memory \"Local\" forgotten.");
    }

    #[test]
    fn the_harness_memories_are_imported_once_and_its_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let harness = dir.path().join("harness");
        std::fs::create_dir_all(&harness).unwrap();
        let state = json!({"schema": 1, "entries": {
            "prompt": {"note": {"title": "Note", "content": "dropped"}},
            "memory": {"db": {"id": "db", "kind": "memory", "title": "Database", "content": "Postgres 16",
                "workspace": "/work/app", "lifetime": "durable", "source": "model", "version": 2,
                "created_at": "2026-09-01T10:00:00Z", "updated_at": "2026-09-02T10:00:00Z"}}}});
        std::fs::write(harness.join("state.json"), state.to_string()).unwrap();
        // The list from before the harness, which the harness already holds.
        std::fs::write(dir.path().join("memories.json"), r#"[{"title":"Stale","body":"old","created_at":"2026-08-01T10:00:00Z","updated_at":"2026-08-01T10:00:00Z"}]"#).unwrap();

        let imported = store(dir.path(), "/work/app");
        let titles: Vec<String> = imported.snapshot().into_iter().map(|memory| memory.title).collect();
        assert_eq!(titles, ["Database"]);
        assert!(harness.join("state.json").exists(), "an older version can still read it");

        // Imported once: what is forgotten here stays forgotten.
        let id = imported.snapshot()[0].id;
        assert!(imported.remove(id));
        assert!(store(dir.path(), "/work/app").snapshot().is_empty());
    }

    #[test]
    fn the_list_from_before_the_harness_is_imported_when_there_is_no_harness() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memories.json"), r#"[{"title":"Old","body":"kept","workspace":null,"created_at":"2026-08-01T10:00:00Z","updated_at":"2026-08-01T10:00:00Z"}]"#).unwrap();
        let memories = store(dir.path(), "/work/app").snapshot();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].body, "kept");
    }

    #[test]
    fn the_layer_is_bounded_and_newest_first() {
        let memories = MemoryStore::default();
        for index in 0..30 {
            let arguments = json!({"title": format!("memory {index:02}"), "body": "x".repeat(400)}).to_string();
            memories.execute("memory_record", &arguments);
        }
        let layer = memories.prompt_context();
        assert!(layer.len() < INJECT_BUDGET_CHARS + 200, "{}", layer.len());
        assert!(layer.contains("memory 29"), "newest first");
        assert!(!layer.contains("memory 00"));
    }
}
