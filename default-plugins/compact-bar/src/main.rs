mod action_types;
mod agents;
mod clipboard_utils;
mod keybind_utils;
mod line;
mod tab;
mod tooltip;

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::convert::TryInto;
use std::path::PathBuf;

use tab::get_tab_to_focus;
use zellij_tile::prelude::*;

use crate::agents::{PaneAgent, TabFlag};
use crate::clipboard_utils::{system_clipboard_error, text_copied_hint};
use crate::line::tab_line;
use crate::tab::tab_style;
use crate::tooltip::TooltipRenderer;

const AGENT_POLL_SECS: f64 = 1.5;

static ARROW_SEPARATOR: &str = "";

const CONFIG_IS_TOOLTIP: &str = "is_tooltip";
const CONFIG_TOGGLE_TOOLTIP_KEY: &str = "tooltip";
const MSG_TOGGLE_TOOLTIP: &str = "toggle_tooltip";
const MSG_TOGGLE_PERSISTED_TOOLTIP: &str = "toggle_persisted_tooltip";
const MSG_LAUNCH_TOOLTIP: &str = "launch_tooltip_if_not_launched";

#[derive(Debug, Default)]
pub struct LinePart {
    part: String,
    len: usize,
    tab_index: Option<usize>,
}

#[derive(Default)]
struct State {
    // Tab state
    tabs: Vec<TabInfo>,
    active_tab_idx: usize,

    // Display state
    mode_info: ModeInfo,
    tab_line: Vec<LinePart>,
    display_area_rows: usize,
    display_area_cols: usize,

    // Clipboard state
    text_copy_destination: Option<CopyDestination>,
    display_system_clipboard_failure: bool,

    // Plugin configuration
    config: BTreeMap<String, String>,
    own_plugin_id: Option<u32>,
    toggle_tooltip_key: Option<String>,

    // Tooltip state
    is_tooltip: bool,
    tooltip_is_active: bool,
    persist: bool,
    is_first_run: bool,
    own_tab_index: Option<usize>,
    own_client_id: u16,

    // Keybinding cache
    cached_keybinds: KeybindsVec,

    // Agent indicator — reads custom-state/agent-readmodel.json (attention)
    // and custom-state/agent-seen-events/ (acknowledgements written by
    // agent-bar). compact-bar is purely a consumer here — it never writes
    // mark-seen events itself; the agent-bar click/Enter path is the only
    // acknowledgement entrypoint.
    agent_snapshot_host_path: Option<PathBuf>,
    agent_seen_events_dir: Option<PathBuf>,
    pane_manifest: PaneManifest,
    pane_agents: HashMap<u32, PaneAgent>,
    tab_flags: HashMap<usize, TabFlag>,
    /// Latest disk scan of agent-seen-events/, refreshed on Timer + on
    /// permission grant.
    seen_disk: HashMap<String, i64>,
}

struct TabRenderData {
    tabs: Vec<LinePart>,
    active_tab_index: usize,
    active_swap_layout_name: Option<String>,
    is_swap_layout_dirty: bool,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        let plugin_ids = get_plugin_ids();
        self.own_plugin_id = Some(plugin_ids.plugin_id);
        self.own_client_id = plugin_ids.client_id;
        self.initialize_configuration(configuration);
        self.setup_subscriptions();
        self.configure_keybinds();
        self.setup_agent_indicator();
    }

    fn update(&mut self, event: Event) -> bool {
        self.is_first_run = false;

        match event {
            Event::InitialKeybinds(keybinds) => {
                self.cached_keybinds = keybinds;
                if !self.cached_keybinds.is_empty() {
                    self.mode_info.keybinds = self.cached_keybinds.clone();
                }
                true
            },
            Event::ModeUpdate(mut mode_info) => {
                if mode_info.keybinds.is_empty() && !self.cached_keybinds.is_empty() {
                    mode_info.keybinds = self.cached_keybinds.clone();
                } else if !mode_info.keybinds.is_empty() {
                    self.cached_keybinds = mode_info.keybinds.clone();
                }
                self.handle_mode_update(mode_info)
            },
            Event::TabUpdate(tabs) => self.handle_tab_update(tabs),
            Event::PaneUpdate(pane_manifest) => self.handle_pane_update(pane_manifest),
            Event::Timer(_) => self.handle_agent_timer(),
            Event::PermissionRequestResult(_) => self.handle_permission_grant(),
            Event::Mouse(mouse_event) => {
                self.handle_mouse_event(mouse_event);
                false
            },
            Event::CopyToClipboard(copy_destination) => {
                self.handle_clipboard_copy(copy_destination)
            },
            Event::SystemClipboardFailure => self.handle_clipboard_failure(),
            Event::InputReceived => self.handle_input_received(),
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if self.is_tooltip && message.is_private {
            self.handle_tooltip_pipe(message);
        } else if message.name == MSG_TOGGLE_TOOLTIP
            && message.is_private
            && self.toggle_tooltip_key.is_some()
            // only launch once per plugin instance
            && self.own_tab_index == Some(self.active_tab_idx.saturating_sub(1))
            // only launch once per client of plugin instance
            && Some(format!("{}", self.own_client_id)) == message.payload
        {
            self.toggle_persisted_tooltip(self.mode_info.mode);
        }
        false
    }

    fn render(&mut self, rows: usize, cols: usize) {
        if self.is_tooltip {
            self.render_tooltip(rows, cols);
        } else {
            self.render_tab_line(cols);
        }
    }
}

