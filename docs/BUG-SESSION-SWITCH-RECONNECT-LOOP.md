# Bug: Client reconnection loop creates orphan sessions on switch failure

## Context

Claude session `3cabe554` — investigated as part of Windows session switching crash analysis.

## Summary

When session switching fails (due to the zombie server bug or other issues), the client enters a reconnection loop that repeatedly tries to connect to the target session. Each attempt uses `create: true`, which can spawn a new server if the previous attempt failed. The log shows repeated `Starting Zellij client!` entries every 2-3 seconds, and `zellij ls` shows accumulating orphan sessions.

```
INFO  | lib.rs:726: Starting Zellij client!    (23:19:02)
INFO  | lib.rs:726: Starting Zellij client!    (23:19:05)
INFO  | lib.rs:726: Starting Zellij client!    (23:19:08)
INFO  | lib.rs:726: Starting Zellij client!    (23:19:16)
INFO  | lib.rs:726: Starting Zellij client!    (23:19:19)
ERROR | route.rs:80: Action SwitchSession did not complete within 1s timeout
INFO  | lib.rs:726: Starting Zellij client!    (23:19:33)
INFO  | lib.rs:655: Starting Zellij server!    (23:19:33)   <-- new server spawned
```

## Reproduction

1. Start zellij (session "A")
2. Trigger session switch to a new session "B" (e.g., via `zellij action switch-session "B"`)
3. If the switch encounters any error (self-switch, zombie server, pipe issue), observe:
   - Client keeps retrying indefinitely
   - Each retry may create a new orphan session
   - `zellij ls` shows multiple sessions accumulating
   - User is dropped to a bare terminal prompt

## Root cause

**File:** `src/commands.rs:688-737`

The reconnection loop runs inside `start_client()`:

```rust
let mut reconnect_to_session: Option<ConnectToSession> = None;
loop {
    // ... setup ...
    if let Some(reconnect_to_session) = &reconnect_to_session {
        opts.command = Some(Command::Sessions(Sessions::Attach {
            session_name: reconnect_to_session.name.clone(),
            create: true,  // <-- always tries to create if not found
            // ...
        }));
        is_a_reconnect = true;
    }
    // ... runs start_client_impl() which either:
    //   - returns Some(new_reconnect) → loop continues
    //   - returns None → loop breaks
}
```

The loop has no:
- **Retry limit** — it loops forever
- **Backoff** — retries immediately (though each attempt takes a few seconds due to server startup)
- **Deduplication** — multiple retries can each spawn a new server for the same session name

## Fix options

1. **Add a retry limit** (e.g., 3 attempts) with a clear error message to the user
2. **Add exponential backoff** to prevent rapid session creation
3. **Check if target session already exists** before using `create: true` on retries
4. **Most importantly:** fix the zombie server bug (BUG-SESSION-SWITCH-ZOMBIE-SERVER.md) — if the old server shuts down cleanly, the target session can be created normally on the first try, and the reconnection loop typically succeeds

## Relationship to other bugs

This bug is a secondary effect. The primary causes are:
- **BUG-SESSION-SWITCH-ZOMBIE-SERVER.md** — old server doesn't shut down, blocks clean reconnection
- **BUG-SESSION-SWITCH-SELF-TIMEOUT.md** — self-switch produces errors that may trigger reconnection

Fixing the zombie server bug alone would likely eliminate most occurrences of this reconnection loop issue.
