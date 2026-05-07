//! Read-side library for `~/.claude/custom-state/agent-readmodel.json` and
//! `~/.claude/custom-state/agent-seen-events/<sid>.json`.
//!
//! Shared by the `agent-bar` and `compact-bar` zellij plugins so the schema
//! lives in exactly one place. The daemon (`sessions watch`, in foam/code/
//! sessions) is the writer; this crate is what consumers parse with.
//!
//! Plugin-specific projection (filter to current zellij session, aggregate
//! per-tab, etc.) stays in each plugin's own `agents.rs` — those depend on
//! `zellij-tile`'s `PaneManifest` and don't belong here.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Busy,
    Idle,
}

/// One entry in the readmodel. The plugin only needs the fields it renders;
/// the rest are kept here so consumers can use whatever subset they want
/// without redeclaring the schema.
#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub session_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cwd: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub zellij_session: Option<String>,
    #[serde(default)]
    pub zellij_pane_id: Option<u32>,
    /// Wall-clock millis of the latest attention-worthy event. 0 = none.
    /// `seen_at_ms` is **not** in the readmodel — it's read separately from
    /// `agent-seen-events/` since seen-state is plugin-owned.
    #[serde(default)]
    pub attention_at_ms: i64,
}

#[derive(Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub agents: Vec<Agent>,
}

/// `host_root` is what the plugin passes to `change_host_folder` to preopen
/// `/host`; `wasi_path` is what the plugin reads from inside the sandbox.
/// The two must agree, hence one struct.
pub struct SnapshotLocation {
    pub host_root: PathBuf,
    pub wasi_path: PathBuf,
    pub seen_events_dir: PathBuf,
}

/// Resolve readmodel + seen-events paths from the WASI session-env vars.
/// Preopens `$HOME` so cross-drive paths on Windows work — `change_host_folder("/")`
/// resolves relative to the zellij server's current drive, which has no
/// `Users` tree if zellij was launched from a different drive than the home dir.
pub fn locate_snapshot(env: &BTreeMap<String, String>) -> Option<SnapshotLocation> {
    let home = env.get("HOME").or_else(|| env.get("USERPROFILE"))?;
    Some(SnapshotLocation {
        host_root: PathBuf::from(home),
        wasi_path: PathBuf::from("/host/.claude/custom-state/agent-readmodel.json"),
        seen_events_dir: PathBuf::from("/host/.claude/custom-state/agent-seen-events"),
    })
}

#[derive(Debug, Clone)]
pub enum ReadResult {
    Ok(Vec<Agent>),
    Missing,
    Unreadable(String),
    ParseError(String),
}

/// Read and parse the readmodel file. Errors propagate as `ReadResult`
/// variants so the UI can surface what actually happened.
pub fn read_readmodel(path: &Path) -> ReadResult {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ReadResult::Missing,
        Err(e) => return ReadResult::Unreadable(e.to_string()),
    };
    match serde_json::from_str::<Snapshot>(&content) {
        Ok(snap) => ReadResult::Ok(snap.agents),
        Err(e) => ReadResult::ParseError(e.to_string()),
    }
}

#[derive(Deserialize)]
struct SeenRecord {
    session_id: String,
    #[serde(default)]
    seen_at_ms: i64,
}

/// Scan `agent-seen-events/` and return `claude_id → seen_at_ms`. Each file
/// represents the latest mark-seen for that agent — written by plugins
/// directly (not the daemon). Daemon only deletes orphans on agent death.
pub fn read_seen_events(dir: &Path) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else { continue };
        if let Ok(rec) = serde_json::from_str::<SeenRecord>(&content) {
            out.insert(rec.session_id, rec.seen_at_ms);
        }
    }
    out
}
