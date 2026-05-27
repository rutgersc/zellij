//! Vertical agent panel — one column on the side of a tab, showing every
//! running Claude agent across every zellij session. Polls
//! `~/.claude/readmodel/agents.json` every `POLL_SECS`.
//!
//! Layout: each agent's `name` is word-wrapped to the pane width (1..=4
//! rows). Agents are grouped under the zellij session they belong to;
//! groups (and agents within them) are ordered by `started_at_ms` desc, so
//! the newest activity stays at the top. Agents without a `zellij_session`
//! collect in a "sessionless" group pinned to the bottom.
//!
//! Status carries through text fg (green=busy, white=idle, amber=waiting).
//! `needs_attention` paints the agent's rows orange-bg. `✗` after the
//! session header marks agents missing a `zellij_pane_id`.
//!
//! Click any row of an agent → `mux focus-agent <session_id>`. Up/Down
//! moves the keyboard cursor across agents, Enter activates, Esc returns
//! focus to the previous pane.
//!
//! Every distinguishable load state — awaiting permission, no $HOME in
//! env, file missing/unreadable/unparseable, empty snapshot — has its own
//! visible rendering. The panel is also the diagnostics surface.

mod agents;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;
use zellij_tile_utils::style;

use crate::agents::{Agent, AgentStatus, ReadResult};

const POLL_SECS: f64 = 1.5;
/// Hard cap on rows used to render a single agent's name. Past this we
/// truncate with `…` — the full name is still on the snapshot.
const MAX_NAME_LINES: usize = 4;
/// Single-col left margin from the pane border. Panel-focus is now signaled
/// by zellij's own pane border highlight (the layout uses bordered panes
/// for resize affordance), so the in-content focus stripe was removed.
const OUTER_PAD: usize = 1;
/// Within an agent row, after `OUTER_PAD`, two more cols for [`>` row
/// marker (or `✗` if unrouted), space] before the wrapped name.
const AGENT_INDENT: usize = 2;
const SESSIONLESS_LABEL: &str = "sessionless";

// Agent-state palette — shared vocabulary across agent-bar, compact-bar's
// tab tints, and (eventually) the mux session picker:
//   busy        → green fg, no bg              (working, no user action needed)
//   waiting     → yellow bg + on-colour fg     (blocked on user)
//   attention   → yellow bg + on-colour fg     (unseen attention event)
//   active-pane → green bg + on-colour fg      (THIS agent owns the zellij
//                                               pane the user is currently in)
//   idle/seen   → text fg, no bg               (neutral)
// Active-pane mirrors the active-tab pattern (green bg + on-colour fg) so the
// agent you're "inside" reads as the visual peer of your active tab.
//
// All status colours are resolved from `mode_info.style.colors` so a
// ChangeTheme action updates everything together — green/yellow/on-colour
// come straight from `ribbon_selected` and `exit_code_error` so they match
// what the theme uses for its own active-tab and error styles. Only the
// red unrouted marker and the structural glyphs are hardcoded.
const UNROUTED: &str = "\u{2717}";

/// Theme-driven colours for every agent state. Built once per render from
/// `mode_info.style.colors` so a ChangeTheme keystroke updates the whole
/// panel atomically.
#[derive(Copy, Clone)]
struct AgentColors {
    /// The "green" — bg for active-pane, fg for busy.
    green: PaletteColor,
    /// The "yellow" — bg for waiting / needs-attention.
    yellow: PaletteColor,
    /// Foreground to use on top of `green` or `yellow`. Comes from the
    /// theme's ribbon_selected.base so it matches whatever fg the user's
    /// theme already uses on its active-tab green.
    on_color: PaletteColor,
    /// Neutral text colour, for idle agents and headers.
    text: PaletteColor,
    /// Bar background — the colour of un-tinted rows.
    bar_bg: PaletteColor,
    /// Header background — distinct shade so groups visually separate.
    header_bg: PaletteColor,
    /// Panel-active indicator (the leftmost `▌` block).
    accent: PaletteColor,
    /// Selection cursor pair, picked based on inferred theme hue.
    selected_bg: PaletteColor,
    selected_fg: PaletteColor,
    /// Theme's red — used for the unrouted `✗` marker and for fatal
    /// diagnostic text (missing snapshot, parse error, etc).
    error: PaletteColor,
}

