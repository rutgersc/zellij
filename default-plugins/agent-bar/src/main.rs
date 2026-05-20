//! Persistent strip showing every running Claude agent across every zellij
//! session. Polls `~/.claude/readmodel/agents.json` every `POLL_SECS`.
//!
//! Layout: `folder: name  folder: name  folder: name ✗  ...`
//!   text fg carries status — green=busy, amber=idle.
//!   ✗ suffix = unrouted (no zellij_pane_id).
//!   Cells render in the order the snapshot lists them.
//! Click → `mux focus-agent <session_id>`.
//!
//! Every distinguishable state — awaiting permission, no $HOME in env, file
//! missing/unreadable/unparseable, empty snapshot, populated snapshot — has
//! its own visible rendering. The bar is also the diagnostics surface.

mod agents;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;
use zellij_tile_utils::style;

use crate::agents::{Agent, AgentStatus, ReadResult};

const POLL_SECS: f64 = 1.5;
/// Hard cap on rendered name width — favours fitting more agents over
/// reading any one in full. The full name is on the snapshot anyway.
const MAX_NAME_WIDTH: usize = 18;
// Color carries activity (green=busy, white=idle), underline carries
// location (in the current tab or not). Two orthogonal channels.
const BUSY_FG: PaletteColor = PaletteColor::EightBit(46); // bright green
const IDLE_FG: PaletteColor = PaletteColor::EightBit(231); // near-white
const UNROUTED_FG: PaletteColor = PaletteColor::EightBit(196); // bright red
const ATTENTION_BG: PaletteColor = PaletteColor::EightBit(130); // dark orange — needs attention
const SELECTED_BG: PaletteColor = PaletteColor::EightBit(24); // dark blue — keyboard cursor
const DIM_COLOR: PaletteColor = PaletteColor::EightBit(8); // dim grey
const ERROR_COLOR: PaletteColor = PaletteColor::EightBit(9); // bright red
const UNROUTED: &str = "\u{2717}";
const SEPARATOR: &str = "  \u{00B7}  "; // "  ·  "
const SEPARATOR_WIDTH: usize = 5;

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

/// Feedback for the most recent click. Persists until the next click so the
/// user can see what happened (was it received? did the spawn succeed? what
/// did the command print?).
enum ClickFeedback {
    /// Click hit-tested but no cell — nothing was dispatched.
    Missed { col: usize },
    Pending { label: String },
    Done {
        label: String,
        exit: Option<i32>,
        stderr: String,
    },
}

