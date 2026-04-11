# Bug: Session switch leaves zombie server with no clients

## Context

Original claude session where this bug came to light: `C:\Projects\foam\Claude-Sessions\2026-04-06-3cabe554-fixing-zellij-windows-keyboard-input.md` — investigated as part of Windows session switching crash analysis.

## Summary

When a client switches sessions, the server removes the client but does not check if it was the last one. Unlike the `ClientExit` handler which cleanly shuts down a clientless server, the `SwitchSession` handler leaves the old server running in a degraded state. The server's ConPTY terminals are eventually cleaned up, but the server keeps running and producing errors:

```
ERROR  | os_input_output_windows.rs:567: a non-fatal error occured
Caused by:
    0: failed to set terminal 0 to size (172, 50)
    1: no ConPTY terminal found for id 0
```

The user sees a blank screen when they try to re-attach to this zombie session.

## Reproduction

1. Start zellij with a single client (session "A")
2. From a pane, run: `zellij action switch-session "B"` (new session)
3. Session B starts, user is now in B
4. Run: `zellij ls` — session A still shows as active
5. Run: `zellij attach A` — blank screen, no response, no timeout

## Root cause

**File:** `zellij-server/src/lib.rs:1434-1490`

The `ServerInstruction::SwitchSession` handler:

```rust
send_to_client!(client_id, os_input, ServerToClientMsg::SwitchSession { .. });
remove_client!(client_id, os_input, session_state);
drop(completion_tx);
// ... sends RemoveClient to screen and plugin threads
// BUT: no check for active_clients_are_connected()
// BUT: no break / server shutdown
```

Compare with the `ClientExit` handler at `lib.rs:1116-1196`:

```rust
remove_client!(client_id, os_input, session_state);
// ...
if !session_state.read().unwrap().active_clients_are_connected() {
    *session_data.write().unwrap() = None;
    // ... cleanup watchers ...
    break; // <-- shuts down the server
}
```

The `SwitchSession` handler is missing the `active_clients_are_connected()` check and the `break` that would shut down the server when the last client leaves.

## Fix

Add the same last-client check after `remove_client!` in the SwitchSession handler. When the last client switches away, the server should save session state and shut down cleanly, just like `ClientExit` does. The session can then be resurrected when the user switches back to it.

The relevant code to add after line 1488 (before the closing brace):

```rust
if !session_state.read().unwrap().active_clients_are_connected() {
    *session_data.write().unwrap() = None;
    // clean up remaining pipe clients
    let client_ids_to_cleanup: Vec<ClientId> = session_state
        .read().unwrap().clients.keys().copied().collect();
    for client_id in client_ids_to_cleanup {
        remove_client!(client_id, os_input, session_state);
    }
    let watcher_client_ids: Vec<ClientId> =
        session_state.read().unwrap().watcher_client_ids();
    for watcher_id in watcher_client_ids {
        let _ = os_input.send_to_client(
            watcher_id,
            ServerToClientMsg::Exit { exit_reason: ExitReason::Normal },
        );
    }
    break;
}
```

## Impact

Without this fix, every session switch on Windows (and likely Unix too, though it may be masked by different IPC behavior) leaves zombie server processes that:
- Consume resources
- Produce error log spam
- Show as "active" in `zellij ls` but are unresponsive
- Cannot be cleaned up except by force-killing (see `zellij-nuke` script)