impl AgentColors {
    fn from_palette(p: &Styling) -> Self {
        Self {
            green: p.ribbon_selected.background,
            yellow: p.exit_code_error.emphasis_0,
            on_color: p.ribbon_selected.base,
            text: p.text_unselected.base,
            bar_bg: p.text_unselected.background,
            // text_selected.background is a distinct shade in most themes
            // (e.g. Dracula's #44475A vs the panel's #282A36) — gives us a
            // header strip without hardcoding a grey.
            header_bg: p.text_selected.background,
            accent: p.frame_selected.base,
            // The canonical "highlighted list item" pair. Adapts per
            // theme — ayu-light gives strong blue, dracula-custom gives a
            // subtle lifted-grey, catppuccin-latte gives soft grey, etc.
            // Edge case: upstream zellij:dracula sets this equal to the
            // bar bg (selection invisible). That's a theme defect; using a
            // different theme is the fix.
            selected_bg: p.list_selected.background,
            selected_fg: p.list_selected.base,
            // The theme's "error red". Used for unrouted (✗) and for fatal
            // diagnostic states. Every theme defines this — it's how zellij
            // paints exit-code-nonzero indicators elsewhere.
            error: p.exit_code_error.base,
        }
    }
}

#[derive(Default)]
enum LoadState {
    #[default]
    AwaitingPermission,
    NoHomeInEnv,
    Polling {
        path: PathBuf,
        last: Option<ReadResult>,
    },
}

