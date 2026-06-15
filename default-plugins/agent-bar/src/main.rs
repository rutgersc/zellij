//! Vertical agent panel — one column on the side of a tab, showing every
//! running Claude agent across every zellij session. Polls
//! `~/.claude/readmodel/agents.json` every `POLL_SECS`.
//!
//! Layout: each agent's `name` is word-wrapped to the pane width (1..=4
//! rows). Agents are grouped under the zellij session they belong to;
//! groups (and agents within them) are ordered by `started_at_ms` asc, so
//! the oldest sessions stay at the top and newly created ones append at the
//! bottom. A session's slot is pinned by its earliest agent's creation
//! time, so it doesn't jump around as later agents come and go. Agents
//! without a `zellij_session` collect in a "sessionless" group pinned to
//! the very bottom.
//!
//! Status carries through the name fg (magenta=busy, white=idle/waiting,
//! red=unknown). The row's first columns are a per-column priority stack:
//! attention/waiting (yellow, red if unknown), in-view (green), and selection
//! (grey) each claim columns by priority, so combinations layer rather than
//! overwrite (see the palette block below). `✗` after the session header marks
//! agents missing a `zellij_pane_id`.
//!
//! Click any row of an agent → `mux focus-agent <session_id>`. Up/Down
//! moves the keyboard cursor across agents, Enter activates, Esc returns
//! focus to the previous pane. While that `mux` command is in flight a
//! braille spinner replaces the row's `>` marker (col 1), so a slow focus
//! shows work happening right where you clicked; it clears on the matching
//! RunCommandResult (or, as a safety net, after MAX_SPIN_TICKS).
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
/// Fast tick used only while a click's `mux focus-agent` is in flight, so the
/// per-row spinner animates. We never stack timers — the single Timer handler
/// re-arms at this rate while `in_flight` is non-empty and reverts to
/// POLL_SECS once it drains — so the two cadences can't compound.
const SPIN_SECS: f64 = 0.1;
/// Spinner frames, one column wide each (braille reads cleanly in the marker
/// cell). The in-flight row's age in fast ticks indexes this, mod its length.
const SPIN_FRAMES: [char; 10] = [
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}',
    '\u{2827}', '\u{2807}', '\u{280F}',
];
/// Safety cap: drop an in-flight row after this many fast ticks (~10s) even if
/// its RunCommandResult never arrives, so a lost result can't spin forever.
const MAX_SPIN_TICKS: u32 = 100;
/// Hard cap on rows used to render a single agent's name. Past this we
/// truncate with `…` — the full name is still on the snapshot.
const MAX_NAME_LINES: usize = 4;
/// Width of col 0 — the leftmost cell of every agent row. Shows the selection
/// grey when keyboard-selected, otherwise neutral. It's the innermost (least
/// dominant) layer of the per-column state stack (see palette block below).
const OUTER_PAD: usize = 1;
/// After col 0, two more cols before the wrapped name: col 1 = the `>` row
/// marker (or `✗` if unrouted), col 2 = a single space. Both sit in the
/// per-column state stack; the marker glyph rides on col 1's bg.
const AGENT_INDENT: usize = 2;
const SESSIONLESS_LABEL: &str = "sessionless";

// Agent-state palette — a shared colour vocabulary (agent-bar today, the mux
// session picker later). Concurrent states are composited as a PER-COLUMN
// priority stack across the row's first columns (a z-stack projected onto the
// left edge — lower-priority states "peek out" to the left as the dominant one
// takes the body). Columns: 0 | 1 (`>`) | 2 (space) | 3.. (name).
//
//   col 0      → grey if selected, else neutral.
//   col 1 & 2  → in-view green · else alert (yellow, or red if unknown) · else
//                selected grey · else neutral.   (context tint survives
//                selection here — the middle keeps its green/yellow.)
//   col 3 name → selected grey · else alert (yellow/red) · else in-view green
//                · else neutral.   (selection outranks everything on the body,
//                so a highlighted row is unmissable even when green/yellow.)
//
// Name fg: selection overrides the status hue (selected-fg on selected-bg is a
// high-contrast pair); otherwise it encodes status, picking a hue legible on
// its body bg:
//   busy    → magenta (reads on neutral/green/yellow alike).
//   unknown → on-colour (its body is red).
//   idle /  → text on a neutral body, on-colour on a green/yellow body
//   waiting    (colourless states carry no hue to lose).
// The `>` marker rides on col 1 (the middle): selection wins first (selected-fg,
// matching the name so the selected row's marker + name are one solid highlight),
// else on-colour over a tinted middle, else the name fg.
//
// All colours resolve from `mode_info.style.colors` so a ChangeTheme repaints
// everything at once — green/yellow/on-colour/magenta come from ribbon_selected,
// exit_code_error and text_unselected. Only the structural glyphs are hardcoded.
const UNROUTED: &str = "\u{2717}";

