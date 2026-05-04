//! Project the agent read-model at `~/.claude/readmodel/agents.json` into
//! per-tab tint flags for the current zellij session. The daemon that
//! writes the snapshot lives outside this repo.

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

/// What we tint a tab with, post attention check.
/// `Attention` wins over `Busy` when a tab has multiple agents.
/// Idle alone contributes nothing — no tint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TabFlag {
    Busy,
    Attention,
}

/// One agent's snapshot data, projected to the current zellij session.
/// `claude_id` is the key for `seen_at` (agent session UUID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneAgent {
    pub claude_id: String,
    pub status: AgentStatus,
    pub attention_at_ms: i64,
}

#[derive(Deserialize)]
struct Snapshot {
    agents: Vec<AgentEntry>,
}

#[derive(Deserialize)]
struct AgentEntry {
    session_id: String,
    #[serde(default)]
    status: Option<AgentStatus>,
    #[serde(default)]
    zellij_session: Option<String>,
    #[serde(default)]
    zellij_pane_id: Option<u32>,
    #[serde(default)]
    attention_at_ms: i64,
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

/// Read the snapshot and project every agent in the current zellij session
/// down to its zellij pane. Multiple agents on the same pane: busy beats idle,
/// max attention_at_ms wins.
pub fn project_for_session(
    snapshot_path: &Path,
    current_session: &str,
) -> HashMap<u32, PaneAgent> {
    let Ok(content) = fs::read_to_string(snapshot_path) else { return HashMap::new() };
    let Ok(snap) = serde_json::from_str::<Snapshot>(&content) else { return HashMap::new() };
    let mut out: HashMap<u32, PaneAgent> = HashMap::new();
    for a in snap.agents {
        let (Some(status), Some(session), Some(pane_id)) =
            (a.status, a.zellij_session.as_deref(), a.zellij_pane_id)
        else { continue };
        if session != current_session {
            continue;
        }
        let candidate = PaneAgent {
            claude_id: a.session_id,
            status,
            attention_at_ms: a.attention_at_ms,
        };
        match out.get_mut(&pane_id) {
            None => {
                out.insert(pane_id, candidate);
            },
            Some(existing) => {
                if candidate.status == AgentStatus::Busy {
                    existing.status = AgentStatus::Busy;
                }
                if candidate.attention_at_ms > existing.attention_at_ms {
                    existing.attention_at_ms = candidate.attention_at_ms;
                    existing.claude_id = candidate.claude_id;
                }
            },
        }
    }
    out
}

/// Compute the per-tab flag for tinting. Side effect: bumps `seen_at` for
/// every claude session whose pane is in the active tab — that's the
/// presence-based "mark seen" rule, identical to agent-bar's.
pub fn compute_tab_flags(
    pane_agents: &HashMap<u32, PaneAgent>,
    pane_manifest: &PaneManifest,
    active_tab_idx: Option<usize>,
    seen_at: &mut HashMap<String, i64>,
) -> HashMap<usize, TabFlag> {
    let mut out: HashMap<usize, TabFlag> = HashMap::new();
    for (tab_idx, panes) in &pane_manifest.panes {
        let in_active = active_tab_idx == Some(*tab_idx);
        for pane in panes {
            if pane.is_plugin {
                continue;
            }
            let Some(pa) = pane_agents.get(&pane.id) else { continue };
            let needs_attention = if in_active {
                seen_at.insert(pa.claude_id.clone(), pa.attention_at_ms);
                false
            } else if pa.attention_at_ms == 0 {
                false
            } else {
                let last = seen_at.get(&pa.claude_id).copied().unwrap_or(0);
                pa.attention_at_ms > last
            };
            let flag = if needs_attention {
                TabFlag::Attention
            } else if pa.status == AgentStatus::Busy {
                TabFlag::Busy
            } else {
                continue; // idle without unseen attention contributes nothing
            };
            let entry = out.entry(*tab_idx).or_insert(flag);
            if flag == TabFlag::Attention {
                *entry = TabFlag::Attention;
            }
        }
    }
    out
}
