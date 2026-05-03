//! Project the agent read-model at `~/.claude/readmodel/agents.json` into
//! `tab_position → AgentStatus` for the current zellij session. The daemon
//! that writes the snapshot lives outside this repo.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zellij_tile::prelude::PaneManifest;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Busy,
    Idle,
}

#[derive(Deserialize)]
struct Snapshot {
    agents: Vec<AgentEntry>,
}

#[derive(Deserialize)]
struct AgentEntry {
    #[serde(default)]
    status: Option<AgentStatus>,
    #[serde(default)]
    zellij_session: Option<String>,
    #[serde(default)]
    zellij_pane_id: Option<u32>,
}

/// `host_root` is what to pass to `change_host_folder` (preopens it as
/// `/host`); `wasi_path` is what to read from inside the plugin sandbox.
/// They must agree, which is why they're returned together.
pub struct SnapshotLocation {
    pub host_root: PathBuf,
    pub wasi_path: PathBuf,
}

/// Preopen `$HOME` as the WASI host root. On Windows `change_host_folder("/")`
/// resolves to whatever drive the zellij server is running on, so passing an
/// absolute home path is the only way to reach C:\Users\... reliably.
pub fn locate_snapshot(env: &BTreeMap<String, String>) -> Option<SnapshotLocation> {
    let home = env.get("HOME").or_else(|| env.get("USERPROFILE"))?;
    Some(SnapshotLocation {
        host_root: PathBuf::from(home),
        wasi_path: PathBuf::from("/host/.claude/readmodel/agents.json"),
    })
}

pub fn project_for_session(snapshot_path: &Path, current_session: &str) -> HashMap<u32, AgentStatus> {
    let Ok(content) = fs::read_to_string(snapshot_path) else { return HashMap::new() };
    let Ok(snap) = serde_json::from_str::<Snapshot>(&content) else { return HashMap::new() };
    let mut out = HashMap::new();
    for a in snap.agents {
        let (Some(status), Some(session), Some(pane_id)) =
            (a.status, a.zellij_session.as_deref(), a.zellij_pane_id)
        else { continue };
        if session != current_session {
            continue;
        }
        let entry = out.entry(pane_id).or_insert(status);
        if status == AgentStatus::Busy {
            *entry = AgentStatus::Busy;
        }
    }
    out
}

pub fn tabs_with_agents(
    pane_to_agent: &HashMap<u32, AgentStatus>,
    pane_manifest: &PaneManifest,
) -> HashMap<usize, AgentStatus> {
    let mut out = HashMap::new();
    for (tab_idx, panes) in &pane_manifest.panes {
        for pane in panes {
            if pane.is_plugin {
                continue;
            }
            if let Some(status) = pane_to_agent.get(&pane.id) {
                let entry = out.entry(*tab_idx).or_insert(*status);
                if *status == AgentStatus::Busy {
                    *entry = AgentStatus::Busy;
                }
            }
        }
    }
    out
}