#[derive(Default)]
struct State {
    load: LoadState,
    mode_info: ModeInfo,
    /// Click hit-test: rendered_row → session_id. Rebuilt every render.
    row_ranges: Vec<(usize, String)>,
    /// PaneManifest + active tab let us decide which agents are "here".
    pane_manifest: PaneManifest,
    active_tab_idx: Option<usize>,
    /// Latest scan of `agent-seen-events/` keyed by claude_id.
    seen_disk: HashMap<String, i64>,
    /// Optimistic mark-seen overlay; effective = max(disk, overlay).
    seen_overlay: HashMap<String, i64>,
    seen_events_dir: Option<PathBuf>,
    /// Keyboard cursor — agent index in display order (groups → agents).
    selected_idx: Option<usize>,
    own_plugin_id: Option<u32>,
    was_focused: bool,
    last_external_focused_pane: Option<u32>,
    /// Row scroll offset into the rendered line list.
    scroll_offset: usize,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        // Stay selectable: zellij #4749 — non-selectable plugins can't
        // receive the y/n permission prompt grant. Also lets the panel
        // join the keyboard pane cycle for Up/Down/Enter navigation.
        subscribe(&[
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::Timer,
            EventType::Mouse,
            EventType::Key,
            EventType::PermissionRequestResult,
            EventType::RunCommandResult,
        ]);
        self.own_plugin_id = Some(get_plugin_ids().plugin_id);
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::FullHdAccess,
            PermissionType::ReadSessionEnvironmentVariables,
            PermissionType::RunCommands,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(_) => {
                if matches!(self.load, LoadState::AwaitingPermission) {
                    let env = get_session_environment_variables();
                    self.load = match agents::locate_snapshot(&env) {
                        Some(loc) => {
                            change_host_folder(loc.host_root);
                            self.seen_events_dir = Some(PathBuf::from(
                                "/host/.claude/custom-state/agent-seen-events",
                            ));
                            self.refresh_seen_disk();
                            // Synchronous initial read — first render
                            // already has agents instead of "reading…".
                            let initial = agents::read(&loc.wasi_path);
                            LoadState::Polling {
                                path: loc.wasi_path,
                                last: Some(initial),
                            }
                        },
                        None => LoadState::NoHomeInEnv,
                    };
                    set_timeout(POLL_SECS);
                    return true;
                }
                false
            },
            Event::ModeUpdate(mode_info) => {
                let changed = self.mode_info != mode_info;
                self.mode_info = mode_info;
                changed
            },
            Event::TabUpdate(tabs) => {
                let new_idx = tabs.iter().find(|t| t.active).map(|t| t.position);
                let changed = self.active_tab_idx != new_idx;
                self.active_tab_idx = new_idx;
                changed
            },
            Event::PaneUpdate(manifest) => {
                let changed = self.pane_manifest != manifest;
                self.pane_manifest = manifest;
                let now_focused = self.am_focused();
                if !now_focused {
                    if let Some(pid) = self.focused_external_pane_id() {
                        self.last_external_focused_pane = Some(pid);
                    }
                }
                if now_focused != self.was_focused {
                    self.selected_idx = if now_focused {
                        self.initial_selection()
                    } else {
                        None
                    };
                    self.was_focused = now_focused;
                    return true;
                }
                changed
            },
            Event::Timer(_) => {
                let mut changed = if let LoadState::Polling { path, last } = &mut self.load {
                    let new = agents::read(path);
                    let changed = !same_result(last.as_ref(), &new);
                    *last = Some(new);
                    changed
                } else {
                    false
                };
                if self.refresh_seen_disk() {
                    changed = true;
                }
                set_timeout(POLL_SECS);
                changed
            },
            Event::Mouse(Mouse::LeftClick(line, _col)) => {
                let row = if line < 0 { return false } else { line as usize };
                let hit = self
                    .row_ranges
                    .iter()
                    .find(|(r, _)| *r == row)
                    .map(|(_, sid)| sid.clone());
                if let Some(sid) = hit {
                    self.acknowledge_agent(&sid);
                    let _ = self.dispatch_focus_agent(&sid);
                    if let Some(idx) = self
                        .display_order()
                        .iter()
                        .position(|a| a.session_id == sid)
                    {
                        self.selected_idx = Some(idx);
                    }
                    return true;
                }
                false
            },
            Event::Key(key) => {
                let agents = self.display_order();
                let len = agents.len();
                let no_mods = key.has_no_modifiers();
                let go_up = matches!(key.bare_key, BareKey::Up | BareKey::Char('k')) && no_mods;
                let go_down = matches!(key.bare_key, BareKey::Down | BareKey::Char('j')) && no_mods;
                let go_first = matches!(key.bare_key, BareKey::Home | BareKey::Char('g')) && no_mods;
                let go_last = matches!(key.bare_key, BareKey::End | BareKey::Char('G')) && no_mods;
                let activate = matches!(key.bare_key, BareKey::Enter) && no_mods;
                let cancel = matches!(key.bare_key, BareKey::Esc | BareKey::Char('q')) && no_mods;
                if go_up && len > 0 {
                    self.selected_idx = Some(match self.selected_idx {
                        Some(i) if i > 0 => i - 1,
                        _ => 0,
                    });
                    return true;
                }
                if go_down && len > 0 {
                    self.selected_idx = Some(match self.selected_idx {
                        Some(i) => (i + 1).min(len - 1),
                        None => 0,
                    });
                    return true;
                }
                if go_first && len > 0 {
                    self.selected_idx = Some(0);
                    return true;
                }
                if go_last && len > 0 {
                    self.selected_idx = Some(len - 1);
                    return true;
                }
                if activate {
                    if let Some(sid) = self
                        .selected_idx
                        .and_then(|i| agents.get(i))
                        .map(|a| a.session_id.clone())
                    {
                        self.acknowledge_agent(&sid);
                        let _ = self.dispatch_focus_agent(&sid);
                        return true;
                    }
                    return false;
                }
                if cancel {
                    // Layout invariant: agent-bar is the leftmost pane in
                    // every tab, with the shell directly to its right
                    // (default_tab_template in foam/layouts/default.kdl).
                    // Always move-right is the simplest path that doesn't
                    // depend on `focus_terminal_pane` (which has been
                    // observed to silently fail when called from a plugin
                    // pane handler).
                    move_focus(Direction::Right);
                    return true;
                }
                false
            },
            Event::RunCommandResult(_, _, _, _) => false,
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.row_ranges.clear();
        let colors = AgentColors::from_palette(&self.mode_info.style.colors);

        enum Action {
            Diag(PaletteColor, String),
            Attention(String),
            Cells,
        }
        let action = match &self.load {
            LoadState::AwaitingPermission => Action::Attention(
                "agent-bar needs permissions — press y \
                 (fullscreen with Ctrl+Shift+; if cropped)"
                    .to_string(),
            ),
            LoadState::NoHomeInEnv => Action::Diag(
                colors.error,
                "$HOME / $USERPROFILE not in session env".to_string(),
            ),
            LoadState::Polling { path, last } => match last {
                None => Action::Diag(colors.text, format!("reading {}", path.display())),
                Some(ReadResult::Missing) => {
                    Action::Diag(colors.error, format!("missing: {}", path.display()))
                },
                Some(ReadResult::Unreadable(e)) => {
                    Action::Diag(colors.error, format!("unreadable {}: {e}", path.display()))
                },
                Some(ReadResult::ParseError(e)) => {
                    Action::Diag(colors.error, format!("parse error: {e}"))
                },
                Some(ReadResult::Ok(agents)) if agents.is_empty() => {
                    Action::Diag(colors.text, "no agents".to_string())
                },
                Some(ReadResult::Ok(_)) => Action::Cells,
            },
        };

