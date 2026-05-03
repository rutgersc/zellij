//! Read-side of the agent snapshot at `~/.claude/readmodel/agents.json`.
//! Errors propagate as `ReadResult` variants so the UI can surface what
//! actually happened — `agent-bar` is a debugging surface as much as a
//! display, and silently swallowing failures is what made it confusing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Busy,
    Idle,
}

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
}

#[derive(Debug, Clone)]
pub enum ReadResult {
    Ok(Vec<Agent>),
    Missing,
    Unreadable(String),
    ParseError(String),
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    agents: Vec<Agent>,
}

/// `host_root` is what to pass to `change_host_folder` (preopens it as
/// `/host`); `wasi_path` is what to read from inside the plugin sandbox.
/// They must agree, which is why they're returned together.
pub struct SnapshotLocation {
    pub host_root: PathBuf,
    pub wasi_path: PathBuf,
}

/// Preopen `$HOME` as the WASI host root. On Windows `change_host_folder("/")`
/// resolves to whatever drive the zellij server is running on (e.g. F:\), so
/// passing an absolute home path is the only way to reach C:\Users\... reliably.
pub fn locate_snapshot(env: &BTreeMap<String, String>) -> Option<SnapshotLocation> {
    let home = env.get("HOME").or_else(|| env.get("USERPROFILE"))?;
    Some(SnapshotLocation {
        host_root: PathBuf::from(home),
        wasi_path: PathBuf::from("/host/.claude/readmodel/agents.json"),
    })
}

pub fn read(path: &Path) -> ReadResult {
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
