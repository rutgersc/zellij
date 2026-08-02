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

use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Busy,
    /// Claude is between turns, ready for the next user prompt.
    Idle,
    /// Claude is blocked on the user (permission, AskUserQuestion,
    /// ExitPlanMode). Distinct from Idle so the bar can surface it.
    Waiting,
    /// Anything Claude Code emits that isn't in the set above. New
    /// variants (e.g. `shell`) land here so the readmodel parse keeps
    /// succeeding — the plugin renders Unknown rows in attention/error
    /// styling instead of crashing the whole bar.
    Unknown(String),
}

impl<'de> Deserialize<'de> for AgentStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "busy" => AgentStatus::Busy,
            "idle" => AgentStatus::Idle,
            "waiting" => AgentStatus::Waiting,
            _ => AgentStatus::Unknown(s),
        })
    }
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
    /// Human reason the agent is blocked when `status == Waiting` (mirrors the
    /// daemon's `waiting_for` / Claude heartbeat `waitingFor`, e.g. "permission
    /// prompt"). No plugin renders it today — the bar derives the alert colour
    /// from `status` alone — but it's part of the schema for consumers (the foam
    /// picker) that do surface the reason.
    #[serde(default)]
    pub waiting_for: Option<String>,
    /// Claude process kind — "interactive" or "bg" (a `run_in_background`
    /// agent). Empty when written by an older daemon → treated as interactive.
    #[serde(default)]
    pub kind: String,
    /// For a background agent, the `session_id` of the session it was forked
    /// from — lets the bar nest bg children under their parent. `None` for
    /// interactive sessions, or when the daemon couldn't resolve the link.
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub zellij_session: Option<String>,
    #[serde(default)]
    pub zellij_pane_id: Option<u32>,
    /// Wall-clock millis of session creation. Stable sort key — agents
    /// appear in creation order regardless of activity. Default 0.
    #[serde(default)]
    pub started_at_ms: i64,
    /// Wall-clock millis of the latest attention-worthy event. 0 = none.
    /// `seen_at_ms` is **not** in the readmodel — it's read separately from
    /// `agent-seen-events/` since seen-state is plugin-owned.
    #[serde(default)]
    pub attention_at_ms: i64,
    /// False once the agent's live heartbeat is gone — the daemon carries it
    /// forward as tracked-but-not-active so the bar keeps showing it until the
    /// user dismisses it. Defaults to `true` so a snapshot from an older daemon
    /// (no field) reads as all-live.
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
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
