//! Vertical agent panel — one column on the side of a tab, showing every
//! running Claude agent across every zellij session. Polls
//! `~/.claude/readmodel/agents.json` every `POLL_SECS`.
//!
//! Layout: each agent's `name` is word-wrapped to the pane width (1..=4
//! rows). Agents are grouped under the zellij session they belong to, and
//! **every live zellij session gets a header whether or not it has agents** —
//! the panel is a session list first, an agent list second. Clicking a session
//! header switches this client to that session; clicking an agent row routes to
//! its pane. Groups are ordered by the zellij session's own creation time
//! (oldest at the top, new ones append at the bottom), pinned on first sight so
//! a slot doesn't drift; a session that has left the live list keeps its slot
//! from its earliest agent's `started_at_ms`. Agents within a group are ordered
//! by `started_at_ms` asc. Agents without a `zellij_session` collect in a
//! "sessionless" group pinned to the very bottom.
//!
//! Status carries through the name fg (magenta=busy, white=idle/waiting,
//! red=unknown). The row's first columns are a per-column priority stack:
//! attention/waiting (yellow, red if unknown), in-view (green), and selection
//! (grey) each claim columns by priority, so combinations layer rather than
//! overwrite (see the palette block below). `✗` after the session header marks
//! agents missing a `zellij_pane_id`; `⧉` marks a session some *other* terminal
//! window already has attached, so you don't open the same one twice.
//!
//! An agent whose live heartbeat is gone is kept as *tracked-but-not-active*
//! (the daemon carries it forward as `active: false`): it renders dim with a
//! `†` marker and no in-view/alert tint, so the list stays a durable history
//! until the user clears it. `x` dismisses the selected inactive row — it
//! writes an `AgentDismissed` tombstone to `agent-dismissed-events/` (the
//! daemon then drops it from the readmodel) and hides it optimistically. Live
//! rows ignore `x`.
//!
//! Click a session header → `switch_session` to it (the header of the session
//! you're already in is inert, as is a header for a session that only exists in
//! the readmodel — switching to a name with no live session would silently
//! create one). Click a live agent's row → `mux focus-agent <session_id>`. A dead
//! (tracked-but-not-active) row only selects on click/Enter — its heartbeat is
//! gone, so routing on a casual click is likelier wrong than right. Up/Down
//! moves the keyboard cursor across agents, Enter activates the selected live
//! row, `g` goes to the selected agent's pane best-effort — live OR dead: a
//! dead agent's zellij pane usually outlives its heartbeat, so `mux
//! focus-agent` navigates to it and aborts loudly only if the pane is actually
//! gone. `y` yanks the id, `x` dismisses an inactive row, Esc returns focus to
//! the previous pane. Rows with no resolvable focus target (bg / orphan
//! children) only select. While that `mux` command is in flight a braille
//! spinner replaces the row's `>` marker (col 1), so a slow focus shows work
//! happening right where you clicked; it clears on the matching
//! RunCommandResult (or, as a safety net, after MAX_SPIN_TICKS). The wheel moves
//! the viewport without moving the cursor — load-bearing now that agentless
//! sessions each cost two rows: a header below the fold has no agent row near it
//! to navigate to, so without the wheel it would be unreachable.
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
/// Nested bg children render on a single line — visually subordinate to their
/// parent (which wraps up to MAX_NAME_LINES).
const MAX_CHILD_NAME_LINES: usize = 1;
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
/// Header suffix for a session another terminal window is attached to.
const OPEN_ELSEWHERE: &str = "\u{29C9}";

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

/// A live zellij session, independent of whether any agent runs in it.
#[derive(Clone, PartialEq)]
struct SessionMeta {
    /// Attached clients. Past our own, another terminal window holds it.
    clients: usize,
    /// Wall-clock creation, derived from the age `get_session_list()` reports.
    created_ms: i64,
}

