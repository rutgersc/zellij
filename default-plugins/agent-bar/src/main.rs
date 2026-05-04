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

use ansi_term::ANSIString;
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
    /// Per-claude-session: the highest `attention_at_ms` the user has been
    /// shown. `agent.attention_at_ms > seen_at[claude_id]` means there's an
    /// unseen attention event. In-memory only — clears on plugin reload.
    seen_at: HashMap<String, i64>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        set_selectable(false);
        subscribe(&[
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::Timer,
            EventType::Mouse,
            EventType::PermissionRequestResult,
            EventType::RunCommandResult,
        ]);
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
                            LoadState::Polling {
                                path: loc.wasi_path,
                                last: None,
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
                changed
            },
            Event::Timer(_) => {
                let changed = if let LoadState::Polling { path, last } = &mut self.load {
                    let new = agents::read(path);
                    let changed = !same_result(last.as_ref(), &new);
                    *last = Some(new);
                    changed
                } else {
                    false
                };
                set_timeout(POLL_SECS);
                changed
            },
            Event::Mouse(Mouse::LeftClick(_, col)) => {
                let col = col as usize;
                self.last_click = Some(match self
                    .cell_ranges
                    .iter()
                    .find(|(start, end, _)| col >= *start && col < *end)
                {
                    Some((_, _, sid)) => {
                        let label = sid.get(..8).unwrap_or(sid).to_string();
                        let mut ctx = BTreeMap::new();
                        ctx.insert("click_label".into(), label.clone());
                        run_command(&["mux", "focus-agent", sid], ctx);
                        ClickFeedback::Pending { label }
                    },
                    None => ClickFeedback::Missed { col },
                });
                true
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
        match &self.load {
            LoadState::AwaitingPermission => {
                print_diag(bg, DIM_COLOR, "awaiting permission grant");
            },
            LoadState::NoHomeInEnv => {
                print_diag(bg, ERROR_COLOR, "$HOME / $USERPROFILE not in session env");
            },
            LoadState::Polling { path, last } => match last {
                None => print_diag(bg, DIM_COLOR, &format!("reading {}", path.display())),
                Some(ReadResult::Missing) => {
                    print_diag(bg, ERROR_COLOR, &format!("missing: {}", path.display()))
                },
                Some(ReadResult::Unreadable(e)) => {
                    print_diag(bg, ERROR_COLOR, &format!("unreadable {}: {e}", path.display()))
                },
                Some(ReadResult::ParseError(e)) => {
                    print_diag(bg, ERROR_COLOR, &format!("parse error: {e}"))
                },
                Some(ReadResult::Ok(agents)) if agents.is_empty() => {
                    print_diag(bg, DIM_COLOR, &format!("no agents @ {}", path.display()))
                },
                Some(ReadResult::Ok(agents)) => {
                    let session = self.mode_info.session_name.as_deref().unwrap_or("");
                    let here = active_tab_pane_ids(&self.pane_manifest, self.active_tab_idx);
                    // Drop seen entries for claude sessions that no longer
                    // exist in the snapshot, so the map stays bounded.
                    let live: HashSet<&str> =
                        agents.iter().map(|a| a.session_id.as_str()).collect();
                    self.seen_at.retain(|k, _| live.contains(k.as_str()));
                    render_agents(
                        agents,
                        &mut self.cell_ranges,
                        self.last_click.as_ref(),
                        &mut self.seen_at,
                        bg,
                        cols,
                        session,
                        &here,
                    );
                },
            },
        }
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

fn render_agents(
    agents: &[Agent],
    cell_ranges: &mut Vec<(usize, usize, String)>,
    last_click: Option<&ClickFeedback>,
    seen_at: &mut HashMap<String, i64>,
    bg: PaletteColor,
    cols: usize,
    current_session: &str,
    here: &HashSet<u32>,
) {
    // Match compact-bar's leading inset so the first cell doesn't ride the
    // left border. Reserve room on the right for click feedback too.
    const LEFT_INSET: usize = 1;
    let feedback = last_click.map(format_feedback);
    let feedback_width = feedback.as_ref().map(|(s, _)| s.width() + 1).unwrap_or(0);
    let cell_budget = cols.saturating_sub(LEFT_INSET).saturating_sub(feedback_width);

    // Compute per-agent flags up front. `check_and_mark_attention` mutates
    // `seen_at` for any agent currently in the active tab — the presence
    // trigger.
    let flags: Vec<AgentFlags> = agents
        .iter()
        .map(|a| {
            let unrouted = a.zellij_pane_id.is_none();
            let in_here = is_in_current_tab(a, current_session, here);
            let needs_attention = check_and_mark_attention(a, in_here, seen_at);
            AgentFlags { unrouted, in_current_tab: in_here, needs_attention }
        })
        .collect();

    let mut cells: Vec<Cell> = agents.iter().map(Cell::from).collect();
    fit_to_width(&flags, &mut cells, cell_budget);

    let mut out = String::new();
    out.push(' ');
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
        out.push_str(&render_cell(agent, cell, flag, bg));
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

fn print_diag(bg: PaletteColor, fg: PaletteColor, msg: &str) {
    let s: ANSIString<'static> = style!(fg, bg).paint(format!(" {msg}"));
    print!("{s}\u{1b}[0K");
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
    bar_bg: PaletteColor,
) -> String {
    let fg = match agent.status {
        AgentStatus::Busy => BUSY_FG,
        AgentStatus::Idle => IDLE_FG,
    };
    // Cell bg flips to the attention fill when there's an unseen event.
    let cell_bg = if flag.needs_attention { ATTENTION_BG } else { bar_bg };
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

/// Decide whether this agent has an unseen attention event, and apply the
/// presence-based "mark seen" rule: an agent currently in the active tab is
/// considered seen, so its `seen_at` is bumped to the latest attention
/// timestamp (or 0 if none) and the indicator never fires while it's here.
///
/// Swapping in a keybinding-driven trigger is one new event handler that
/// also writes to `seen_at` — same data, different cause.
fn check_and_mark_attention(
    agent: &Agent,
    in_current_tab: bool,
    seen_at: &mut HashMap<String, i64>,
) -> bool {
    if in_current_tab {
        seen_at.insert(agent.session_id.clone(), agent.attention_at_ms);
        return false;
    }
    if agent.attention_at_ms == 0 {
        return false;
    }
    let last_seen = seen_at.get(&agent.session_id).copied().unwrap_or(0);
    agent.attention_at_ms > last_seen
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