        if rows == 0 || cols == 0 {
            return;
        }

        let frame: Vec<String> = match action {
            Action::Diag(fg, msg) => diag_frame(&msg, fg, colors.bar_bg, rows, cols),
            Action::Attention(msg) => attention_frame(&msg, colors, rows, cols),
            Action::Cells => self.cells_frame(rows, cols, &colors),
        };

        emit_frame(&frame, rows);
    }
}

impl State {
    fn current_agents(&self) -> &[Agent] {
        match &self.load {
            LoadState::Polling { last: Some(ReadResult::Ok(a)), .. } => a,
            _ => &[],
        }
    }

    fn display_order(&self) -> Vec<Agent> {
        group_by_session(self.current_agents())
            .into_iter()
            .flat_map(|g| g.agents)
            .collect()
    }

    fn am_focused(&self) -> bool {
        let Some(own) = self.own_plugin_id else { return false };
        let Some(idx) = self.active_tab_idx else { return false };
        self.pane_manifest
            .panes
            .get(&idx)
            .into_iter()
            .flatten()
            .any(|p| p.is_plugin && p.id == own && p.is_focused)
    }

    fn focused_external_pane_id(&self) -> Option<u32> {
        let idx = self.active_tab_idx?;
        self.pane_manifest
            .panes
            .get(&idx)?
            .iter()
            .find(|p| p.is_focused && !p.is_plugin)
            .map(|p| p.id)
    }

    /// Map `last_external_focused_pane` to an agent index, falling back to
    /// the first agent. `None` only when there are no agents.
    fn initial_selection(&self) -> Option<usize> {
        let agents = self.display_order();
        if agents.is_empty() {
            return None;
        }
        if let Some(prev) = self.last_external_focused_pane {
            if let Some(idx) = agents.iter().position(|a| a.zellij_pane_id == Some(prev)) {
                return Some(idx);
            }
        }
        Some(0)
    }

    /// Common dispatch for mouse click and Enter key.
    fn dispatch_focus_agent(&self, sid: &str) -> String {
        let label = sid.get(..8).unwrap_or(sid).to_string();
        let mut ctx = BTreeMap::new();
        ctx.insert("click_label".into(), label.clone());
        run_command(&["mux", "focus-agent", sid], ctx);
        label
    }

    fn effective_seen_at(&self, agent: &Agent) -> i64 {
        let disk = self.seen_disk.get(&agent.session_id).copied().unwrap_or(0);
        let overlay = self.seen_overlay.get(&agent.session_id).copied().unwrap_or(0);
        disk.max(overlay)
    }

