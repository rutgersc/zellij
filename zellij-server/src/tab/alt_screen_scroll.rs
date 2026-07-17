use zellij_utils::position::Position;

use super::Tab;
use crate::ClientId;

pub enum AltScreenScroll {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
}

impl Tab {
    // Fork addition: keyboard scroll actions silently no-op in the alternate screen (the
    // scrollback is parked in AlternateScreenState), so forward them to the application
    // instead — the keyboard counterpart of the mouse-wheel faux scrolling in
    // mouse_handler.rs. Line scrolls prefer an encoded wheel event when the app tracks the
    // mouse (apps like Claude Code bind arrows to something else); page scrolls send
    // PageUp/PageDown. Returns true when the scroll was forwarded.
    pub fn forward_scroll_in_alternate_screen(
        &mut self,
        scroll: AltScreenScroll,
        client_id: ClientId,
    ) -> bool {
        let synthetic_position = Position::new(0, 0);
        let (pane_id, bytes) = match self.get_active_pane_or_floating_pane_mut(client_id) {
            Some(pane) if pane.is_alternate_mode_active() => {
                let bytes = match scroll {
                    AltScreenScroll::LineUp => pane
                        .mouse_scroll_up(&synthetic_position)
                        .map(String::into_bytes)
                        .unwrap_or_else(|| b"\x1b[A".to_vec()),
                    AltScreenScroll::LineDown => pane
                        .mouse_scroll_down(&synthetic_position)
                        .map(String::into_bytes)
                        .unwrap_or_else(|| b"\x1b[B".to_vec()),
                    AltScreenScroll::PageUp => b"\x1b[5~".to_vec(),
                    AltScreenScroll::PageDown => b"\x1b[6~".to_vec(),
                };
                (pane.pid(), bytes)
            },
            _ => return false,
        };
        if let Err(e) = self.write_to_pane_id(&None, bytes, false, pane_id, Some(client_id), None) {
            log::error!("failed to forward scroll to pane in alternate screen: {e}");
        }
        true
    }
}
