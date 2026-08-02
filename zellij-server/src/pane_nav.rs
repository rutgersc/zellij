//! Browser-style cross-session pane jumplist.
//!
//! A single JSON file under the zellij cache dir holds the jumplist + cursor so
//! it spans every session's server process. Written from the focus funnel
//! (`Screen::update_active_pane_ids`); navigated by the `FocusPrevJump` /
//! `FocusNextJump` actions.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::panes::PaneId;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct Entry {
    session: String,
    pane: String, // "terminal_<id>" / "plugin_<id>"
}

#[derive(Serialize, Deserialize, Default)]
struct NavState {
    entries: Vec<Entry>,
    cursor: usize,
}

fn state_path() -> PathBuf {
    zellij_utils::consts::ZELLIJ_CACHE_DIR.join("pane-nav.json")
}

fn load() -> NavState {
    fs::read(state_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn store(state: &NavState) {
    let path = state_path();
    let tmp = path.with_extension("tmp");
    if let Ok(bytes) = serde_json::to_vec(state) {
        let _ = fs::write(&tmp, &bytes).and_then(|_| fs::rename(&tmp, &path));
    }
}

fn spec(pane: PaneId) -> String {
    match pane {
        PaneId::Terminal(id) => format!("terminal_{id}"),
        PaneId::Plugin(id) => format!("plugin_{id}"),
    }
}

fn parse_spec(s: &str) -> Option<PaneId> {
    let (kind, id) = s.split_once('_')?;
    let id: u32 = id.parse().ok()?;
    match kind {
        "terminal" => Some(PaneId::Terminal(id)),
        "plugin" => Some(PaneId::Plugin(id)),
        _ => None,
    }
}

/// Record a genuine focus change, browser-truncating the forward branch. The
/// `entries[cursor] == e` guard also swallows our own jump echo, because `step`
/// pre-positions the cursor at the pane we're about to land on.
pub fn record(session: &str, pane: PaneId) {
    if matches!(pane, PaneId::Plugin(_)) {
        return; // plugin panes (status/tab bars, pickers) are UI furniture, not nav targets
    }
    let e = Entry {
        session: session.to_string(),
        pane: spec(pane),
    };
    let mut s = load();
    if s.entries.get(s.cursor) == Some(&e) {
        return;
    }
    s.entries.truncate(s.cursor + 1);
    s.entries.retain(|x| x != &e); // dedup: re-focusing a pane moves it to the end, never repeats
    s.entries.push(e);
    // cap to the most recent MAX entries (drop oldest)
    const MAX: usize = 15;
    if s.entries.len() > MAX {
        let drop = s.entries.len() - MAX;
        s.entries.drain(0..drop);
    }
    s.cursor = s.entries.len() - 1;
    store(&s);
}

/// Step the cursor one place; returns the (session, pane) to focus, or None at
/// an edge.
pub fn step(forward: bool) -> Option<(String, PaneId)> {
    let mut s = load();
    let next = s.cursor as isize + if forward { 1 } else { -1 };
    if next < 0 || next as usize >= s.entries.len() {
        return None;
    }
    s.cursor = next as usize;
    let e = s.entries[s.cursor].clone();
    store(&s);
    parse_spec(&e.pane).map(|p| (e.session, p))
}
