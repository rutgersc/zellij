//! Submitting text to a pane as one operation.
//!
//! `paste` followed by a separate `write … 13` is two server round trips, and the pane can be
//! redrawn or written to in between - which is enough to misplace the ENTER. Both writes happen
//! here inside a single screen-thread instruction instead.

use zellij_utils::errors::prelude::*;

use crate::{panes::PaneId, route::NotificationEnd, tab::Tab, ClientId};

const ENTER: u8 = 13;

impl Tab {
    pub fn prompt_to_pane_id(
        &mut self,
        bytes: Vec<u8>,
        pane_id: PaneId,
        completion: Option<NotificationEnd>,
    ) -> Result<()> {
        self.paste_to_pane_id(bytes, pane_id, None)?;
        self.write_to_pane_id(&None, vec![ENTER], false, pane_id, None, completion)?;
        Ok(())
    }

    pub fn prompt_to_active_terminal(
        &mut self,
        bytes: Vec<u8>,
        client_id: ClientId,
        completion: Option<NotificationEnd>,
    ) -> Result<()> {
        let active_pane_id = self
            .get_active_pane_id(client_id)
            .ok_or_else(|| anyhow!("no active pane for client {client_id}"))
            .with_context(|| format!("failed to prompt active terminal for client {client_id}"))?;
        self.prompt_to_pane_id(bytes, active_pane_id, completion)
    }
}