#[derive(Default)]
struct State {
    load: LoadState,
    mode_info: ModeInfo,
    /// Click hit-test ranges, rebuilt every render: (col_start, col_end, session_id).
    cell_ranges: Vec<(usize, usize, String)>,
    last_click: Option<ClickFeedback>,
    /// PaneManifest + active tab index let us decide which agents are "here".
    pane_manifest: PaneManifest,
    active_tab_idx: Option<usize>,
    /// Latest scan of `agent-seen-events/` keyed by claude_id. Refreshed
    /// each Timer tick alongside the readmodel.
    seen_disk: HashMap<String, i64>,
    /// Optimistic mark-seen overlay. After firing
    /// `agent-seen-events/<sid>.json` we patch this map so the next render
    /// sees the agent as "seen" before the file write propagates to the
    /// next disk scan. `effective_seen = max(seen_disk, overlay)`.
    seen_overlay: HashMap<String, i64>,
    /// Where the seen-event files live, set after permission grant when the
    /// WASI host root is known. Same dir is read for `seen_disk` and
    /// written for mark-seen.
    seen_events_dir: Option<PathBuf>,
    /// Keyboard cursor over the rendered cells. Clamped to the agent count
    /// on every render. `None` = no cursor visible. Auto-set on focus gain
    /// (PaneUpdate), cleared on focus loss, acted on by Enter.
    selected_idx: Option<usize>,
    /// Captured in `load()` so we can identify ourselves in PaneManifest.
    own_plugin_id: Option<u32>,
    /// True when our pane was the focused one in the last PaneUpdate.
    /// Edge-triggered: transitions drive selected_idx changes.
    was_focused: bool,
    /// Most recent non-plugin focused pane in the active tab — the "came
    /// from" target. Used to (a) auto-position the keyboard cursor on focus
    /// gain and (b) jump back on Esc.
    last_external_focused_pane: Option<u32>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        // Stay selectable for two reasons:
        //   1. zellij #4749 — non-selectable plugins can't receive the y/n
        //      permission prompt grant.
        //   2. Selectable panes receive Key events, which we use for
        //      keyboard activation (Left/Right to move cursor, Enter to
        //      focus the agent's session).
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
                    // Stay selectable after the grant — see `load()`. The
                    // bar joining the pane navigation cycle is the cost we
                    // pay for keyboard-driven agent switching.
                    let env = get_session_environment_variables();
                    self.load = match agents::locate_snapshot(&env) {
                        Some(loc) => {
                            change_host_folder(loc.host_root);
                            // agent-seen-events is plugin-owned: we both
                            // read this dir to overlay seen state and write
                            // here on mark-seen. Daemon does not consume.
                            self.seen_events_dir = Some(PathBuf::from(
                                "/host/.claude/custom-state/agent-seen-events",
                            ));
                            self.refresh_seen_disk();
                            // Synchronous initial read so the first render
                            // already has agents. zellij creates a fresh
                            // plugin instance *per client* on every attach
                            // — see zellij-server/src/plugins/wasm_bridge.rs
                            // `add_client` (which `mux focus-agent` triggers
                            // via switch-session → detach + attach). Every
                            // session switch lands on a freshly-loaded
                            // instance; without this read, that instance
                            // would render the "reading…" diagnostic until
                            // its first Timer fires ~1.5s later.
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
            Event::Mouse(Mouse::LeftClick(_, col)) => {
                let col = col as usize;
                let hit = self
                    .cell_ranges
                    .iter()
                    .find(|(start, end, _)| col >= *start && col < *end)
                    .map(|(_, _, sid)| sid.clone());
                self.last_click = Some(match hit {
                    Some(sid) => {
                        self.acknowledge_agent(&sid);
                        let label = self.dispatch_focus_agent(&sid);
                        ClickFeedback::Pending { label }
                    },
                    None => ClickFeedback::Missed { col },
                });
                true
            },
            Event::Key(key) => {
                let len = self.current_agents().len();
                let no_mods = key.has_no_modifiers();
                // j/k are vertical in vim but the bar is 1D — alias them as
                // next/prev (j = forward = right, k = back = left).
                let go_left = matches!(key.bare_key, BareKey::Left | BareKey::Char('h' | 'k')) && no_mods;
                let go_right = matches!(key.bare_key, BareKey::Right | BareKey::Char('l' | 'j')) && no_mods;
                let go_first = matches!(key.bare_key, BareKey::Home | BareKey::Char('0' | '^')) && no_mods;
                let go_last = matches!(key.bare_key, BareKey::End | BareKey::Char('$' | 'G')) && no_mods;
                let activate = matches!(key.bare_key, BareKey::Enter) && no_mods;
                let cancel = matches!(key.bare_key, BareKey::Esc | BareKey::Char('q')) && no_mods;
                if go_left && len > 0 {
                    self.selected_idx = Some(match self.selected_idx {
                        Some(i) if i > 0 => i - 1,
                        _ => 0,
                    });
                    return true;
                }
                if go_right && len > 0 {
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
                        .and_then(|i| self.current_agents().get(i))
                        .map(|a| a.session_id.clone())
                    {
                        self.acknowledge_agent(&sid);
                        let label = self.dispatch_focus_agent(&sid);
                        self.last_click = Some(ClickFeedback::Pending { label });
                        return true;
                    }
                    return false;
                }
                if cancel {
                    // Send focus back to where we came from. If that pane
                    // is gone, fall back to spatially-above.
                    let still_alive = self.last_external_focused_pane.filter(|pid| {
                        self.pane_manifest
                            .panes
                            .values()
                            .flatten()
                            .any(|p| !p.is_plugin && p.id == *pid)
                    });
                    if let Some(pid) = still_alive {
                        focus_terminal_pane(pid, false, false);
                    } else {
                        move_focus(Direction::Up);
                    }
                    return true;
                }
                false
            },
            Event::RunCommandResult(exit, _stdout, stderr, ctx) => {
                if let Some(label) = ctx.get("click_label") {
                    let stderr = String::from_utf8_lossy(&stderr).into_owned();
                    self.last_click = Some(ClickFeedback::Done {
                        label: label.clone(),
                        exit,
                        stderr,
                    });
                    return true;
                }
                false
            },
            _ => false,
        }
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        self.cell_ranges.clear();
        let bg = self.mode_info.style.colors.text_unselected.background;
        // Decide what to do under the immutable borrow, then either print
        // a diagnostic immediately or fall through to a method call that
        // wants &mut self. Clone the agents Vec out so the &self.load
        // borrow can drop before the cell render begins.
        enum Action {
            Diag(PaletteColor, String),
            // Loud, bold, bright bg — for user-action-required states that
            // must not blend into the bar on dim themes.
            Attention(String),
            Cells(Vec<Agent>),
        }
        let action = match &self.load {
            LoadState::AwaitingPermission => Action::Attention(
                "agent-bar needs permissions — focus this pane and press y \
                 (fullscreen with Ctrl+Shift+; if the prompt is cropped)"
                    .to_string(),
            ),
            LoadState::NoHomeInEnv => Action::Diag(
                ERROR_COLOR,
                "$HOME / $USERPROFILE not in session env".to_string(),
            ),
            LoadState::Polling { path, last } => match last {
                None => Action::Diag(DIM_COLOR, format!("reading {}", path.display())),
                Some(ReadResult::Missing) => {
                    Action::Diag(ERROR_COLOR, format!("missing: {}", path.display()))
                },
                Some(ReadResult::Unreadable(e)) => {
                    Action::Diag(ERROR_COLOR, format!("unreadable {}: {e}", path.display()))
                },
                Some(ReadResult::ParseError(e)) => {
                    Action::Diag(ERROR_COLOR, format!("parse error: {e}"))
                },
                Some(ReadResult::Ok(agents)) if agents.is_empty() => {
                    Action::Diag(DIM_COLOR, format!("no agents @ {}", path.display()))
                },
                Some(ReadResult::Ok(agents)) => Action::Cells(agents.clone()),
            },
        };
        // Column 0 is the focus indicator — a theme-accent block (▌) when
        // our pane is focused, otherwise just a space. Same width either
        // way so col positions stay stable. Attention state skips it; the
        // orange bg already screams "look here".
        let accent = self.mode_info.style.colors.frame_selected.base;
        let lead = focus_lead(self.was_focused, accent, bg);
        match action {
            Action::Diag(fg, msg) => {
                print!("{lead}");
                let s = style!(fg, bg).paint(msg);
                print!("{s}\u{1b}[0K");
            },
            Action::Attention(msg) => {
                let s = style!(IDLE_FG, ATTENTION_BG)
                    .bold()
                    .paint(format!(" {msg} "));
                print!("{s}\u{1b}[0K");
            },
            Action::Cells(agents) => self.render_agent_cells(&agents, bg, cols, &lead),
        }
    }
}