/// Theme-driven colours for every agent state. Built once per render from
/// `mode_info.style.colors` so a ChangeTheme keystroke updates the whole
/// panel atomically.
#[derive(Copy, Clone)]
struct AgentColors {
    /// The "green" — bg for in-view.
    green: PaletteColor,
    /// The "magenta/purple" — fg for busy. The theme's text emphasis-3 slot,
    /// a foreground-legible hue distinct from green/yellow/red, so a busy
    /// agent reads clearly on every body bg (neutral, grey, green, yellow).
    magenta: PaletteColor,
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
            // text_unselected.emphasis_3 is the theme's magenta slot (pink/
            // purple family) — a text-emphasis colour, so it's tuned to be
            // legible as a foreground.
            magenta: p.text_unselected.emphasis_3,
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
    /// session_ids whose `mux focus-agent` was dispatched but hasn't reported
    /// back yet (cleared on RunCommandResult). The value is the row's age in
    /// fast ticks — used both as the spinner frame index and the
    /// MAX_SPIN_TICKS safety cap.
    in_flight: HashMap<String, u32>,
    /// Adaptive-timer accounting: the interval the currently-pending Timer was
    /// armed at, and elapsed time accumulated toward the next readmodel poll.
    cur_timeout: f64,
    since_poll: f64,
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
            PermissionType::WriteToClipboard,
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
                    self.cur_timeout = POLL_SECS;
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
                // Advance any in-flight click spinners — this is what makes
                // the marker glyph animate. Returns true (needs repaint) while
                // anything is still spinning.
                let mut changed = self.advance_spinners();

                // Poll the readmodel at POLL_SECS regardless of how fast we're
                // ticking for the spinner: accumulate the interval the pending
                // timer was armed at and only re-read once a poll is due.
                self.since_poll += self.cur_timeout;
                if self.since_poll + 1e-9 >= POLL_SECS {
                    self.since_poll = 0.0;
                    if let LoadState::Polling { path, last } = &mut self.load {
                        let new = agents::read(path);
                        if !same_result(last.as_ref(), &new) {
                            changed = true;
                        }
                        *last = Some(new);
                    }
                    if self.refresh_seen_disk() {
                        changed = true;
                    }
                }

