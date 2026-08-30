//! Cross-session tab jump *tree* — vim's undo tree, applied to navigation.
//!
//! A single JSON file under the zellij cache dir holds the tree + cursor so it
//! spans every session's server process. Written from the focus funnel
//! (`Screen::update_active_pane_ids`); navigated by the `FocusPrevJump` /
//! `FocusNextJump` actions.
//!
//! Nodes are tabs, not panes. Moving focus between panes of one tab is
//! navigation *within* a context, not a jump between contexts — recording it
//! buried the deliberate jumps under directional-focus noise.
//!
//! A browser history truncates the forward branch the moment you go back and
//! then somewhere new, so the trail you backed out of is lost. Here that trail
//! stays as a sibling branch, and a node is a *visit*: the same tab can sit at
//! several nodes, exactly as one buffer state can in vim's undo tree.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One live tab of the recording session: (stable id, display position, name).
pub type TabSnapshot = (usize, usize, String);

/// How many visits the tree keeps. Deliberately small: this is a jump history
/// you read at a glance, not an archive.
const MAX_NODES: usize = 10;

#[derive(Serialize, Deserialize, Clone)]
struct Node {
    /// Stable across pruning, unlike a Vec index, so `parent` / `last_child` /
    /// `cursor` never need remapping when a node is dropped.
    id: usize,
    session: String,
    tab_id: usize,
    /// Refreshed from the snapshot on every record, because a cross-session
    /// jump goes through `ConnectToSession`, which addresses tabs by position
    /// and cannot resolve a foreign session's ids.
    tab_position: usize,
    tab_name: String,
    parent: Option<usize>,
    /// The child last descended into. `FocusNextJump` follows it, so stepping
    /// back and forward again returns to where you were rather than to whatever
    /// branch happens to be newest.
    last_child: Option<usize>,
    at: u64,
}

impl Node {
    fn is(&self, session: &str, tab_id: usize) -> bool {
        self.session == session && self.tab_id == tab_id
    }
}

#[derive(Serialize, Deserialize, Default)]
struct NavState {
    nodes: Vec<Node>,
    cursor: usize,
    next_id: usize,
}

impl NavState {
    fn node(&self, id: usize) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    fn node_mut(&mut self, id: usize) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }
    fn children_of(&self, id: usize) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(move |n| n.parent == Some(id))
    }
    fn set_cursor(&mut self, id: usize) {
        if let Some(from) = self.node(self.cursor).map(|n| n.id) {
            if self.node(id).and_then(|n| n.parent) == Some(from) {
                if let Some(parent) = self.node_mut(from) {
                    parent.last_child = Some(id);
                }
            }
        }
        self.cursor = id;
    }
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

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Drop a node, splicing its children onto its parent so the branch below it
/// stays reachable. A root's children become roots.
fn remove_node(state: &mut NavState, id: usize) {
    let Some(parent) = state.node(id).map(|n| n.parent) else {
        return;
    };
    state
        .nodes
        .iter_mut()
        .filter(|n| n.parent == Some(id))
        .for_each(|n| n.parent = parent);
    state
        .nodes
        .iter_mut()
        .filter(|n| n.last_child == Some(id))
        .for_each(|n| n.last_child = None);
    state.nodes.retain(|n| n.id != id);
    if state.cursor == id {
        state.cursor = parent
            .or_else(|| state.nodes.last().map(|n| n.id))
            .unwrap_or(0);
    }
}

/// Re-sync this session's nodes against its live tabs: drop the closed ones,
/// refresh position + name on the rest. Reports whether anything moved.
fn refresh_session(state: &mut NavState, session: &str, tabs: &[TabSnapshot]) -> bool {
    let closed: Vec<usize> = state
        .nodes
        .iter()
        .filter(|n| n.session == session && !tabs.iter().any(|(id, _, _)| *id == n.tab_id))
        .map(|n| n.id)
        .collect();
    let mut changed = !closed.is_empty();
    closed.into_iter().for_each(|id| remove_node(state, id));

    state
        .nodes
        .iter_mut()
        .filter(|n| n.session == session)
        .for_each(|n| {
            if let Some((_, position, name)) = tabs.iter().find(|(id, _, _)| *id == n.tab_id) {
                if n.tab_position != *position || n.tab_name != *name {
                    n.tab_position = *position;
                    n.tab_name = name.clone();
                    changed = true;
                }
            }
        });
    changed
}

