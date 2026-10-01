use std::io::BufRead;

use super::IpcReceiverWithContext;

impl<T> IpcReceiverWithContext<T> {
    /// A failed read is either a message this side can't decode or the peer
    /// hanging up. Only the first is worth counting; a hang-up (the Windows
    /// liveness probe connects and drops on every registry refresh) otherwise
    /// spins the router through 1000 "unknown messages" before it lets go.
    pub fn peer_closed(&mut self) -> bool {
        self.receiver.fill_buf().map_or(true, |buf| buf.is_empty())
    }
}
