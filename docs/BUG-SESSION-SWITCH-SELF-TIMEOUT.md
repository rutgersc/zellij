# Bug: Self-switch session causes 1s timeout and error cascade

## Context

Claude session `3cabe554` — investigated as part of Windows session switching crash analysis.

## Summary

When a user triggers `zellij action switch-session <name>` where `<name>` is the current session (self-switch), the action silently fails with a 1-second timeout. This is common when using a sessionizer script that lists both active sessions and directories — the user can easily select the session they're already in.

A self-switch can also happen after renaming: user renames session to "foo", sessionizer lists "foo", user selects it.

## Reproduction

1. Start zellij (session name: "foo")
2. From within a pane, run: `zellij action switch-session "foo"`

**Expected:** No-op, or a message saying already in that session.

**Actual:** 1-second hang, then:
```
ERROR  | route.rs:80: Action SwitchSession did not complete within 1s timeout
ERROR  | lib.rs:1073: Received unknown message from server
```

## Root cause

Two code paths detect the self-switch but neither signals completion properly:

### Path 1: `route.rs:1152-1179` (route_action)

```rust
Action::SwitchSession { name, .. } => {
    let current_session_name = envs::get_session_name().unwrap_or_else(|_| String::new());
    if name != current_session_name {
        // ... sends to server
    } else {
        drop(completion_tx); // no need to wait, this is a no-op
    }
}
```

`drop(completion_tx)` does NOT signal the `oneshot::Receiver`. The receiver in `wait_for_action_completion` (line 75-93) sees `Err(Canceled)` which hits the `Ok(Err(_))` branch — logged as a timeout.

### Path 2: `lib.rs:1434-1490` (ServerInstruction::SwitchSession)

```rust
if connect_to_session.name == current_session_name.ok() {
    log::error!("Cannot attach to same session");
    // completion_tx dropped implicitly at end of match arm — same timeout issue
}
```

## Fix

Signal `completion_tx` before dropping it in both paths. For the route_action path:

```rust
} else {
    let _ = completion_tx.send(ActionCompletionResult {
        exit_status: None,
        affected_pane_id: None,
        affected_tab_id: None,
        error_message: Some("Already in this session".into()),
        stdout_message: None,
    });
}
```

Same pattern for the server-side handler.