    fn acknowledge_agent(&mut self, session_id: &str) {
        let Some(agent) = self.current_agents().iter().find(|a| a.session_id == session_id)
        else { return };
        if agent.attention_at_ms == 0 {
            return;
        }
        let at_ms = agent.attention_at_ms;
        let entry = self.seen_overlay.entry(session_id.to_string()).or_insert(0);
        if at_ms > *entry {
            *entry = at_ms;
        }
        let Some(dir) = &self.seen_events_dir else { return };
        let _ = std::fs::create_dir_all(dir);
        let event = serde_json::json!({
            "session_id": session_id,
            "seen_at_ms": at_ms,
        });
        let Ok(json) = serde_json::to_vec(&event) else { return };
        let target = dir.join(format!("{session_id}.json"));
        let tmp = dir.join(format!("{session_id}.json.tmp"));
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &target);
        }
    }

    fn prune_overlay(&mut self, agents: &[Agent]) {
        let live: HashSet<&str> = agents.iter().map(|a| a.session_id.as_str()).collect();
        self.seen_overlay.retain(|sid, overlay_seen| {
            if !live.contains(sid.as_str()) {
                return false;
            }
            let disk_seen = self.seen_disk.get(sid).copied().unwrap_or(0);
            *overlay_seen > disk_seen
        });
    }

    fn refresh_seen_disk(&mut self) -> bool {
        let Some(dir) = &self.seen_events_dir else { return false };
        let new = agents::read_seen_events(dir);
        if new == self.seen_disk {
            return false;
        }
        self.seen_disk = new;
        true
    }

    fn cells_frame(&mut self, rows: usize, cols: usize, colors: &AgentColors) -> Vec<String> {
        let raw_agents = self.current_agents().to_vec();
        self.prune_overlay(&raw_agents);
        self.refresh_seen_disk();

        let session = self.mode_info.session_name.as_deref().unwrap_or("").to_string();
        let active_pane_id = self.focused_external_pane_id();

        // Compute flags first; pass into rendering.
        let mut flags: HashMap<String, Flags> = HashMap::new();
        for agent in &raw_agents {
            let unrouted = agent.zellij_pane_id.is_none();
            let unseen = agent.attention_at_ms > 0
                && agent.attention_at_ms > self.effective_seen_at(agent);
            // Active pane = the ONE pane the user is currently typing in.
            // Requires same zellij session AND same pane id. When the
            // panel itself is focused, focused_external_pane_id is None
            // and no agent gets this flag — green-bg is reserved for the
            // moment when the agent's own pane has the cursor.
            let is_active_pane = active_pane_id.is_some()
                && agent.zellij_session.as_deref() == Some(session.as_str())
                && agent.zellij_pane_id == active_pane_id;
            flags.insert(
                agent.session_id.clone(),
                Flags { unrouted, needs_attention: unseen, is_active_pane },
            );
        }

        let groups = group_by_session(&raw_agents);
        let display: Vec<Agent> = groups.iter().flat_map(|g| g.agents.clone()).collect();

        // Clamp selected_idx in case agents went away between ticks.
        self.selected_idx = self.selected_idx.and_then(|i| {
            display.len().checked_sub(1).map(|max| i.min(max))
        });

        // Names wrap inside `cols - OUTER_PAD - AGENT_INDENT`. Headers use
        // `cols - OUTER_PAD` (header bg fills out to that edge).
        let name_wrap_w = cols.saturating_sub(OUTER_PAD).saturating_sub(AGENT_INDENT);
        let (lines, agent_row_starts) = build_lines(&groups, &flags, name_wrap_w);

        // Scroll so selected agent's first line is visible.
        self.adjust_scroll(&agent_row_starts, &lines, rows);

        let mut frame: Vec<String> = Vec::with_capacity(rows);
        let selected_sid = self
            .selected_idx
            .and_then(|i| display.get(i))
            .map(|a| a.session_id.clone());

        let end = (self.scroll_offset + rows).min(lines.len());
        for (visible_row, abs_idx) in (self.scroll_offset..end).enumerate() {
            let line = &lines[abs_idx];
            let rendered = match line {
                Line::Padding => render_padding(colors, cols),
                Line::Header { label, unrouted_in_group } => {
                    render_header(label, *unrouted_in_group, colors, cols)
                },
                Line::AgentRow {
                    sid,
                    status,
                    text,
                    needs_attention,
                    unrouted,
                    is_active_pane,
                    is_first_wrap_line,
                    ..
                } => {
                    let selected = selected_sid.as_deref() == Some(sid.as_str());
                    self.row_ranges.push((visible_row, sid.clone()));
                    render_agent_line(
                        text,
                        *status,
                        *needs_attention,
                        *unrouted,
                        *is_active_pane,
                        *is_first_wrap_line,
                        selected,
                        colors,
                        cols,
                    )
                },
            };
            frame.push(rendered);
        }

        // Scroll indicators in the rightmost col if content exceeds viewport.
        let has_more_above = self.scroll_offset > 0;
        let has_more_below = end < lines.len();
        if has_more_above && !frame.is_empty() {
            let row = &mut frame[0];
            row.push_str(
                &style!(colors.accent, colors.bar_bg)
                    .paint("\u{2191}".to_string())
                    .to_string(),
            );
        }
        if has_more_below && !frame.is_empty() {
            let last = frame.len() - 1;
            let row = &mut frame[last];
            row.push_str(
                &style!(colors.accent, colors.bar_bg)
                    .paint("\u{2193}".to_string())
                    .to_string(),
            );
        }

        frame
    }

    fn adjust_scroll(
        &mut self,
        agent_row_starts: &[(usize, usize)],
        lines: &[Line],
        rows: usize,
    ) {
        let Some(sel) = self.selected_idx else {
            self.scroll_offset = 0;
            return;
        };
        let Some(&(start, end_exclusive)) = agent_row_starts.get(sel) else {
            return;
        };
        let total = lines.len();
        let max_scroll = total.saturating_sub(rows);
        if start < self.scroll_offset {
            self.scroll_offset = start.saturating_sub(1); // keep header in view if possible
        } else if end_exclusive > self.scroll_offset + rows {
            self.scroll_offset = end_exclusive.saturating_sub(rows);
        }
        if self.scroll_offset > max_scroll {
            self.scroll_offset = max_scroll;
        }
    }
}