impl State {
    fn initialize_configuration(&mut self, configuration: BTreeMap<String, String>) {
        self.config = configuration.clone();
        self.is_tooltip = self.parse_bool_config(CONFIG_IS_TOOLTIP, false);

        if !self.is_tooltip {
            if let Some(tooltip_toggle_key) = configuration.get(CONFIG_TOGGLE_TOOLTIP_KEY) {
                self.toggle_tooltip_key = Some(tooltip_toggle_key.clone());
            }
        }

        if self.is_tooltip {
            self.is_first_run = true;
        }
    }

    fn setup_subscriptions(&self) {
        // Start selectable so the permission prompt is mouse-focusable.
        // Switched to set_selectable(false) in handle_permission_grant once
        // the user has answered. Bug ref:
        // https://github.com/zellij-org/zellij/issues/4749
        let events = if self.is_tooltip {
            vec![
                EventType::ModeUpdate,
                EventType::TabUpdate,
                EventType::InitialKeybinds,
            ]
        } else {
            vec![
                EventType::TabUpdate,
                EventType::PaneUpdate,
                EventType::ModeUpdate,
                EventType::Mouse,
                EventType::CopyToClipboard,
                EventType::InputReceived,
                EventType::SystemClipboardFailure,
                EventType::InitialKeybinds,
                EventType::Timer,
                EventType::PermissionRequestResult,
            ]
        };

        subscribe(&events);
    }

    fn configure_keybinds(&self) {
        if !self.is_tooltip && self.toggle_tooltip_key.is_some() {
            if let Some(toggle_key) = &self.toggle_tooltip_key {
                reconfigure(
                    bind_toggle_key_config(toggle_key, self.own_client_id),
                    false,
                );
            }
        }
    }