/// Trim to `MAX_NODES` by dropping the oldest visits. Only the cursor is spared
/// — the node you are standing on outranks any age rule.
///
/// Dropping the *oldest* rather than the oldest leaf is what makes the cap hold:
/// navigation is overwhelmingly linear, and a chain has exactly one leaf, which
/// is usually the cursor. A leaf-only rule therefore had nothing it was allowed
/// to drop and the tree grew without bound.
fn prune(state: &mut NavState) {
    while state.nodes.len() > MAX_NODES {
        // Ids are monotonic, so they order nodes chronologically without
        // depending on `at`, whose one-second resolution ties constantly.
        let oldest = state
            .nodes
            .iter()
            .map(|n| n.id)
            .filter(|id| *id != state.cursor)
            .min();
        match oldest {
            // Splicing (see `remove_node`) keeps the rest of the chain hanging
            // together: the second-oldest node simply becomes a root.
            Some(id) => remove_node(state, id),
            None => break,
        }
    }
}

/// What folding one visit into the tree did.
enum Visit {
    /// It is in the tree. `changed` is false for the echo of a move `step` (or
    /// the picker) just made, which the funnel reports back a moment later.
    Folded { changed: bool },
    /// The snapshot could not account for the visit, so the tree was left alone.
    Refused,
}

/// Reports whether the visit made it into the tree. `false` means the caller
/// must not remember this visit as done — nothing was recorded, and only
/// asking again can fix that.
#[must_use]
pub fn record(session: &str, tab_id: usize, tabs: &[TabSnapshot]) -> bool {
    let mut state = load();
    match apply(&mut state, session, tab_id, tabs) {
        Visit::Folded { changed } => {
            // The funnel fires on every session-state report, so most calls are
            // a no-op echo; writing regardless would churn the file all day.
            if changed {
                store(&state);
            }
            true
        },
        Visit::Refused => false,
    }
}

/// Record a tab change: every hop gets its own node, hanging off the cursor.
/// Revisiting a tab the tree already holds is still a visit — a back/forward
/// stack that silently swallowed the hops you make between two tabs is exactly
/// what stopped the tree from matching what you actually did.
///
/// The one hop that is *not* navigation is landing on the tab the cursor already
/// names: that is the focus funnel echoing a move `step` (or the picker) just
/// made, so it must stay a no-op or every back would append a node.
fn apply(state: &mut NavState, session: &str, tab_id: usize, tabs: &[TabSnapshot]) -> Visit {
    // A snapshot that does not hold the client's own active tab is a torn view
    // of the session — mid tab-close, mid layout-apply. It has to be refused
    // before `refresh_session` touches anything: that reads every tab missing
    // from the snapshot as closed and drops its nodes, and the visit itself
    // cannot be added in their place, so the session leaves the tree with
    // nothing to show for it.
    let Some((_, tab_position, tab_name)) = tabs.iter().find(|(id, _, _)| *id == tab_id) else {
        return Visit::Refused;
    };
    let (tab_position, tab_name) = (*tab_position, tab_name.clone());

    let refreshed = refresh_session(state, session, tabs);
    if state.node(state.cursor).is_some_and(|n| n.is(session, tab_id)) {
        return Visit::Folded { changed: refreshed };
    }

    let id = state.next_id;
    state.next_id += 1;
    let parent = state.node(state.cursor).map(|n| n.id);
    state.nodes.push(Node {
        id,
        session: session.to_string(),
        tab_id,
        tab_position,
        tab_name,
        parent,
        last_child: None,
        at: now(),
    });
    state.set_cursor(id);
    prune(state);
    Visit::Folded { changed: true }
}