/// One zellij-session's worth of agents.
struct Group {
    label: GroupLabel,
    agents: Vec<Agent>,
    max_started_ms: i64,
    any_unrouted: bool,
}

#[derive(Clone)]
enum GroupLabel {
    Session(String),
    Sessionless,
}

impl GroupLabel {
    fn as_str(&self) -> &str {
        match self {
            GroupLabel::Session(s) => s.as_str(),
            GroupLabel::Sessionless => SESSIONLESS_LABEL,
        }
    }
}

#[derive(Clone, Copy)]
struct Flags {
    unrouted: bool,
    needs_attention: bool,
    is_active_pane: bool,
}

enum Line {
    /// Blank breathing room above non-first groups.
    Padding,
    Header {
        label: String,
        unrouted_in_group: bool,
    },
    AgentRow {
        sid: String,
        status: AgentStatus,
        text: String,
        needs_attention: bool,
        unrouted: bool,
        is_active_pane: bool,
        /// True on the first wrap-line of an agent — the `>` (or `✗` if
        /// unrouted) row marker shows only on this line so adjacent agents
        /// with the same bg tint stay visually distinct.
        is_first_wrap_line: bool,
    },
}

fn group_by_session(agents: &[Agent]) -> Vec<Group> {
    let mut by_key: BTreeMap<Option<String>, Group> = BTreeMap::new();
    for a in agents {
        let key = a.zellij_session.clone();
        let label = match &key {
            Some(s) => GroupLabel::Session(s.clone()),
            None => GroupLabel::Sessionless,
        };
        let entry = by_key.entry(key).or_insert(Group {
            label,
            agents: Vec::new(),
            max_started_ms: i64::MIN,
            any_unrouted: false,
        });
        if a.started_at_ms > entry.max_started_ms {
            entry.max_started_ms = a.started_at_ms;
        }
        if a.zellij_pane_id.is_none() {
            entry.any_unrouted = true;
        }
        entry.agents.push(a.clone());
    }
    let mut groups: Vec<Group> = by_key.into_values().collect();
    for g in &mut groups {
        // Newest agent first within the group.
        g.agents.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
    }
    groups.sort_by(|a, b| match (&a.label, &b.label) {
        (GroupLabel::Sessionless, GroupLabel::Sessionless) => std::cmp::Ordering::Equal,
        (GroupLabel::Sessionless, _) => std::cmp::Ordering::Greater,
        (_, GroupLabel::Sessionless) => std::cmp::Ordering::Less,
        _ => b.max_started_ms.cmp(&a.max_started_ms),
    });
    groups
}

/// Flatten groups → lines and record (start, end_exclusive) row ranges per
/// agent in display order. Used by scroll logic to keep the selection in
/// the viewport.
fn build_lines(
    groups: &[Group],
    flags: &HashMap<String, Flags>,
    content_w: usize,
) -> (Vec<Line>, Vec<(usize, usize)>) {
    let mut lines: Vec<Line> = Vec::new();
    let mut agent_rows: Vec<(usize, usize)> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        if gi > 0 {
            lines.push(Line::Padding);
        }
        lines.push(Line::Header {
            label: g.label.as_str().to_string(),
            unrouted_in_group: g.any_unrouted,
        });
        for a in &g.agents {
            let f = flags.get(&a.session_id).copied().unwrap_or(Flags {
                unrouted: false,
                needs_attention: false,
                is_active_pane: false,
            });
            let text = if a.name.trim().is_empty() {
                a.session_id.get(..8).unwrap_or(&a.session_id).to_string()
            } else {
                a.name.trim().to_string()
            };
            let wrapped = wrap_text(&text, content_w, MAX_NAME_LINES);
            let start = lines.len();
            for (i, w) in wrapped.into_iter().enumerate() {
                lines.push(Line::AgentRow {
                    sid: a.session_id.clone(),
                    status: a.status,
                    text: w,
                    needs_attention: f.needs_attention,
                    unrouted: f.unrouted,
                    is_active_pane: f.is_active_pane,
                    is_first_wrap_line: i == 0,
                });
            }
            let end_exclusive = lines.len();
            agent_rows.push((start, end_exclusive));
        }
    }
    (lines, agent_rows)
}

