//! Resolving "the first tab" for a request that has no client to answer it.
//!
//! `tabs` is keyed by tab **id**, which is a stable identifier and not a position: closing the
//! leftmost tab leaves the remaining ids as they were, so a long-lived session routinely has no
//! tab with id 0. Asking for id 0 as the fallback therefore finds nothing and the caller's work
//! is dropped, which is what made `new-pane` against a detached session do nothing.

use crate::{screen::Screen, tab::Tab};

impl Screen {
    /// The leftmost tab that actually exists, by position.
    pub fn first_tab_mut(&mut self) -> Option<&mut Tab> {
        self.get_tabs_mut()
            .values_mut()
            .min_by_key(|tab| tab.position)
    }
}