/// Step the cursor one place along the tree; returns the (session, tab id, tab
/// position) to focus, or None at an edge. Forward follows the branch you were
/// last on, falling back to the newest child.
pub fn step(forward: bool) -> Option<(String, usize, usize)> {
    let mut state = load();
    let landed = advance(&mut state, forward)?;
    store(&state);
    Some(landed)
}

fn advance(state: &mut NavState, forward: bool) -> Option<(String, usize, usize)> {
    let current = state.node(state.cursor)?;

    let next = if forward {
        current
            .last_child
            .filter(|id| state.node(*id).is_some())
            .or_else(|| {
                state
                    .children_of(state.cursor)
                    .max_by_key(|n| n.id)
                    .map(|n| n.id)
            })
    } else {
        current.parent
    }?;

    state.set_cursor(next);
    let node = state.node(next)?;
    Some((node.session.clone(), node.tab_id, node.tab_position))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "sess";

    fn tabs(ids: &[usize]) -> Vec<TabSnapshot> {
        ids.iter()
            .enumerate()
            .map(|(position, id)| (*id, position, format!("tab{id}")))
            .collect()
    }

    fn go(state: &mut NavState, tab_id: usize, live: &[TabSnapshot]) {
        apply(state, S, tab_id, live);
    }

    fn cursor_tab(state: &NavState) -> usize {
        state.node(state.cursor).unwrap().tab_id
    }

    fn changed(visit: Visit) -> bool {
        matches!(visit, Visit::Folded { changed: true })
    }

    #[test]
    fn linear_visits_form_a_chain() {
        let live = tabs(&[1, 2, 3]);
        let mut state = NavState::default();
        [1, 2, 3].iter().for_each(|t| go(&mut state, *t, &live));

        assert_eq!(state.nodes.len(), 3);
        assert_eq!(state.nodes[0].parent, None);
        assert_eq!(state.nodes[1].parent, Some(state.nodes[0].id));
        assert_eq!(state.nodes[2].parent, Some(state.nodes[1].id));
    }

    #[test]
    fn backing_out_and_going_elsewhere_branches_without_losing_the_old_limb() {
        let live = tabs(&[1, 2, 3, 4]);
        let mut state = NavState::default();
        [1, 2, 3].iter().for_each(|t| go(&mut state, *t, &live));

        advance(&mut state, false).unwrap();
        advance(&mut state, false).unwrap();
        assert_eq!(cursor_tab(&state), 1);

        go(&mut state, 4, &live);

        // The 2 -> 3 trail a browser history would have truncated is still here.
        assert_eq!(state.nodes.len(), 4);
        assert!(state.nodes.iter().any(|n| n.tab_id == 3));
        let root = state.nodes.iter().find(|n| n.tab_id == 1).unwrap().id;
        let mut children: Vec<usize> = state.children_of(root).map(|n| n.tab_id).collect();
        children.sort();
        assert_eq!(children, vec![2, 4]);
    }

    #[test]
    fn ping_pong_between_two_tabs_records_every_hop() {
        let live = tabs(&[1, 2]);
        let mut state = NavState::default();
        [1, 2, 1, 2, 1].iter().for_each(|t| go(&mut state, *t, &live));

        assert_eq!(state.nodes.len(), 5);
        assert_eq!(
            state.nodes.iter().map(|n| n.tab_id).collect::<Vec<_>>(),
            vec![1, 2, 1, 2, 1]
        );
    }

    #[test]
    fn stepping_back_does_not_append_the_tab_it_lands_on() {
        let live = tabs(&[1, 2]);
        let mut state = NavState::default();
        [1, 2].iter().for_each(|t| go(&mut state, *t, &live));

        advance(&mut state, false).unwrap();
        // The focus funnel echoes the move `advance` just made.
        assert!(!changed(apply(&mut state, S, 1, &live)));
        assert_eq!(state.nodes.len(), 2);
    }

    #[test]
    fn a_snapshot_missing_the_visited_tab_leaves_the_tree_alone() {
        let live = tabs(&[1, 2]);
        let mut state = NavState::default();
        [1, 2].iter().for_each(|t| go(&mut state, *t, &live));

        // What the funnel reports mid tab-close: an active tab that the snapshot
        // of its own session does not contain. Acting on it used to prune every
        // node of the session and add nothing back.
        assert!(matches!(apply(&mut state, S, 2, &[]), Visit::Refused));

        assert_eq!(state.nodes.len(), 2);
        assert_eq!(cursor_tab(&state), 2);
    }

    #[test]
    fn the_history_keeps_only_the_newest_visits() {
        let live = tabs(&[1, 2]);
        let mut state = NavState::default();
        (0..MAX_NODES * 2).for_each(|i| go(&mut state, 1 + i % 2, &live));

        assert_eq!(state.nodes.len(), MAX_NODES);
        // The survivors are the last MAX_NODES ids handed out, and the oldest of
        // them is a root — `remove_node` spliced the chain back together.
        let ids: Vec<usize> = state.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, (MAX_NODES..MAX_NODES * 2).collect::<Vec<_>>());
        assert_eq!(state.nodes[0].parent, None);
    }

    #[test]
    fn pruning_spares_the_cursor_even_when_it_is_the_oldest_node() {
        // Built by hand: `apply` always prunes with the cursor on the node it
        // just pushed, so the guard is only reachable from a state loaded off
        // disk after the user stepped back.
        let mut state = NavState::default();
        (0..MAX_NODES + 3).for_each(|i| {
            state.nodes.push(Node {
                id: i,
                session: S.to_string(),
                tab_id: i,
                tab_position: i,
                tab_name: format!("tab{i}"),
                parent: (i > 0).then(|| i - 1),
                last_child: None,
                at: 0,
            })
        });
        state.next_id = MAX_NODES + 3;
        state.cursor = 0;

        prune(&mut state);

        assert_eq!(state.nodes.len(), MAX_NODES);
        assert!(state.node(0).is_some());
    }

    #[test]
    fn forward_follows_the_branch_last_taken() {
        let live = tabs(&[1, 2, 3, 4]);
        let mut state = NavState::default();
        [1, 2, 3].iter().for_each(|t| go(&mut state, *t, &live));
        advance(&mut state, false).unwrap();
        advance(&mut state, false).unwrap();
        go(&mut state, 4, &live); // branch; tab 4 is now the last-taken child of tab 1

        advance(&mut state, false).unwrap();
        assert_eq!(cursor_tab(&state), 1);
        advance(&mut state, true).unwrap();
        assert_eq!(cursor_tab(&state), 4);
    }

    #[test]
    fn closing_a_tab_splices_its_children_onto_the_grandparent() {
        let live = tabs(&[1, 2, 3]);
        let mut state = NavState::default();
        [1, 2, 3].iter().for_each(|t| go(&mut state, *t, &live));

        go(&mut state, 3, &tabs(&[1, 3])); // tab 2 closed

        assert_eq!(state.nodes.len(), 2);
        let root = state.nodes.iter().find(|n| n.tab_id == 1).unwrap().id;
        let orphan = state.nodes.iter().find(|n| n.tab_id == 3).unwrap();
        assert_eq!(orphan.parent, Some(root));
    }

    #[test]
    fn only_a_real_move_reports_a_change_worth_writing() {
        let live = tabs(&[1, 2]);
        let mut state = NavState::default();

        assert!(
            changed(apply(&mut state, S, 1, &live)),
            "first visit creates the root"
        );
        assert!(
            !changed(apply(&mut state, S, 1, &live)),
            "echo of the cursor changes nothing"
        );
        assert!(
            changed(apply(&mut state, S, 2, &live)),
            "a genuine move changes the tree"
        );
    }

    #[test]
    fn stepping_back_from_the_root_is_a_no_op() {
        let live = tabs(&[1]);
        let mut state = NavState::default();
        go(&mut state, 1, &live);

        assert!(advance(&mut state, false).is_none());
        assert_eq!(cursor_tab(&state), 1);
    }
}