/// Soft-break on '-', '_', '/', '.', ' '. Greedy line-fill. Hard-split a
/// token wider than `width`. Truncate past `max_lines` with `…`.
fn wrap_text(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let separators = ['-', '_', '/', '.', ' '];
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if separators.contains(&c) {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut truncated = false;
    'outer: for tok in &tokens {
        let tok_w = tok.width();
        if cur.width() + tok_w <= width {
            cur.push_str(tok);
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            if lines.len() >= max_lines {
                truncated = true;
                break 'outer;
            }
        }
        if tok_w > width {
            for chunk in hard_split(tok, width) {
                if lines.len() >= max_lines {
                    truncated = true;
                    break 'outer;
                }
                if cur.width() + chunk.width() <= width {
                    cur.push_str(&chunk);
                } else {
                    if !cur.is_empty() {
                        lines.push(std::mem::take(&mut cur));
                        if lines.len() >= max_lines {
                            truncated = true;
                            break 'outer;
                        }
                    }
                    cur.push_str(&chunk);
                }
            }
        } else {
            cur.push_str(tok);
        }
    }
    if !cur.is_empty() && lines.len() < max_lines {
        lines.push(cur);
    } else if !cur.is_empty() {
        truncated = true;
    }

    // If we didn't consume the whole input, ellipsize the last line.
    let consumed: usize = lines.iter().map(|l| l.chars().count()).sum();
    let total_chars = text.chars().count();
    if truncated || consumed < total_chars {
        if let Some(last) = lines.last_mut() {
            while last.width() + 1 > width && !last.is_empty() {
                last.pop();
            }
            last.push('\u{2026}');
        }
    }
    lines
}

fn hard_split(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        let cw = c.to_string().width();
        if cur.width() + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn render_padding(colors: &AgentColors, cols: usize) -> String {
    let pad: String = std::iter::repeat(' ').take(cols).collect();
    style!(colors.text, colors.bar_bg).paint(pad).to_string()
}

fn render_header(
    label: &str,
    unrouted_in_group: bool,
    colors: &AgentColors,
    cols: usize,
) -> String {
    // The header strip spans the full row width in header_bg — unlike agent
    // rows it has no bar_bg margin, so it extends past OUTER_PAD to the left
    // edge. The leading col is a header_bg space, giving the title a 1-col
    // lead instead of butting against the edge.
    let body_w = cols.saturating_sub(OUTER_PAD);
    let mut text_budget = body_w;
    let mut trimmed = String::new();
    for c in label.chars() {
        let cw = c.to_string().width();
        if trimmed.width() + cw > text_budget {
            break;
        }
        trimmed.push(c);
    }
    if trimmed.width() < label.width() {
        while trimmed.width() + 1 > text_budget && !trimmed.is_empty() {
            trimmed.pop();
        }
        trimmed.push('\u{2026}');
    }
    text_budget = text_budget.saturating_sub(trimmed.width());
    let unrouted_suffix = if unrouted_in_group && text_budget >= 2 {
        text_budget = text_budget.saturating_sub(2);
        true
    } else {
        false
    };
    let h_fg = colors.text;
    let h_bg = colors.header_bg;
    let mut body = String::new();
    body.push_str(&style!(h_fg, h_bg).bold().paint(trimmed).to_string());
    if unrouted_suffix {
        body.push_str(
            &style!(colors.error, h_bg)
                .bold()
                .paint(format!(" {UNROUTED}"))
                .to_string(),
        );
    }
    let trailing: String = std::iter::repeat(' ').take(text_budget).collect();
    body.push_str(&style!(h_fg, h_bg).paint(trailing).to_string());
    let outer = style!(h_fg, h_bg).paint(" ".to_string()).to_string();
    format!("{outer}{body}")
}

fn render_agent_line(
    text: &str,
    status: AgentStatus,
    needs_attention: bool,
    unrouted: bool,
    is_active_pane: bool,
    is_first_wrap_line: bool,
    selected: bool,
    colors: &AgentColors,
    cols: usize,
) -> String {
    // Resolve (bg, fg) by priority — selection wins (keyboard cursor),
    // then user-attention states (yellow), then active-pane (green,
    // mirroring active-tab), then status text on neutral bg.
    let (cell_bg, fg, colored_bg, bold) = if selected {
        (colors.selected_bg, colors.selected_fg, true, true)
    } else if needs_attention || matches!(status, AgentStatus::Waiting) {
        (colors.yellow, colors.on_color, true, true)
    } else if is_active_pane {
        (colors.green, colors.on_color, true, true)
    } else {
        // No bg tint — fg by status. Waiting is handled above so this
        // arm only ever sees Busy or Idle.
        let fg = match status {
            AgentStatus::Busy => colors.green,
            AgentStatus::Idle => colors.text,
            AgentStatus::Waiting => colors.text, // unreachable
        };
        (colors.bar_bg, fg, false, false)
    };
    // 1-col outer pad on bar_bg sits to the left of every agent row, giving
    // breathing room from the pane border. Selection/attention bg starts
    // from the agent body, not from this pad, so the row's left edge
    // visually separates from the border line.
    let outer_pad = style!(fg, colors.bar_bg).paint(" ".to_string()).to_string();

    let body_w = cols.saturating_sub(OUTER_PAD);
    let mut out = String::new();
    // Unrouted marker stays theme-error-red on neutral bg, switches to the
    // on-colour fg (theme's "on green/yellow" colour) on a tinted bg so it
    // doesn't disappear into the tint.
    let unrouted_color = if colored_bg { colors.on_color } else { colors.error };
    let style_for = |fg_color: PaletteColor| {
        let mut s = style!(fg_color, cell_bg);
        if bold {
            s = s.bold();
        }
        s
    };

    // Row marker — shows only on the first wrap-line of an agent so adjacent
    // agents with the same bg tint stay visually distinct. Unrouted agents
    // get `✗` (red), all others get `>` (same fg as the row text).
    let (marker_char, marker_color) = if !is_first_wrap_line {
        (' ', fg)
    } else if unrouted {
        ('\u{2717}', unrouted_color) // ✗
    } else {
        ('>', fg)
    };
    out.push_str(&style_for(marker_color).paint(marker_char.to_string()).to_string());
    out.push_str(&style_for(fg).paint(" ".to_string()).to_string());

    let content_w = body_w.saturating_sub(AGENT_INDENT);
    let mut visible = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = c.to_string().width();
        if w + cw > content_w {
            break;
        }
        visible.push(c);
        w += cw;
    }
    out.push_str(&style_for(fg).paint(visible).to_string());

    let used = AGENT_INDENT + w;
    if used < body_w {
        let pad = body_w - used;
        let pad_str: String = std::iter::repeat(' ').take(pad).collect();
        out.push_str(&style_for(fg).paint(pad_str).to_string());
    }
    format!("{outer_pad}{out}")
}