impl State {
    fn current_agents(&self) -> &[Agent] {
        match &self.load {
            LoadState::Polling { last: Some(ReadResult::Ok(a)), .. } => a,
            _ => &[],
        }
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
    /// the first agent. `None` only when there are no agents at all.
    fn initial_selection(&self) -> Option<usize> {
        let agents = self.current_agents();
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

    /// Common dispatch for mouse click and Enter key. Fires
    /// `mux focus-agent <sid>` and returns the truncated label so the
    /// caller can stash it on `last_click` for the run feedback.
    fn dispatch_focus_agent(&self, sid: &str) -> String {
        let label = sid.get(..8).unwrap_or(sid).to_string();
        let mut ctx = BTreeMap::new();
        ctx.insert("click_label".into(), label.clone());
        run_command(&["mux", "focus-agent", sid], ctx);
        label
    }

    /// Effective seen timestamp = max(latest disk scan, optimistic overlay).
    /// The disk scan reflects mark-seen events from any plugin instance or
    /// the `sessions ingest_agent_seen` CLI; the overlay covers the gap
    /// between firing a mark-seen and the next disk scan picking it up.
    fn effective_seen_at(&self, agent: &Agent) -> i64 {
        let disk = self.seen_disk.get(&agent.session_id).copied().unwrap_or(0);
        let overlay = self.seen_overlay.get(&agent.session_id).copied().unwrap_or(0);
        disk.max(overlay)
    }

    /// Acknowledge any unseen attention for `session_id`. Called from the
    /// click and Enter handlers — never auto-fired. Idempotent: if the
    /// agent has no unseen attention, this is a no-op.
    fn acknowledge_agent(&mut self, session_id: &str) {
        let Some(agent) = self.current_agents().iter().find(|a| a.session_id == session_id)
        else { return };
        if agent.attention_at_ms == 0 {
            return;
        }
        let at_ms = agent.attention_at_ms;
        // Optimistic patch first — covers the gap until the next disk scan.
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

    /// Drop overlay entries for dead agents or whose disk-side `seen_at_ms`
    /// already covers our optimistic patch.
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

    /// Refresh `seen_disk` from `agent-seen-events/` and return whether
    /// anything changed. Called on each Timer tick so cross-instance
    /// mark-seen propagates.
    fn refresh_seen_disk(&mut self) -> bool {
        let Some(dir) = &self.seen_events_dir else { return false };
        let new = agents::read_seen_events(dir);
        if new == self.seen_disk {
            return false;
        }
        self.seen_disk = new;
        true
    }

    fn render_agent_cells(&mut self, agents: &[Agent], bg: PaletteColor, cols: usize, lead: &str) {
        self.prune_overlay(agents);
        // Clamp the keyboard cursor — agents come and go between ticks.
        self.selected_idx = self.selected_idx.and_then(|i| {
            agents.len().checked_sub(1).map(|max| i.min(max))
        });

        // Refresh seen state from disk before each render. Each zellij
        // session has its own plugin instance with its own `seen_disk`
        // cache; without this, a session you've just switched to via
        // `mux focus-agent` would render with whatever its last Timer
        // tick saw (up to 1.5s stale) and briefly show the orange before
        // catching up. Directory scan is a few file reads, cheap on each
        // render.
        self.refresh_seen_disk();
        let session = self.mode_info.session_name.as_deref().unwrap_or("").to_string();
        let here = active_tab_pane_ids(&self.pane_manifest, self.active_tab_idx);

        // Pure flag computation — no auto mark-seen. User must click a cell
        // or press Enter on a selected cell to acknowledge. See
        // `acknowledge_agent`.
        let mut flags: Vec<AgentFlags> = Vec::with_capacity(agents.len());
        for agent in agents {
            let unrouted = agent.zellij_pane_id.is_none();
            let in_current_tab = is_in_current_tab(agent, &session, &here);
            let unseen = agent.attention_at_ms > 0
                && agent.attention_at_ms > self.effective_seen_at(agent);
            flags.push(AgentFlags { unrouted, in_current_tab, needs_attention: unseen });
        }

        // Render.
        render_agents(
            agents,
            &flags,
            self.selected_idx,
            &mut self.cell_ranges,
            self.last_click.as_ref(),
            bg,
            cols,
            lead,
        );
    }
}

/// Folder is identity (kept intact); name is detail (truncated aggressively).
struct Cell {
    folder: String,
    name: String,
}

impl Cell {
    fn from(agent: &Agent) -> Self {
        // `agent.cwd` arrives with whichever separator the host wrote (Windows
        // backslashes, POSIX forward slashes). The WASI build of Path treats
        // `\` as a regular char, so we split on both ourselves.
        let folder = agent
            .cwd
            .rsplit(|c| c == '/' || c == '\\')
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();
        let raw_name = if agent.name.trim().is_empty() {
            agent.session_id.get(..8).unwrap_or(&agent.session_id).to_string()
        } else {
            agent.name.trim().to_string()
        };
        Cell { folder, name: ellipsize(&raw_name, MAX_NAME_WIDTH) }
    }

    /// `folder:name` (routed) or `folder✗name` (unrouted) — colon and cross
    /// are mutually exclusive separators, no whitespace around either. When
    /// folder is empty: `name` or `✗name`. Attention prepends `!` to the name.
    fn width(&self, unrouted: bool, needs_attention: bool) -> usize {
        let folder_w = self.folder.width();
        let folder_present = !self.folder.is_empty();
        let mid = match (folder_present, unrouted) {
            (true, true) => UNROUTED.width(),
            (true, false) => 1, // ":"
            (false, true) => UNROUTED.width(),
            (false, false) => 0,
        };
        let attn = if needs_attention { 1 } else { 0 };
        folder_w + mid + attn + self.name.width()
    }

    /// Drop one char from the name (with `…` suffix on first truncation).
    /// Returns false if name is already empty.
    fn shrink(&mut self) -> bool {
        if self.name.is_empty() {
            return false;
        }
        self.name = ellipsize(&self.name, self.name.width().saturating_sub(1));
        true
    }
}

/// Cap `s` to `max` display columns, appending `…` if truncated.
fn ellipsize(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut acc = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.to_string().width();
        if w + cw + 1 > max {
            break;
        }
        acc.push(c);
        w += cw;
    }
    acc.push('…');
    acc
}

/// Pure renderer — flags and mark-seen decisions come from the caller.
fn render_agents(
    agents: &[Agent],
    flags: &[AgentFlags],
    selected_idx: Option<usize>,
    cell_ranges: &mut Vec<(usize, usize, String)>,
    last_click: Option<&ClickFeedback>,
    bg: PaletteColor,
    cols: usize,
    lead: &str,
) {
    // Match compact-bar's leading inset so the first cell doesn't ride the
    // left border. Reserve room on the right for click feedback too.
    const LEFT_INSET: usize = 1;
    let feedback = last_click.map(format_feedback);
    let feedback_width = feedback.as_ref().map(|(s, _)| s.width() + 1).unwrap_or(0);
    let cell_budget = cols.saturating_sub(LEFT_INSET).saturating_sub(feedback_width);

    let mut cells: Vec<Cell> = agents.iter().map(Cell::from).collect();
    fit_to_width(flags, &mut cells, cell_budget);

    let mut out = String::new();
    out.push_str(lead);
    let mut col = LEFT_INSET;
    let cell_limit = LEFT_INSET + cell_budget;
    for (i, ((agent, cell), flag)) in agents.iter().zip(cells.iter()).zip(flags.iter()).enumerate() {
        let width = cell.width(flag.unrouted, flag.needs_attention);
        if col + width > cell_limit {
            break;
        }
        if i > 0 {
            out.push_str(&style!(DIM_COLOR, bg).paint(SEPARATOR).to_string());
            col += SEPARATOR_WIDTH;
        }
        let selected = selected_idx == Some(i);
        out.push_str(&render_cell(agent, cell, flag, selected, bg));
        cell_ranges.push((col, col + width, agent.session_id.clone()));
        col += width;
    }
    if let Some((text, fg)) = feedback {
        let pad = cols.saturating_sub(col).saturating_sub(text.width());
        for _ in 0..pad {
            out.push(' ');
        }
        out.push_str(&style!(fg, bg).paint(text).to_string());
    }
    out.push_str("\u{1b}[0K");
    print!("{out}");
}

fn format_feedback(fb: &ClickFeedback) -> (String, PaletteColor) {
    match fb {
        ClickFeedback::Missed { col } => {
            (format!("click at col {col}: no cell"), DIM_COLOR)
        },
        ClickFeedback::Pending { label } => {
            (format!("running mux focus-agent {label}…"), DIM_COLOR)
        },
        ClickFeedback::Done { label, exit, stderr } => {
            let summary = match exit {
                Some(0) => format!("ok: mux focus-agent {label}"),
                Some(code) => {
                    let first = stderr.lines().next().unwrap_or("").trim();
                    if first.is_empty() {
                        format!("exit {code}: mux focus-agent {label}")
                    } else {
                        format!("exit {code} ({first}): {label}")
                    }
                },
                None => {
                    let first = stderr.lines().next().unwrap_or("spawn failed").trim();
                    format!("spawn failed ({first}): {label}")
                },
            };
            let color = if matches!(exit, Some(0)) { DIM_COLOR } else { ERROR_COLOR };
            (summary, color)
        },
    }
}

fn focus_lead(focused: bool, accent: PaletteColor, bg: PaletteColor) -> String {
    if focused {
        style!(accent, bg).paint("\u{258C}").to_string() // ▌
    } else {
        " ".to_string()
    }
}

/// Round-robin shrink: while total cell+separator width overshoots `cols`,
/// shave the longest *name* by one char. Folders, unrouted mark, and
/// attention prefix are all preserved.
fn fit_to_width(flags: &[AgentFlags], cells: &mut [Cell], cols: usize) {
    let separator_total = cells.len().saturating_sub(1) * SEPARATOR_WIDTH;
    loop {
        let total: usize = flags
            .iter()
            .zip(cells.iter())
            .map(|(f, c)| c.width(f.unrouted, f.needs_attention))
            .sum::<usize>()
            + separator_total;
        if total <= cols {
            return;
        }
        let Some((idx, _)) = cells.iter().enumerate().max_by_key(|(_, c)| c.name.width()) else {
            return;
        };
        if !cells[idx].shrink() {
            return;
        }
    }
}

fn render_cell(
    agent: &Agent,
    cell: &Cell,
    flag: &AgentFlags,
    selected: bool,
    bar_bg: PaletteColor,
) -> String {
    let fg = match agent.status {
        AgentStatus::Busy => BUSY_FG,
        AgentStatus::Idle => IDLE_FG,
    };
    // Selection wins over attention so the keyboard cursor is always
    // unambiguous; the `!` prefix still carries the attention signal.
    let cell_bg = if selected {
        SELECTED_BG
    } else if flag.needs_attention {
        ATTENTION_BG
    } else {
        bar_bg
    };
    let paint = |fg: PaletteColor, text: &str, bold: bool| -> String {
        let mut s = style!(fg, cell_bg);
        if bold {
            s = s.bold();
        }
        if flag.in_current_tab {
            s = s.underline();
        }
        s.paint(text.to_string()).to_string()
    };
    let mut body = String::new();
    if !cell.folder.is_empty() {
        body.push_str(&paint(fg, &cell.folder, true));
        if flag.unrouted {
            body.push_str(&paint(UNROUTED_FG, UNROUTED, true));
        } else {
            body.push_str(&paint(fg, ":", false));
        }
    } else if flag.unrouted {
        body.push_str(&paint(UNROUTED_FG, UNROUTED, true));
    }
    if flag.needs_attention {
        body.push_str(&paint(fg, "!", true));
    }
    body.push_str(&paint(fg, &cell.name, false));
    body
}

fn active_tab_pane_ids(manifest: &PaneManifest, active_tab_idx: Option<usize>) -> HashSet<u32> {
    let Some(idx) = active_tab_idx else { return HashSet::new() };
    manifest
        .panes
        .get(&idx)
        .into_iter()
        .flatten()
        .filter(|p| !p.is_plugin)
        .map(|p| p.id)
        .collect()
}

fn is_in_current_tab(agent: &Agent, current_session: &str, here: &HashSet<u32>) -> bool {
    agent.zellij_session.as_deref() == Some(current_session)
        && matches!(agent.zellij_pane_id, Some(id) if here.contains(&id))
}

struct AgentFlags {
    unrouted: bool,
    in_current_tab: bool,
    needs_attention: bool,
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
                && x.name == y.name
                && x.cwd == y.cwd
        })
}