                // Re-arm: fast while a click is in flight (so the spinner
                // spins), otherwise the normal poll cadence. This is the only
                // set_timeout outside load, so exactly one timer is ever
                // pending and the two rates can't stack.
                self.cur_timeout = if self.in_flight.is_empty() { POLL_SECS } else { SPIN_SECS };
                set_timeout(self.cur_timeout);
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
                    self.in_flight.insert(sid.clone(), 0);
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
                let yank = matches!(key.bare_key, BareKey::Char('y')) && no_mods;
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
                        self.in_flight.insert(sid.clone(), 0);
                        let _ = self.dispatch_focus_agent(&sid);
                        return true;
                    }
                    return false;
                }
                if yank {
                    if let Some(sid) = self
                        .selected_idx
                        .and_then(|i| agents.get(i))
                        .map(|a| a.session_id.clone())
                    {
                        copy_to_clipboard(sid);
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
            Event::RunCommandResult(_, _, _, ctx) => {
                // The dispatched `mux focus-agent` finished — stop its spinner.
                match ctx.get("session_id") {
                    Some(sid) => self.in_flight.remove(sid).is_some(),
                    None => false,
                }
            },
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
        // Echoed back on RunCommandResult so we can clear this row's spinner.
        ctx.insert("session_id".into(), sid.to_string());
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

    /// Tick every in-flight row's spinner age forward, dropping any that have
    /// outlived MAX_SPIN_TICKS (the missing-RunCommandResult safety net).
    /// Returns whether anything is still spinning — i.e. a repaint is due.
    fn advance_spinners(&mut self) -> bool {
        if self.in_flight.is_empty() {
            return false;
        }
        self.in_flight.retain(|_, age| {
            *age += 1;
            *age <= MAX_SPIN_TICKS
        });
        true
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
        // Panes "in view" right now = the non-suppressed terminal panes of
        // THIS session's active tab. An agent whose pane is among them is on
        // screen (its tab is the active one) — that's what green now signals,
        // regardless of whether the panel or a terminal currently has focus.
        let in_view_pane_ids: HashSet<u32> = self
            .active_tab_idx
            .and_then(|idx| self.pane_manifest.panes.get(&idx))
            .into_iter()
            .flatten()
            .filter(|p| !p.is_plugin && !p.is_suppressed)
            .map(|p| p.id)
            .collect();

        // Compute flags first; pass into rendering.
        let mut flags: HashMap<String, Flags> = HashMap::new();
        for agent in &raw_agents {
            let unrouted = agent.zellij_pane_id.is_none();
            let unseen = agent.attention_at_ms > 0
                && agent.attention_at_ms > self.effective_seen_at(agent);
            // In view = same zellij session AND the agent's pane sits in this
            // session's active tab. The session guard is load-bearing: pane
            // ids are unique only within a session, so without it a cross-
            // session agent whose id collides with an in-view pane here would
            // falsely light green. (A cross-session agent is on another
            // screen anyway, so it's correctly never in-view.)
            let is_in_view = agent.zellij_session.as_deref() == Some(session.as_str())
                && matches!(agent.zellij_pane_id, Some(pid) if in_view_pane_ids.contains(&pid));
            flags.insert(
                agent.session_id.clone(),
                Flags { unrouted, needs_attention: unseen, is_in_view },
            );
        }

        let groups = group_by_session(&raw_agents);
        let display: Vec<Agent> = groups.iter().flat_map(|g| g.agents.clone()).collect();

        // Clamp selected_idx in case agents went away between ticks.
        self.selected_idx = self.selected_idx.and_then(|i| {
            display.len().checked_sub(1).map(|max| i.min(max))
        });

        // Names wrap inside `cols - OUTER_PAD - AGENT_INDENT` (col 0 + the `>`
        // marker + the space are reserved on the left). Headers use
        // `cols - OUTER_PAD`.
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
                    is_in_view,
                    is_first_wrap_line,
                    ..
                } => {
                    let selected = selected_sid.as_deref() == Some(sid.as_str());
                    self.row_ranges.push((visible_row, sid.clone()));
                    let spinner = self
                        .in_flight
                        .get(sid)
                        .map(|&age| SPIN_FRAMES[age as usize % SPIN_FRAMES.len()]);
                    render_agent_line(
                        text,
                        status.clone(),
                        *needs_attention,
                        *unrouted,
                        *is_in_view,
                        *is_first_wrap_line,
                        selected,
                        spinner,
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
    /// Earliest agent `started_at_ms` in the group — the session's creation
    /// time. Used as the group's sort key so its slot stays put as later
    /// agents come and go.
    created_ms: i64,
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
    is_in_view: bool,
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
        is_in_view: bool,
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
            created_ms: i64::MAX,
            any_unrouted: false,
        });
        if a.started_at_ms < entry.created_ms {
            entry.created_ms = a.started_at_ms;
        }
        if a.zellij_pane_id.is_none() {
            entry.any_unrouted = true;
        }
        entry.agents.push(a.clone());
    }
    let mut groups: Vec<Group> = by_key.into_values().collect();
    for g in &mut groups {
        // Oldest agent first within the group (creation order).
        g.agents.sort_by(|a, b| a.started_at_ms.cmp(&b.started_at_ms));
    }
    groups.sort_by(|a, b| match (&a.label, &b.label) {
        (GroupLabel::Sessionless, GroupLabel::Sessionless) => std::cmp::Ordering::Equal,
        (GroupLabel::Sessionless, _) => std::cmp::Ordering::Greater,
        (_, GroupLabel::Sessionless) => std::cmp::Ordering::Less,
        _ => a.created_ms.cmp(&b.created_ms),
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
                is_in_view: false,
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
                    status: a.status.clone(),
                    text: w,
                    needs_attention: f.needs_attention,
                    unrouted: f.unrouted,
                    is_in_view: f.is_in_view,
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
    is_in_view: bool,
    is_first_wrap_line: bool,
    selected: bool,
    spinner: Option<char>,
    colors: &AgentColors,
    cols: usize,
) -> String {
    // Per-column priority stack (see the palette block at the top of this
    // file). `alert` = needs-attention / waiting / unknown; it shows yellow,
    // or red when the status itself is unknown.
    let is_unknown = matches!(status, AgentStatus::Unknown(_));
    let alert = needs_attention || matches!(status, AgentStatus::Waiting) || is_unknown;
    let alert_color = if is_unknown { colors.error } else { colors.yellow };

    // Selection is the dominant signal: when a row is selected it claims the
    // BODY (col 3) bg + the name fg outright, so the highlight is unmissable
    // even on a green/yellow row. Only the MIDDLE (col 1/2) keeps the context
    // tint, so a selected row still shows a sliver of its green/yellow there.
    //
    // col 0   → selected grey, else neutral.
    // col 1/2 → in-view green · alert · selected grey · neutral.  (context tint
    //           survives selection here — green outranks alert.)
    // col 3   → selected grey · alert · in-view green · neutral.  (selection
    //           outranks everything on the body.)
    let col0_bg = if selected { colors.selected_bg } else { colors.bar_bg };
    let mid_bg = if is_in_view {
        colors.green
    } else if alert {
        alert_color
    } else if selected {
        colors.selected_bg
    } else {
        colors.bar_bg
    };
    let body_bg = if selected {
        colors.selected_bg
    } else if alert {
        alert_color
    } else if is_in_view {
        colors.green
    } else {
        colors.bar_bg
    };

    // Name fg: selection overrides the status hue so the body always reads as
    // selected-fg-on-selected-bg (a deliberately high-contrast pair). Otherwise
    // it encodes status, choosing a hue that reads on the body bg — busy's
    // magenta reads on every bg, idle/waiting take on-colour on a tinted body.
    let body_tinted = alert || is_in_view;
    let name_fg = if selected {
        colors.selected_fg
    } else if matches!(status, AgentStatus::Busy) {
        colors.magenta
    } else if is_unknown {
        colors.on_color // body is red
    } else if body_tinted {
        colors.on_color // green / yellow body
    } else {
        colors.text
    };

    // Bold whenever the row carries any signal so it pops without a full bg.
    let bold = selected || is_in_view || alert;
    let paint = |fg: PaletteColor, bg: PaletteColor, s: String| {
        let mut style = style!(fg, bg);
        if bold {
            style = style.bold();
        }
        style.paint(s).to_string()
    };

    // Marker (col 1), first wrap-line only (blanked on continuations so stacked
    // agents stay distinct). An in-flight click spinner preempts the static
    // marker on the first wrap-line, so the "working" feedback lands exactly on
    // the row you clicked.
    let marker_char = if !is_first_wrap_line {
        ' '
    } else if let Some(spin) = spinner {
        spin
    } else if unrouted {
        '\u{2717}' // ✗
    } else {
        '>'
    };
    // The marker rides on `mid_bg`, NOT the body. Selection wins first so a
    // selected row's `>` matches its name (both `selected_fg`) and the cursor
    // reads as one solid highlight — even when the middle keeps a green/yellow
    // tint. Otherwise the marker takes on-colour over a tinted middle, else the
    // name fg. The unrouted `✗` stays error red (its own signal), swapping to
    // on-colour only where red wouldn't read.
    let marker_fg = if unrouted && spinner.is_none() {
        if is_in_view || alert { colors.on_color } else { colors.error }
    } else if selected {
        colors.selected_fg
    } else if is_in_view || alert {
        colors.on_color
    } else {
        name_fg
    };

    // Width budget: col 0 (1) + marker (1) + space (1) + name/pad = cols.
    let name_w = cols.saturating_sub(OUTER_PAD).saturating_sub(AGENT_INDENT);
    let mut visible = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = c.to_string().width();
        if w + cw > name_w {
            break;
        }
        visible.push(c);
        w += cw;
    }
    let pad: String = std::iter::repeat(' ').take(name_w - w).collect();

    let mut row = String::new();
    row.push_str(&paint(name_fg, col0_bg, " ".to_string())); // col 0
    row.push_str(&paint(marker_fg, mid_bg, marker_char.to_string())); // col 1: marker
    row.push_str(&paint(name_fg, mid_bg, " ".to_string())); // col 2: space
    row.push_str(&paint(name_fg, body_bg, visible)); // col 3: name
    row.push_str(&paint(name_fg, body_bg, pad));
    row
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