fn diag_frame(
    msg: &str,
    fg: PaletteColor,
    bg: PaletteColor,
    rows: usize,
    cols: usize,
) -> Vec<String> {
    let mut frame = Vec::with_capacity(rows);
    let wrapped = wrap_text(msg, cols.saturating_sub(1), rows);
    for line in wrapped {
        frame.push(style!(fg, bg).paint(line).to_string());
    }
    frame
}

fn attention_frame(msg: &str, colors: AgentColors, rows: usize, cols: usize) -> Vec<String> {
    let mut frame = Vec::with_capacity(rows);
    let wrapped = wrap_text(msg, cols.saturating_sub(2), rows);
    for line in wrapped {
        let padded = format!(" {line} ");
        frame.push(
            style!(colors.on_color, colors.yellow)
                .bold()
                .paint(padded)
                .to_string(),
        );
    }
    frame
}

fn emit_frame(frame: &[String], rows: usize) {
    let mut out = String::new();
    // Cursor home before drawing — plugins are repainted into the same
    // origin each frame, so anchor explicitly to avoid drift after partial
    // ANSI sequences in the previous frame.
    out.push_str("\u{1b}[H");
    for r in 0..rows {
        if r > 0 {
            out.push_str("\r\n");
        }
        if let Some(line) = frame.get(r) {
            out.push_str(line);
        }
        out.push_str("\u{1b}[0K");
    }
    print!("{out}");
}

fn same_result(a: Option<&ReadResult>, b: &ReadResult) -> bool {
    match (a, b) {
        (Some(ReadResult::Ok(x)), ReadResult::Ok(y)) => same_agents(x, y),
        (Some(ReadResult::Missing), ReadResult::Missing) => true,
        (Some(ReadResult::Unreadable(x)), ReadResult::Unreadable(y)) => x == y,
        (Some(ReadResult::ParseError(x)), ReadResult::ParseError(y)) => x == y,
        _ => false,
    }
}

fn same_agents(a: &[Agent], b: &[Agent]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.session_id == y.session_id
                && x.status == y.status
                && x.zellij_pane_id == y.zellij_pane_id
                && x.zellij_session == y.zellij_session
                && x.name == y.name
                && x.started_at_ms == y.started_at_ms
                && x.attention_at_ms == y.attention_at_ms
        })
}