/// What a rendered row routes to when clicked.
#[derive(Clone)]
enum RowTarget {
    Agent(String),
    /// A live session's header — switches this client to it.
    Session(String),
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
    /// Click hit-test: rendered_row → what that row routes to. Rebuilt every
    /// render.
    row_ranges: Vec<(usize, RowTarget)>,
    /// session_id → the session_id to actually focus when this row is clicked.
    /// For a top-level agent it's itself; for a nested bg child it's the parent
    /// (the child has no pane of its own). Absent → no pane to focus (orphan).
    /// Rebuilt every render alongside `row_ranges`.
    focus_targets: HashMap<String, String>,
    /// PaneManifest + active tab let us decide which agents are "here".
    pane_manifest: PaneManifest,
    active_tab_idx: Option<usize>,
    /// Every live zellij session from `get_session_list()`, keyed by name. Drives
    /// the always-present per-session header, the group sort order, and the `⧉`
    /// suffix (a client count past our own = another terminal window has it).
    sessions: HashMap<String, SessionMeta>,
    /// Latest scan of `agent-seen-events/` keyed by claude_id.
    seen_disk: HashMap<String, i64>,
    /// Optimistic mark-seen overlay; effective = max(disk, overlay).
    seen_overlay: HashMap<String, i64>,
    seen_events_dir: Option<PathBuf>,
    /// Sids the user dismissed this session — hidden optimistically until the
    /// daemon drops them from the readmodel (or they go active again on resume).
    dismissed_overlay: HashSet<String>,
    dismissed_events_dir: Option<PathBuf>,
    /// Keyboard cursor — agent index in display order (groups → agents).
    selected_idx: Option<usize>,
    own_plugin_id: Option<u32>,
    was_focused: bool,
    last_external_focused_pane: Option<u32>,
    /// Row scroll offset into the rendered line list.
    scroll_offset: usize,
    /// Set whenever the cursor MOVES, consumed by the next `adjust_scroll`. The
    /// viewport chases the selection only then — otherwise a wheel scroll that
    /// pushes the selected row off-screen would be yanked straight back.
    follow_selection: bool,
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
                            self.dismissed_events_dir = Some(PathBuf::from(
                                "/host/.claude/custom-state/agent-dismissed-events",
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
                    // Seed the session list synchronously too, so the very first
                    // frame already carries every session's header rather than
                    // waiting a poll for them to appear.
                    self.refresh_sessions();
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
                    self.follow_selection = now_focused;
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
                    if self.refresh_sessions() {
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
            // The wheel moves the viewport only — it never moves the cursor, and
            // `follow_selection` stays clear so the next render won't snap back to
            // the selected row. Overshoot down is clamped at render time, where the
            // line count and viewport height are known.
            Event::Mouse(Mouse::ScrollUp(lines)) => {
                let before = self.scroll_offset;
                self.scroll_offset = self.scroll_offset.saturating_sub(lines.max(1));
                self.scroll_offset != before
            },
            Event::Mouse(Mouse::ScrollDown(lines)) => {
                self.scroll_offset = self.scroll_offset.saturating_add(lines.max(1));
                true
            },
            Event::Mouse(Mouse::LeftClick(line, _col)) => {
                let row = if line < 0 { return false } else { line as usize };
                let hit = self
                    .row_ranges
                    .iter()
                    .find(|(r, _)| *r == row)
                    .map(|(_, target)| target.clone());
                match hit {
                    // Header click → attach this client to that session. Only
                    // headers of live sessions other than our own carry a target
                    // (see `Line::Header::switch_to`), so this can't create a
                    // session or pointlessly re-attach the one we're in.
                    Some(RowTarget::Session(name)) => {
                        switch_session(Some(&name));
                        true
                    },
                    Some(RowTarget::Agent(sid)) => {
                        if let Some(idx) = self
                            .display_order()
                            .iter()
                            .position(|a| a.session_id == sid)
                        {
                            self.selected_idx = Some(idx);
                            self.follow_selection = true;
                        }
                        // A click acknowledges the row's attention (clears the yellow)
                        // regardless of whether it can be focused — that's the only
                        // way to mark a select-only bg agent seen, since it has no
                        // pane to route to.
                        self.acknowledge_agent(&sid);
                        // Route only a LIVE agent's pane. A dead (tracked-but-not-
                        // active) row only selects — its heartbeat is gone, so routing
                        // on a casual click is likelier wrong than right. Press `g` for
                        // a best-effort jump to a dead agent's still-alive pane.
                        self.focus_row(&sid, false);
                        true
                    },
                    None => false,
                }
            },
            Event::Key(key) => {
                let agents = self.display_order();
                let len = agents.len();
                let no_mods = key.has_no_modifiers();
                let go_up = matches!(key.bare_key, BareKey::Up | BareKey::Char('k')) && no_mods;
                let go_down = matches!(key.bare_key, BareKey::Down | BareKey::Char('j')) && no_mods;
                let go_first = matches!(key.bare_key, BareKey::Home) && no_mods;
                let go_last = matches!(key.bare_key, BareKey::End | BareKey::Char('G')) && no_mods;
                let go_pane = matches!(key.bare_key, BareKey::Char('g')) && no_mods;
                let activate = matches!(key.bare_key, BareKey::Enter) && no_mods;
                let yank = matches!(key.bare_key, BareKey::Char('y')) && no_mods;
                let yank_cmd = matches!(key.bare_key, BareKey::Char('v')) && no_mods;
                let dismiss = matches!(key.bare_key, BareKey::Char('x')) && no_mods;
                let cancel = matches!(key.bare_key, BareKey::Esc | BareKey::Char('q')) && no_mods;
                if go_up && len > 0 {
                    self.selected_idx = Some(match self.selected_idx {
                        Some(i) if i > 0 => i - 1,
                        _ => 0,
                    });
                    self.follow_selection = true;
                    return true;
                }
                if go_down && len > 0 {
                    self.selected_idx = Some(match self.selected_idx {
                        Some(i) => (i + 1).min(len - 1),
                        None => 0,
                    });
                    self.follow_selection = true;
                    return true;
                }
                if go_first && len > 0 {
                    self.selected_idx = Some(0);
                    self.follow_selection = true;
                    return true;
                }
                if go_last && len > 0 {
                    self.selected_idx = Some(len - 1);
                    self.follow_selection = true;
                    return true;
                }
                if go_pane {
                    if let Some(sid) = self
                        .selected_idx
                        .and_then(|i| agents.get(i))
                        .map(|a| a.session_id.clone())
                    {
                        // Go to the selected agent's pane, best effort — routes a
                        // live OR dead row. A dead agent's zellij pane usually
                        // outlives its heartbeat; `mux focus-agent` verifies it and
                        // aborts loudly if the pane is actually gone.
                        self.acknowledge_agent(&sid);
                        self.focus_row(&sid, true);
                        return true;
                    }
                    return false;
                }
                if activate {
                    if let Some(agent) = self.selected_idx.and_then(|i| agents.get(i)) {
                        let sid = agent.session_id.clone();
                        // Enter acknowledges the row's attention (clears the
                        // yellow) even for select-only bg agents — that's their
                        // only way to be marked seen (no pane to focus).
                        self.acknowledge_agent(&sid);
                        // Route only a LIVE agent — a dead row only selects (press
                        // `g` to force a best-effort jump to its pane).
                        self.focus_row(&sid, false);
                        return true;
                    }
                    return false;
                }
                if dismiss {
                    // Only tracked-but-not-active rows can be dismissed — a live
                    // agent would just reappear on the next heartbeat.
                    if let Some(sid) = self
                        .selected_idx
                        .and_then(|i| agents.get(i))
                        .filter(|a| !a.active)
                        .map(|a| a.session_id.clone())
                    {
                        self.dismiss_agent(&sid);
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
                if yank_cmd {
                    // Copy the exact CLI behind the go-to-pane action:
                    // `mux focus-agent <focus_sid>`. Only rows that route to a pane
                    // have a command — bg / orphan rows carry no focus target.
                    if let Some(target) = self
                        .selected_idx
                        .and_then(|i| agents.get(i))
                        .and_then(|a| self.focus_targets.get(&a.session_id))
                    {
                        copy_to_clipboard(format!("mux focus-agent {target}"));
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
                // Session headers stand on their own, so an empty readmodel still
                // renders cells. "no agents" is only the whole story when there
                // are no sessions to list either.
                Some(ReadResult::Ok(agents)) if agents.is_empty() && self.sessions.is_empty() => {
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

    /// Agents minus those optimistically dismissed this session (the daemon
    /// hasn't dropped them from the readmodel yet). The render and selection
    /// paths both work off this so an `x` dismiss takes effect instantly.
    fn visible_agents(&self) -> Vec<Agent> {
        self.current_agents()
            .iter()
            .filter(|a| !self.dismissed_overlay.contains(&a.session_id))
            .cloned()
            .collect()
    }

    fn display_order(&self) -> Vec<Agent> {
        grouped(&self.visible_agents(), &self.sessions)
            .into_iter()
            .flat_map(|g| g.rows.into_iter().map(|r| r.agent))
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

    /// Common dispatch for mouse click and Enter key. `focus_sid` is the pane
    /// to focus (a child routes to its parent); `echo_sid` is the row that owns
    /// the in-flight spinner — echoed on RunCommandResult so the spinner clears
    /// on the row the user actually clicked (which == focus_sid for a top-level
    /// agent).
    /// Dispatch `mux focus-agent` for a row when it routes to a pane. `allow_dead`
    /// gates tracked-but-not-active rows: click/Enter pass `false` (a dead row's
    /// heartbeat is gone, so it only selects), the `g` key passes `true` for a
    /// best-effort jump — a dead agent's zellij pane usually outlives its
    /// heartbeat, and `mux focus-agent` verifies the pane and aborts loudly if it
    /// is actually gone. bg / orphan rows carry no target and never route.
    fn focus_row(&mut self, sid: &str, allow_dead: bool) {
        let is_dead = self
            .current_agents()
            .iter()
            .find(|a| a.session_id == sid)
            .map(|a| !a.active)
            .unwrap_or(false);
        if is_dead && !allow_dead {
            return;
        }
        if let Some(target) = self.focus_targets.get(sid).cloned() {
            self.in_flight.insert(sid.to_string(), 0);
            let _ = self.dispatch_focus_agent(&target, sid);
        }
    }

    fn dispatch_focus_agent(&self, focus_sid: &str, echo_sid: &str) -> String {
        let label = focus_sid.get(..8).unwrap_or(focus_sid).to_string();
        let mut ctx = BTreeMap::new();
        ctx.insert("click_label".into(), label.clone());
        ctx.insert("session_id".into(), echo_sid.to_string());
        run_command(&["mux", "focus-agent", focus_sid], ctx);
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

    /// Write an `AgentDismissed` tombstone and hide the row optimistically.
    /// Mirrors `acknowledge_agent`'s atomic temp-then-rename. The timestamp is
    /// wall-clock now (>= a dead agent's frozen `updated_at`, so the daemon
    /// hides it); a later resume bumps `updated_at` past this and re-surfaces it.
    fn dismiss_agent(&mut self, session_id: &str) {
        self.dismissed_overlay.insert(session_id.to_string());
        let Some(dir) = &self.dismissed_events_dir else { return };
        let _ = std::fs::create_dir_all(dir);
        let event = serde_json::json!({
            "session_id": session_id,
            "dismissed_at_ms": now_ms(),
        });
        let Ok(json) = serde_json::to_vec(&event) else { return };
        let target = dir.join(format!("{session_id}.json"));
        let tmp = dir.join(format!("{session_id}.json.tmp"));
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &target);
        }
    }

    /// Drop overlay entries once the agent is gone from the readmodel (daemon
    /// caught up) or went active again (resumed) — keep hiding only while it's
    /// still listed as inactive.
    fn prune_dismissed_overlay(&mut self, agents: &[Agent]) {
        let still_inactive: HashSet<&str> = agents
            .iter()
            .filter(|a| !a.active)
            .map(|a| a.session_id.as_str())
            .collect();
        self.dismissed_overlay
            .retain(|sid| still_inactive.contains(sid.as_str()));
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

    /// Pull the live session list. Also pushes a fresh SessionUpdate to every
    /// plugin as a side effect, so this doubles as the panel's session refresh.
    fn refresh_sessions(&mut self) -> bool {
        let now = now_ms();
        let new: HashMap<String, SessionMeta> = match get_session_list() {
            Ok(snapshot) => snapshot
                .live_sessions
                .into_iter()
                .map(|s| {
                    // `creation_time` is an AGE, not an epoch. Derive the absolute
                    // key once and then keep it: the reported age is truncated to
                    // whole seconds, so recomputing it every poll would jitter the
                    // group order and make every poll look like a change.
                    let created_ms = self
                        .sessions
                        .get(&s.name)
                        .map(|m| m.created_ms)
                        .unwrap_or_else(|| now - s.creation_time.as_millis() as i64);
                    (s.name, SessionMeta { clients: s.connected_clients, created_ms })
                })
                .collect(),
            // Only fails when the server's session-scan state is missing, which
            // can't recover — drop the list rather than keep asserting stale
            // headers.
            Err(_) => HashMap::new(),
        };
        if new == self.sessions {
            return false;
        }
        self.sessions = new;
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
        let all_agents = self.current_agents().to_vec();
        self.prune_dismissed_overlay(&all_agents);
        let raw_agents: Vec<Agent> = all_agents
            .into_iter()
            .filter(|a| !self.dismissed_overlay.contains(&a.session_id))
            .collect();
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
            // A busy agent is actively working, not waiting on you: going busy
            // again means you gave it new work, which acknowledges the prior
            // completion. Suppress the stale yellow now — the next completion
            // re-raises it with a fresh `attention_at_ms`. Without this the
            // alert lingers when you type on a pane you're already viewing (the
            // done→busy flip has no acknowledge path of its own).
            let unseen = agent.active
                && !matches!(agent.status, AgentStatus::Busy)
                && agent.attention_at_ms > 0
                && agent.attention_at_ms > self.effective_seen_at(agent);
            // In view = same zellij session AND the agent's pane sits in this
            // session's active tab. The session guard is load-bearing: pane
            // ids are unique only within a session, so without it a cross-
            // session agent whose id collides with an in-view pane here would
            // falsely light green. (A cross-session agent is on another
            // screen anyway, so it's correctly never in-view.)
            let is_in_view = agent.active
                && agent.zellij_session.as_deref() == Some(session.as_str())
                && matches!(agent.zellij_pane_id, Some(pid) if in_view_pane_ids.contains(&pid));
            flags.insert(
                agent.session_id.clone(),
                Flags {
                    unrouted,
                    needs_attention: unseen,
                    is_in_view,
                    active: agent.active,
                    is_bg: agent.kind == "bg",
                },
            );
        }

        let groups = grouped(&raw_agents, &self.sessions);
        let display: Vec<Agent> = groups
            .iter()
            .flat_map(|g| g.rows.iter().map(|r| r.agent.clone()))
            .collect();
        // Rebuild the click → focus-target map in lockstep with the rows:
        // interactive agents map to their own pane; background agents have no
        // target (select-only).
        self.focus_targets.clear();
        for g in &groups {
            for r in &g.rows {
                if let Some(t) = &r.focus_sid {
                    self.focus_targets.insert(r.agent.session_id.clone(), t.clone());
                }
            }
        }

        // Clamp selected_idx in case agents went away between ticks.
        self.selected_idx = self.selected_idx.and_then(|i| {
            display.len().checked_sub(1).map(|max| i.min(max))
        });

        // Names wrap inside `cols - OUTER_PAD - AGENT_INDENT` (col 0 + the `>`
        // marker + the space are reserved on the left). Headers use
        // `cols - OUTER_PAD`.
        let name_wrap_w = cols.saturating_sub(OUTER_PAD).saturating_sub(AGENT_INDENT);
        let (lines, agent_row_starts) =
            build_lines(&groups, &flags, name_wrap_w, &self.sessions, &session);

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
                Line::Header { label, unrouted_in_group, open_elsewhere, switch_to } => {
                    let is_active = !session.is_empty() && label.as_str() == session.as_str();
                    if let Some(name) = switch_to {
                        self.row_ranges
                            .push((visible_row, RowTarget::Session(name.clone())));
                    }
                    render_header(
                        label,
                        *unrouted_in_group,
                        *open_elsewhere,
                        is_active,
                        colors,
                        cols,
                    )
                },
                Line::AgentRow {
                    sid,
                    status,
                    text,
                    needs_attention,
                    unrouted,
                    is_in_view,
                    active,
                    is_first_wrap_line,
                    is_child,
                    is_last_child,
                    is_bg,
                } => {
                    let selected = selected_sid.as_deref() == Some(sid.as_str());
                    self.row_ranges
                        .push((visible_row, RowTarget::Agent(sid.clone())));
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
                        *active,
                        *is_first_wrap_line,
                        *is_child,
                        *is_last_child,
                        *is_bg,
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
        let follow = std::mem::take(&mut self.follow_selection);
        let range = self
            .selected_idx
            .filter(|_| follow)
            .and_then(|sel| agent_row_starts.get(sel).copied());
        if let Some((start, end_exclusive)) = range {
            if start < self.scroll_offset {
                self.scroll_offset = start.saturating_sub(1); // keep header in view if possible
            } else if end_exclusive > self.scroll_offset + rows {
                self.scroll_offset = end_exclusive.saturating_sub(rows);
            }
        }
        // Also the landing place for a wheel scroll, which adds optimistically
        // without knowing the line count or viewport height.
        self.scroll_offset = self.scroll_offset.min(lines.len().saturating_sub(rows));
    }
}

/// One section of rows — either a zellij session's interactive agents, the
/// sessionless bucket, or the decoupled background-agents section.
struct Group {
    label: GroupLabel,
    rows: Vec<Row>,
    /// Earliest agent `started_at_ms` in the group — the session's creation
    /// time. Used as the group's sort key so its slot stays put as later
    /// agents come and go.
    created_ms: i64,
    any_unrouted: bool,
}

/// A single display row: an agent plus its place in the parent/child tree.
#[derive(Clone)]
struct Row {
    agent: Agent,
    /// True for a nested background-agent child (rendered indented under its
    /// parent with a `└─`/`├─` connector, single-line).
    is_child: bool,
    /// Last child of its parent → `└─`, otherwise `├─`. Meaningless when
    /// `!is_child`.
    is_last_child: bool,
    /// The session to focus when this row is clicked: itself for an interactive
    /// agent. `None` → not clickable (a background agent — no pane to focus).
    focus_sid: Option<String>,
}

#[derive(Clone)]
enum GroupLabel {
    Session(String),
    Sessionless,
    /// All background (`run_in_background`) agents. They're daemon-owned, have no
    /// pane of their own, and are reached through Claude's agent view rather than
    /// by focusing a pane here — so they're decoupled into their own section and
    /// rendered select-only.
    BackgroundAgents,
}

impl GroupLabel {
    fn as_str(&self) -> &str {
        match self {
            GroupLabel::Session(s) => s.as_str(),
            GroupLabel::Sessionless => SESSIONLESS_LABEL,
            GroupLabel::BackgroundAgents => "bg agents",
        }
    }
}

#[derive(Clone, Copy)]
struct Flags {
    unrouted: bool,
    needs_attention: bool,
    is_in_view: bool,
    /// False for tracked-but-not-active agents — rendered dim with a `†`
    /// marker, no in-view/alert tint.
    active: bool,
    /// A background (`run_in_background`) agent — rendered with a neutral `∙`
    /// marker instead of `>`/`✗`, and never clickable (no pane to focus).
    is_bg: bool,
}

enum Line {
    /// Blank breathing room above non-first groups.
    Padding,
    Header {
        label: String,
        unrouted_in_group: bool,
        /// Another terminal window is attached to this session.
        open_elsewhere: bool,
        /// The session to switch to when this header is clicked. `None` — an
        /// inert header — for the session we're already in, the sessionless /
        /// bg-agents pseudo-groups, and a session that only exists in the
        /// readmodel (switching to a name with no live session would silently
        /// create one).
        switch_to: Option<String>,
    },
    AgentRow {
        sid: String,
        status: AgentStatus,
        text: String,
        needs_attention: bool,
        unrouted: bool,
        is_in_view: bool,
        active: bool,
        /// True on the first wrap-line of an agent — the `>` (or `✗` if
        /// unrouted, `†` if inactive) row marker shows only on this line so
        /// adjacent agents with the same bg tint stay visually distinct.
        is_first_wrap_line: bool,
        /// A nested bg child — the `>` marker column becomes the `└`/`├`
        /// connector (under the parent's `>`) so it reads as subordinate.
        is_child: bool,
        /// Last child of its parent → `└`, otherwise `├`.
        is_last_child: bool,
        /// A background agent — neutral `∙` marker, select-only.
        is_bg: bool,
    },
}

/// Group interactive agents by zellij session (flat top-level rows). Every live
/// zellij session gets a group even with no agents in it, so the panel always
/// lists the full set of sessions. All background (`run_in_background`) agents
/// are decoupled into a single trailing "bg agents" section — they're
/// daemon-owned, have no pane, and aren't clickable (`focus_sid: None`); they're
/// reached through Claude's agent view, not by focusing a pane here. They still
/// render as full top-level rows (wrap to `MAX_NAME_LINES`, not the single-line
/// child form). `display_order` and `build_lines` both consume this, so their
/// row order stays in lockstep.
fn grouped(agents: &[Agent], sessions: &HashMap<String, SessionMeta>) -> Vec<Group> {
    let is_bg = |a: &Agent| a.kind == "bg";

    let mut by_key: BTreeMap<Option<String>, Vec<Agent>> = BTreeMap::new();
    for name in sessions.keys() {
        by_key.entry(Some(name.clone())).or_default();
    }
    for a in agents.iter().filter(|a| !is_bg(a)) {
        by_key.entry(a.zellij_session.clone()).or_default().push(a.clone());
    }

    let mut groups: Vec<Group> = by_key
        .into_iter()
        .map(|(key, mut tops)| {
            tops.sort_by(|a, b| a.started_at_ms.cmp(&b.started_at_ms));
            // Sort key is the zellij SESSION's creation time, so agentless and
            // agentful groups compare on one clock. A session that has left the
            // live list (only tracked-but-inactive agents remain) keeps its slot
            // from its earliest agent instead.
            let created_ms = key
                .as_ref()
                .and_then(|s| sessions.get(s))
                .map(|m| m.created_ms)
                .or_else(|| tops.iter().map(|a| a.started_at_ms).min())
                .unwrap_or(i64::MAX);
            let any_unrouted = tops.iter().any(|a| a.zellij_pane_id.is_none());
            let label = match &key {
                Some(s) => GroupLabel::Session(s.clone()),
                None => GroupLabel::Sessionless,
            };
            let rows = tops
                .into_iter()
                .map(|t| {
                    let sid = t.session_id.clone();
                    Row { agent: t, is_child: false, is_last_child: false, focus_sid: Some(sid) }
                })
                .collect();
            Group { label, rows, created_ms, any_unrouted }
        })
        .collect();

    // Sessionless after named sessions; otherwise oldest-first.
    groups.sort_by(|a, b| match (&a.label, &b.label) {
        (GroupLabel::Sessionless, GroupLabel::Sessionless) => std::cmp::Ordering::Equal,
        (GroupLabel::Sessionless, _) => std::cmp::Ordering::Greater,
        (_, GroupLabel::Sessionless) => std::cmp::Ordering::Less,
        _ => a.created_ms.cmp(&b.created_ms),
    });

    // Background agents → one trailing section, flat top-level rows, select-only.
    let mut bg: Vec<Agent> = agents.iter().filter(|a| is_bg(a)).cloned().collect();
    if !bg.is_empty() {
        bg.sort_by(|a, b| a.started_at_ms.cmp(&b.started_at_ms));
        let created_ms = bg.iter().map(|a| a.started_at_ms).min().unwrap_or(i64::MAX);
        let rows = bg
            .into_iter()
            .map(|a| Row { agent: a, is_child: false, is_last_child: false, focus_sid: None })
            .collect();
        groups.push(Group {
            label: GroupLabel::BackgroundAgents,
            rows,
            created_ms,
            any_unrouted: false,
        });
    }

    groups
}

/// Flatten groups → lines and record (start, end_exclusive) row ranges per
/// agent in display order. Used by scroll logic to keep the selection in
/// the viewport.
fn build_lines(
    groups: &[Group],
    flags: &HashMap<String, Flags>,
    content_w: usize,
    sessions: &HashMap<String, SessionMeta>,
    own_session: &str,
) -> (Vec<Line>, Vec<(usize, usize)>) {
    let mut lines: Vec<Line> = Vec::new();
    let mut agent_rows: Vec<(usize, usize)> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        // Breathing room above every non-first group, agentless ones included:
        // without it adjacent headers stack flush and their filled strips merge
        // into one block instead of reading as separate sessions.
        if gi > 0 {
            lines.push(Line::Padding);
        }
        let meta = sessions.get(g.label.as_str());
        let clients = meta.map(|m| m.clients).unwrap_or(0);
        // Discount our own client so our session flags only when a SECOND window
        // also has it attached. The sessionless / bg-agents labels never match a
        // real session, so they land on 0 and never flag.
        let is_own = !own_session.is_empty() && g.label.as_str() == own_session;
        lines.push(Line::Header {
            label: g.label.as_str().to_string(),
            unrouted_in_group: g.any_unrouted,
            open_elsewhere: clients.saturating_sub(is_own as usize) > 0,
            switch_to: match &g.label {
                GroupLabel::Session(name) if !is_own && meta.is_some() => Some(name.clone()),
                _ => None,
            },
        });
        for row in &g.rows {
            let a = &row.agent;
            let f = flags.get(&a.session_id).copied().unwrap_or(Flags {
                unrouted: false,
                needs_attention: false,
                is_in_view: false,
                active: true,
                is_bg: false,
            });
            let name = if a.name.trim().is_empty() {
                a.session_id.get(..8).unwrap_or(&a.session_id).to_string()
            } else {
                a.name.trim().to_string()
            };
            // Children render single-line, name aligned under the parent's
            // name; the `└`/`├` connector lives in the marker columns (rendered
            // by render_agent_line), not in the text. Parents wrap to MAX_NAME_LINES.
            let max_lines = if row.is_child { MAX_CHILD_NAME_LINES } else { MAX_NAME_LINES };
            let wrapped = wrap_text(&name, content_w, max_lines);
            let start = lines.len();
            for (i, w) in wrapped.into_iter().enumerate() {
                lines.push(Line::AgentRow {
                    sid: a.session_id.clone(),
                    status: a.status.clone(),
                    text: w,
                    needs_attention: f.needs_attention,
                    unrouted: f.unrouted,
                    is_in_view: f.is_in_view,
                    active: f.active,
                    is_first_wrap_line: i == 0,
                    is_child: row.is_child,
                    is_last_child: row.is_last_child,
                    is_bg: f.is_bg,
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
    open_elsewhere: bool,
    is_active: bool,
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
    let elsewhere_suffix = if open_elsewhere && text_budget >= 2 {
        text_budget = text_budget.saturating_sub(2);
        true
    } else {
        false
    };
    // The header of the session the user is currently in glows with the
    // theme's active-tab green (matching the in-view agent tint), so "you are
    // here" is obvious; other sessions keep the neutral header strip.
    // A session another window holds takes the busy-magenta on the neutral strip
    // — the one hue in the palette that isn't already spoken for by in-view green
    // or attention yellow, so "someone else has this" can't be misread as either.
    let (h_fg, h_bg) = if is_active {
        (colors.on_color, colors.green)
    } else if open_elsewhere {
        (colors.magenta, colors.header_bg)
    } else {
        (colors.text, colors.header_bg)
    };
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
    if elsewhere_suffix {
        body.push_str(
            &style!(h_fg, h_bg)
                .bold()
                .paint(format!(" {OPEN_ELSEWHERE}"))
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
    active: bool,
    is_first_wrap_line: bool,
    is_child: bool,
    is_last_child: bool,
    is_bg: bool,
    selected: bool,
    spinner: Option<char>,
    colors: &AgentColors,
    cols: usize,
) -> String {
    // Tracked-but-not-active rows carry no live signal — they render dim with a
    // `†` marker and none of the in-view/alert tints. `dead` short-circuits all
    // of those so a frozen busy/waiting/unknown status doesn't paint a stale hue.
    let dead = !active;
    // Per-column priority stack (see the palette block at the top of this
    // file). `alert` = needs-attention / waiting / unknown; it shows yellow,
    // or red when the status itself is unknown.
    let is_unknown = !dead && matches!(status, AgentStatus::Unknown(_));
    let alert = !dead && (needs_attention || matches!(status, AgentStatus::Waiting) || is_unknown);
    let alert_color = if is_unknown { colors.error } else { colors.yellow };
    let in_view = is_in_view && !dead;

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
    let mid_bg = if in_view {
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
    } else if in_view {
        colors.green
    } else {
        colors.bar_bg
    };

    // Name fg: selection overrides the status hue so the body always reads as
    // selected-fg-on-selected-bg (a deliberately high-contrast pair). Otherwise
    // it encodes status, choosing a hue that reads on the body bg — busy's
    // magenta reads on every bg, idle/waiting take on-colour on a tinted body.
    let body_tinted = alert || in_view;
    let name_fg = if selected {
        colors.selected_fg
    } else if dead {
        colors.text // muted via `dim` below
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
    let bold = selected || in_view || alert;
    // Dead rows render faint (the chosen "dim text" look). A selected dead row
    // stays full-strength so the cursor is unmistakable.
    let dim = dead && !selected;
    let paint = |fg: PaletteColor, bg: PaletteColor, s: String| {
        let mut style = style!(fg, bg);
        if bold {
            style = style.bold();
        }
        if dim {
            style = style.dimmed();
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
    } else if is_child {
        // Connector sits in the marker column, directly under the parent's `>`.
        if is_last_child { '\u{2514}' } else { '\u{251c}' } // └ / ├
    } else if dead {
        '\u{2020}' // † — tracked-but-not-active
    } else if is_bg {
        '\u{2219}' // ∙ — background agent: select-only, no pane to route to
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
    let marker_fg = if dead {
        if selected { colors.selected_fg } else { name_fg }
    } else if unrouted && !is_bg && spinner.is_none() {
        if in_view || alert { colors.on_color } else { colors.error }
    } else if selected {
        colors.selected_fg
    } else if in_view || alert {
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

    // Col 2 is the connector's horizontal arm (`─`, same fg as the marker) on a
    // child's first line, else a plain space.
    let (col2_char, col2_fg) = if is_child && is_first_wrap_line {
        ("\u{2500}".to_string(), marker_fg)
    } else {
        (" ".to_string(), name_fg)
    };

    let mut row = String::new();
    row.push_str(&paint(name_fg, col0_bg, " ".to_string())); // col 0
    row.push_str(&paint(marker_fg, mid_bg, marker_char.to_string())); // col 1: marker / connector
    row.push_str(&paint(col2_fg, mid_bg, col2_char)); // col 2: space or `─` arm
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
                && x.active == y.active
        })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