    fn parse_bool_config(&self, key: &str, default: bool) -> bool {
        self.config
            .get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    // Event handlers
    fn handle_mode_update(&mut self, mode_info: ModeInfo) -> bool {
        let should_render = self.mode_info != mode_info;
        let old_mode = self.mode_info.mode;
        let new_mode = mode_info.mode;
        let base_mode = mode_info.base_mode.unwrap_or(InputMode::Normal);

        self.mode_info = mode_info;

        if self.is_tooltip {
            self.handle_tooltip_mode_update(old_mode, new_mode, base_mode);
        } else {
            self.handle_main_mode_update(new_mode, base_mode);
        }

        should_render
    }

    fn handle_main_mode_update(&self, new_mode: InputMode, base_mode: InputMode) {
        if self.toggle_tooltip_key.is_some()
            && new_mode != base_mode
            && !self.is_restricted_mode(new_mode)
        {
            self.launch_tooltip_if_not_launched(new_mode);
        }
    }

    fn handle_tooltip_mode_update(
        &mut self,
        old_mode: InputMode,
        new_mode: InputMode,
        base_mode: InputMode,
    ) {
        if !self.persist && (new_mode == base_mode || self.is_restricted_mode(new_mode)) {
            close_self();
        } else if new_mode != old_mode || self.persist {
            self.update_tooltip_for_mode_change(new_mode);
        }
    }

    fn handle_tab_update(&mut self, tabs: Vec<TabInfo>) -> bool {
        self.update_display_area(&tabs);

        if let Some(active_tab_index) = tabs.iter().position(|t| t.active) {
            let active_tab_idx = active_tab_index + 1; // Convert to 1-based indexing
            let mut should_render = self.active_tab_idx != active_tab_idx || self.tabs != tabs;
            let active_tab_changed = self.active_tab_idx != active_tab_idx;

            if self.is_tooltip && self.active_tab_idx != active_tab_idx {
                self.move_tooltip_to_new_tab(active_tab_idx);
            }

            self.active_tab_idx = active_tab_idx;
            self.tabs = tabs;
            // Active tab change resets mark-seen state for the new tab's
            // agents — recompute so the orange tint clears immediately.
            if active_tab_changed && !self.is_tooltip {
                should_render |= self.recompute_tab_flags();
            }
            should_render
        } else {
            false
        }
    }

    fn handle_pane_update(&mut self, pane_manifest: PaneManifest) -> bool {
        let mut should_render = false;
        if self.toggle_tooltip_key.is_some() {
            let previous_tooltip_state = self.tooltip_is_active;
            self.tooltip_is_active = self.detect_tooltip_presence(&pane_manifest);
            self.own_tab_index = self.find_own_tab_index(&pane_manifest);
            should_render = previous_tooltip_state != self.tooltip_is_active;
        }
        if !self.is_tooltip && self.pane_manifest != pane_manifest {
            self.pane_manifest = pane_manifest;
            should_render |= self.recompute_tab_flags();
        }
        should_render
    }

    fn setup_agent_indicator(&mut self) {
        if self.is_tooltip {
            return;
        }
        // Builtin plugins auto-grant via is_builtin(); a file:// load needs
        // explicit grants. PermissionRequestResult fires either way, which is
        // where we kick off the snapshot polling.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::FullHdAccess,
            PermissionType::ReadSessionEnvironmentVariables,
        ]);
    }

    fn handle_permission_grant(&mut self) -> bool {
        if self.is_tooltip || self.agent_snapshot_host_path.is_some() {
            return false;
        }
        // Permission settled — drop out of the navigable focus rotation.
        set_selectable(false);
        let env = get_session_environment_variables();
        if let Some(loc) = agents::locate_snapshot(&env) {
            change_host_folder(loc.host_root);
            self.agent_snapshot_host_path = Some(loc.wasi_path);
            let seen_dir = PathBuf::from("/host/.claude/custom-state/agent-seen-events");
            self.seen_disk = agents::read_seen_events(&seen_dir);
            self.agent_seen_events_dir = Some(seen_dir);
        }
        set_timeout(AGENT_POLL_SECS);
        false
    }

    fn handle_agent_timer(&mut self) -> bool {
        let mut should_render = false;
        if let Some(path) = &self.agent_snapshot_host_path {
            let session = self.mode_info.session_name.as_deref().unwrap_or_default();
            let new_pane_agents = agents::project_for_session(path, session);
            if new_pane_agents != self.pane_agents {
                self.pane_agents = new_pane_agents;
            }
        }
        if let Some(dir) = &self.agent_seen_events_dir {
            let new = agents::read_seen_events(dir);
            if new != self.seen_disk {
                self.seen_disk = new;
            }
        }
        // Recompute every tick — pane_agents, seen_disk, or active tab may
        // have shifted since last render.
        should_render |= self.recompute_tab_flags();
        set_timeout(AGENT_POLL_SECS);
        should_render
    }

    fn recompute_tab_flags(&mut self) -> bool {
        // Always re-read seen state from disk before recomputing. Without
        // this, a session you've just switched into would tint a tab
        // orange briefly using stale `seen_disk` until its next Timer
        // tick caught up — see the equivalent comment in agent-bar.
        if let Some(dir) = &self.agent_seen_events_dir {
            self.seen_disk = agents::read_seen_events(dir);
        }
        let active_tab_position = self.active_tab_idx.checked_sub(1);
        let new_tab_flags = agents::compute_tab_flags(
            &self.pane_agents,
            &self.pane_manifest,
            active_tab_position,
            &self.seen_disk,
        );
        if new_tab_flags != self.tab_flags {
            self.tab_flags = new_tab_flags;
            true
        } else {
            false
        }
    }

    fn handle_mouse_event(&mut self, mouse_event: Mouse) {
        if self.is_tooltip {
            return;
        }

        match mouse_event {
            Mouse::LeftClick(_, col) => self.handle_tab_click(col),
            Mouse::ScrollUp(_) => self.scroll_tab_up(),
            Mouse::ScrollDown(_) => self.scroll_tab_down(),
            _ => {},
        }
    }

    fn handle_clipboard_copy(&mut self, copy_destination: CopyDestination) -> bool {
        if self.is_tooltip {
            return false;
        }

        let should_render = match self.text_copy_destination {
            Some(current) => current != copy_destination,
            None => true,
        };

        self.text_copy_destination = Some(copy_destination);
        should_render
    }

    fn handle_clipboard_failure(&mut self) -> bool {
        if self.is_tooltip {
            return false;
        }

        self.display_system_clipboard_failure = true;
        true
    }

    fn handle_input_received(&mut self) -> bool {
        if self.is_tooltip {
            return false;
        }

        let should_render =
            self.text_copy_destination.is_some() || self.display_system_clipboard_failure;
        self.clear_clipboard_state();
        should_render
    }

    fn handle_tooltip_pipe(&mut self, message: PipeMessage) {
        if message.name == MSG_TOGGLE_PERSISTED_TOOLTIP {
            if self.is_first_run {
                self.persist = true;
            } else {
                #[cfg(target_family = "wasm")]
                close_self();
            }
        }
    }

    // Helper methods
    fn update_display_area(&mut self, tabs: &[TabInfo]) {
        for tab in tabs {
            if tab.active {
                self.display_area_rows = tab.display_area_rows;
                self.display_area_cols = tab.display_area_columns;
                break;
            }
        }
    }

    fn detect_tooltip_presence(&self, pane_manifest: &PaneManifest) -> bool {
        for (_tab_index, panes) in &pane_manifest.panes {
            for pane in panes {
                if pane.plugin_url == Some("zellij:compact-bar".to_owned())
                    && pane.pane_x != pane.pane_content_x
                {
                    return true;
                }
            }
        }
        false
    }

    fn find_own_tab_index(&self, pane_manifest: &PaneManifest) -> Option<usize> {
        for (tab_index, panes) in &pane_manifest.panes {
            for pane in panes {
                if pane.is_plugin && Some(pane.id) == self.own_plugin_id {
                    return Some(*tab_index);
                }
            }
        }
        None
    }

    fn handle_tab_click(&self, col: usize) {
        if let Some(tab_idx) = get_tab_to_focus(&self.tab_line, self.active_tab_idx, col) {
            switch_tab_to(tab_idx.try_into().unwrap());
        }
    }

    fn scroll_tab_up(&self) {
        let next_tab = min(self.active_tab_idx + 1, self.tabs.len());
        switch_tab_to(next_tab as u32);
    }

    fn scroll_tab_down(&self) {
        let prev_tab = max(self.active_tab_idx.saturating_sub(1), 1);
        switch_tab_to(prev_tab as u32);
    }

    fn clear_clipboard_state(&mut self) {
        self.text_copy_destination = None;
        self.display_system_clipboard_failure = false;
    }

    fn is_restricted_mode(&self, mode: InputMode) -> bool {
        matches!(
            mode,
            InputMode::Locked
                | InputMode::EnterSearch
                | InputMode::RenameTab
                | InputMode::RenamePane
                | InputMode::Prompt
                | InputMode::Tmux
                | InputMode::CopyMode
        )
    }

    // Tooltip operations
    fn toggle_persisted_tooltip(&self, new_mode: InputMode) {
        #[allow(unused_variables)]
        let message = self
            .create_tooltip_message(MSG_TOGGLE_PERSISTED_TOOLTIP, new_mode)
            .with_args(self.create_persist_args());

        #[cfg(target_family = "wasm")]
        pipe_message_to_plugin(message);
    }

    fn launch_tooltip_if_not_launched(&self, new_mode: InputMode) {
        let message = self.create_tooltip_message(MSG_LAUNCH_TOOLTIP, new_mode);
        pipe_message_to_plugin(message);
    }

    fn create_tooltip_message(&self, name: &str, mode: InputMode) -> MessageToPlugin {
        let mut tooltip_config = self.config.clone();
        tooltip_config.insert(CONFIG_IS_TOOLTIP.to_string(), "true".to_string());

        MessageToPlugin::new(name)
            .with_plugin_url("zellij:OWN_URL")
            .with_plugin_config(tooltip_config)
            .with_floating_pane_coordinates(self.calculate_tooltip_coordinates())
            .new_plugin_instance_should_have_pane_title(format!("{:?}", mode))
    }

    fn create_persist_args(&self) -> BTreeMap<String, String> {
        let mut args = BTreeMap::new();
        args.insert("persist".to_string(), String::new());
        args
    }

    fn update_tooltip_for_mode_change(&self, new_mode: InputMode) {
        if let Some(plugin_id) = self.own_plugin_id {
            let coordinates = self.calculate_tooltip_coordinates();
            change_floating_panes_coordinates(vec![(PaneId::Plugin(plugin_id), coordinates)]);
            rename_plugin_pane(plugin_id, format!("{:?}", new_mode));
        }
    }

    fn move_tooltip_to_new_tab(&self, new_tab_index: usize) {
        if let Some(plugin_id) = self.own_plugin_id {
            break_panes_to_tab_with_index(
                &[PaneId::Plugin(plugin_id)],
                new_tab_index.saturating_sub(1), // Convert to 0-based indexing
                false,
            );
        }
    }

    fn calculate_tooltip_coordinates(&self) -> FloatingPaneCoordinates {
        let tooltip_renderer = TooltipRenderer::new(&self.mode_info);
        let (tooltip_rows, tooltip_cols) =
            tooltip_renderer.calculate_dimensions(self.mode_info.mode);

        let width = tooltip_cols + 4; // 2 for borders, 2 for padding
        let height = tooltip_rows + 2; // 2 for borders
        let x_position = 2;
        let y_position = self.display_area_rows.saturating_sub(height + 2);

        FloatingPaneCoordinates::new(
            Some(x_position.to_string()),
            Some(y_position.to_string()),
            Some(width.to_string()),
            Some(height.to_string()),
            Some(true),
            Some(false),
        )
        .unwrap_or_default()
    }

    // Rendering
    fn render_tooltip(&self, rows: usize, cols: usize) {
        let tooltip_renderer = TooltipRenderer::new(&self.mode_info);
        tooltip_renderer.render(rows, cols);
    }

    fn render_tab_line(&mut self, cols: usize) {
        if let Some(copy_destination) = self.text_copy_destination {
            self.render_clipboard_hint(copy_destination);
        } else if self.display_system_clipboard_failure {
            self.render_clipboard_error();
        } else {
            self.render_tabs(cols);
        }
    }

    fn render_clipboard_hint(&self, copy_destination: CopyDestination) {
        let hint = text_copied_hint(copy_destination).part;
        self.render_background_with_text(&hint);
    }

    fn render_clipboard_error(&self) {
        let hint = system_clipboard_error().part;
        self.render_background_with_text(&hint);
    }

    fn render_background_with_text(&self, text: &str) {
        let background = self.mode_info.style.colors.text_unselected.background;
        match background {
            PaletteColor::Rgb((r, g, b)) => {
                print!("{}\u{1b}[48;2;{};{};{}m\u{1b}[0K", text, r, g, b);
            },
            PaletteColor::EightBit(color) => {
                print!("{}\u{1b}[48;5;{}m\u{1b}[0K", text, color);
            },
        }
    }

    fn render_tabs(&mut self, cols: usize) {
        if self.tabs.is_empty() {
            return;
        }

        let tab_data = self.prepare_tab_data();
        self.tab_line = tab_line(
            &self.mode_info,
            tab_data,
            cols,
            self.toggle_tooltip_key.clone(),
            self.tooltip_is_active,
        );

        let output = self
            .tab_line
            .iter()
            .fold(String::new(), |acc, part| acc + &part.part);

        self.render_background_with_text(&output);
    }

    fn prepare_tab_data(&self) -> TabRenderData {
        let mut all_tabs = Vec::new();
        let mut active_tab_index = 0;
        let mut active_swap_layout_name = None;
        let mut is_swap_layout_dirty = false;
        let mut is_alternate_tab = false;

        for tab in &self.tabs {
            let tab_name = self.get_tab_display_name(tab);

            if tab.active {
                active_tab_index = tab.position;
                if self.mode_info.mode != InputMode::RenameTab {
                    is_swap_layout_dirty = tab.is_swap_layout_dirty;
                    active_swap_layout_name = tab.active_swap_layout_name.clone();
                }
            }

            let tab_flag = self.tab_flags.get(&tab.position).copied();
            let styled_tab = tab_style(
                tab_name,
                tab,
                is_alternate_tab,
                self.mode_info.style.colors,
                self.mode_info.capabilities,
                tab_flag,
            );

            is_alternate_tab = !is_alternate_tab;
            all_tabs.push(styled_tab);
        }

        TabRenderData {
            tabs: all_tabs,
            active_tab_index,
            active_swap_layout_name,
            is_swap_layout_dirty,
        }
    }

    fn get_tab_display_name(&self, tab: &TabInfo) -> String {
        let mut tab_name = tab.name.clone();
        if tab.active && self.mode_info.mode == InputMode::RenameTab && tab_name.is_empty() {
            tab_name = "Enter name...".to_string();
        }
        tab_name
    }
}

fn bind_toggle_key_config(toggle_key: &str, client_id: u16) -> String {
    format!(
        r#"
        keybinds {{
            shared {{
                bind "{}" {{
                  MessagePlugin "compact-bar" {{
                      name "toggle_tooltip"
                      tooltip "{}"
                      payload "{}"
                  }}
                }}
            }}
        }}
    "#,
        toggle_key, toggle_key, client_id
    )
}
