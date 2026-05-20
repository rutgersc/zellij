//! Project the readmodel into per-tab tint flags for the current zellij
//! session. Schema and disk-reads are in the shared `agent-readmodel`
//! crate; this module is just the projection.

use std::collections::HashMap;
use std::path::Path;

use agent_readmodel::{AgentStatus, ReadResult, read_readmodel};
use zellij_tile::prelude::PaneManifest;

pub use agent_readmodel::{locate_snapshot, read_seen_events};

/// What we tint a tab with, post attention check.
/// `Attention` wins over `Busy` when a tab has multiple agents.
/// Idle alone contributes nothing — no tint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TabFlag {
    Busy,
    Attention,
}

/// One agent's snapshot data, projected to the current zellij session.
/// `claude_id` is the key for the seen-overlay/disk maps (agent session UUID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneAgent {
    pub claude_id: String,
    pub status: AgentStatus,
    pub attention_at_ms: i64,
}

/// Read the readmodel and project every agent in the current zellij session
/// down to its zellij pane. Multiple agents on the same pane: busy beats idle,
/// max attention_at_ms wins.
pub fn project_for_session(
    snapshot_path: &Path,
    current_session: &str,
) -> HashMap<u32, PaneAgent> {
    let agents = match read_readmodel(snapshot_path) {
        ReadResult::Ok(a) => a,
        _ => return HashMap::new(),
    };
    let mut out: HashMap<u32, PaneAgent> = HashMap::new();
    for a in agents {
        let Some(session) = a.zellij_session.as_deref() else { continue };
        let Some(pane_id) = a.zellij_pane_id else { continue };
        if session != current_session {
            continue;
        }
        let candidate = PaneAgent {
            claude_id: a.session_id,
            status: a.status,
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

/// Compute per-tab tint flags. compact-bar is a read-only consumer of
/// `agent-seen-events/` — mark-seen events are written only by
/// agent-bar's click/Enter paths. So we simply project: if any pane on a
/// tab hosts an agent whose `attention_at_ms > seen_at_ms`, the tab is
/// flagged Attention. Busy is the fallback when no attention is unseen.
/// Active-tab rendering ignores the flag (uses the theme's selected
/// ribbon style), so we don't try to suppress flags for the active tab.
pub fn compute_tab_flags(
    pane_agents: &HashMap<u32, PaneAgent>,
    pane_manifest: &PaneManifest,
    _active_tab_idx: Option<usize>,
    seen: &HashMap<String, i64>,
) -> HashMap<usize, TabFlag> {
    let mut out: HashMap<usize, TabFlag> = HashMap::new();
    for (tab_idx, panes) in &pane_manifest.panes {
        for pane in panes {
            if pane.is_plugin {
                continue;
            }
            let Some(pa) = pane_agents.get(&pane.id) else { continue };
            let seen_at = seen.get(&pa.claude_id).copied().unwrap_or(0);
            let unseen = pa.attention_at_ms > 0 && pa.attention_at_ms > seen_at;
            let flag = if unseen {
                TabFlag::Attention
            } else if pa.status == AgentStatus::Busy {
                TabFlag::Busy
            } else {
                continue;
            };
            let entry = out.entry(*tab_idx).or_insert(flag);
            if flag == TabFlag::Attention {
                *entry = TabFlag::Attention;
            }
        }
    }
    out
}
