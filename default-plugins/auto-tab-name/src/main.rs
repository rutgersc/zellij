use std::collections::{BTreeMap, HashMap, HashSet};
use zellij_tile::prelude::*;

#[derive(Default)]
struct State {
    auto_state: HashMap<usize, AutoState>,
    position_to_tab_id: HashMap<usize, usize>,
    panes_by_position: HashMap<usize, Vec<PaneInfo>>,
}

#[derive(Clone)]
enum AutoState {
    Auto { last_set_name: String },
    Manual,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        subscribe(&[EventType::TabUpdate, EventType::PaneUpdate]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => self.on_tab_update(tabs),
            Event::PaneUpdate(manifest) => self.on_pane_update(manifest),
            _ => {},
        }
        false
    }

    fn render(&mut self, _rows: usize, _cols: usize) {}
}

impl State {
    fn on_tab_update(&mut self, tabs: Vec<TabInfo>) {
        self.position_to_tab_id.clear();
        for t in &tabs {
            self.position_to_tab_id.insert(t.position, t.tab_id);
        }

        for t in &tabs {
            self.reconcile_tab_state(t);
        }

        let live: HashSet<usize> = tabs.iter().map(|t| t.tab_id).collect();
        self.auto_state.retain(|id, _| live.contains(id));

        self.maybe_rename_all();
    }

    fn on_pane_update(&mut self, manifest: PaneManifest) {
        self.panes_by_position = manifest.panes;
        self.maybe_rename_all();
    }

    // Decide whether the tab is in Auto or Manual mode based on whether the
    // current visible name matches what we last set. If the user (or anything
    // else) changed it out from under us, flip to Manual. If the name is back
    // to the default "Tab #N" placeholder, treat that as the reset-to-auto
    // gesture (undo_rename_tab).
    fn reconcile_tab_state(&mut self, t: &TabInfo) {
        let is_default = is_default_tab_name(&t.name);
        let prev = self.auto_state.get(&t.tab_id).cloned();
        let next = match prev {
            Some(AutoState::Auto { last_set_name }) => {
                if t.name == last_set_name {
                    AutoState::Auto { last_set_name }
                } else if is_default {
                    AutoState::Auto { last_set_name: String::new() }
                } else {
                    AutoState::Manual
                }
            },
            Some(AutoState::Manual) => {
                if is_default {
                    AutoState::Auto { last_set_name: String::new() }
                } else {
                    AutoState::Manual
                }
            },
            None => {
                if is_default {
                    AutoState::Auto { last_set_name: String::new() }
                } else {
                    AutoState::Manual
                }
            },
        };
        self.auto_state.insert(t.tab_id, next);
    }

    fn maybe_rename_all(&mut self) {
        let pairs: Vec<(usize, usize)> = self
            .position_to_tab_id
            .iter()
            .map(|(p, id)| (*p, *id))
            .collect();
        for (position, tab_id) in pairs {
            self.maybe_rename(position, tab_id);
        }
    }

    fn maybe_rename(&mut self, position: usize, tab_id: usize) {
        let last_set = match self.auto_state.get(&tab_id) {
            Some(AutoState::Auto { last_set_name }) => last_set_name.clone(),
            _ => return,
        };
        let Some(panes) = self.panes_by_position.get(&position) else {
            return;
        };
        let Some(anchor) = anchor_pane(panes) else {
            return;
        };
        let Some(new_name) = derive_name(anchor) else {
            return;
        };
        if new_name == last_set {
            return;
        }
        rename_tab_with_id(tab_id as u64, &new_name);
        self.auto_state
            .insert(tab_id, AutoState::Auto { last_set_name: new_name });
    }
}

fn anchor_pane(panes: &[PaneInfo]) -> Option<&PaneInfo> {
    panes
        .iter()
        .filter(|p| !p.is_plugin && !p.is_suppressed && p.is_selectable)
        .min_by_key(|p| p.id)
}

fn derive_name(pane: &PaneInfo) -> Option<String> {
    let from_cmd = pane.terminal_command.as_deref().and_then(extract_name);
    if from_cmd.is_some() {
        return from_cmd;
    }
    extract_name(&pane.title)
}

fn extract_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = first_token(trimmed);
    if is_boring(candidate) {
        return None;
    }
    Some(candidate.to_string())
}

fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

fn is_boring(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if looks_like_path(s) {
        return true;
    }
    let lower = strip_exe(s).to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "pwsh"
            | "powershell"
            | "bash"
            | "zsh"
            | "fish"
            | "nu"
            | "nushell"
            | "cmd"
            | "sh"
            | "dash"
            | "ksh"
    ) {
        return true;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if s == "Pane" || s.starts_with("Pane #") || s.starts_with("Tab #") {
        return true;
    }
    false
}

fn looks_like_path(s: &str) -> bool {
    if s.starts_with('/') || s.starts_with('~') {
        return true;
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    false
}

fn strip_exe(s: &str) -> &str {
    s.strip_suffix(".exe").unwrap_or(s)
}

fn is_default_tab_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("Tab #") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boring_shells() {
        assert!(is_boring("pwsh"));
        assert!(is_boring("powershell"));
        assert!(is_boring("PowerShell.exe"));
        assert!(is_boring("bash"));
        assert!(is_boring("zsh"));
    }

    #[test]
    fn boring_paths() {
        assert!(is_boring("/usr/bin/foo"));
        assert!(is_boring("~/projects"));
        assert!(is_boring("C:\\Users"));
        assert!(is_boring("F:/GitRepos"));
    }

    #[test]
    fn interesting_apps() {
        assert!(!is_boring("nvim"));
        assert!(!is_boring("claude"));
        assert!(!is_boring("lazygit"));
        assert!(!is_boring("ssh"));
    }

    #[test]
    fn boring_placeholders() {
        assert!(is_boring("Pane"));
        assert!(is_boring("Pane #1"));
        assert!(is_boring("Tab #3"));
        assert!(is_boring("42"));
    }

    #[test]
    fn extract_first_token() {
        assert_eq!(extract_name("nvim file.rs"), Some("nvim".into()));
        assert_eq!(extract_name("  claude --task foo"), Some("claude".into()));
        assert_eq!(extract_name(""), None);
        assert_eq!(extract_name("/usr/bin/foo"), None);
    }

    #[test]
    fn default_name_recognition() {
        assert!(is_default_tab_name("Tab #1"));
        assert!(is_default_tab_name("Tab #42"));
        assert!(!is_default_tab_name("Tab #"));
        assert!(!is_default_tab_name("Tab #abc"));
        assert!(!is_default_tab_name("nvim"));
    }
}
