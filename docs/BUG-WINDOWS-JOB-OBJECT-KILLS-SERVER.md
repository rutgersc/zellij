# Bug: Terminal close/crash kills zellij server via Windows Job Object

## Context

Discovered during normal usage: closing a WezTerm window or a Windows Terminal tab kills the zellij session, even when another terminal is attached to the same session.

Related upstream issues (symptom reports, root cause not identified):
- https://github.com/zellij-org/zellij/issues/4868 — "sessions: crashing in windows" (Ctrl+C closes WezTerm tab, session unrecoverable)
- https://github.com/zellij-org/zellij/issues/4915 — "infinite hang post wezterm crash in windows" (session cannot reattach after crash)
- https://github.com/zellij-org/zellij/issues/5009 — "[windows]: dead session" (server panics, dead session)
- https://github.com/zellij-org/zellij/issues/4745 — Windows implementation tracking issue

## Summary

When a terminal emulator (WezTerm, Windows Terminal) closes — whether by crash, closing a tab, or closing the window — the zellij server process is killed along with it. This happens because the server process is placed inside the terminal's **Windows Job Object**, which has `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` set. When the terminal exits, Windows forcibly terminates all processes in the job.

This defeats the core purpose of a terminal multiplexer: sessions should survive terminal disconnections. On Unix this works correctly because `daemonize` double-forks the server into its own process tree. On Windows, `CREATE_NEW_PROCESS_GROUP` only detaches from the console control group (prevents Ctrl+C propagation) but does **not** escape the Job Object.

The impact is amplified when multiple clients are attached to the same session — all of them lose their session when any one terminal closes.

## Reproduction

1. Open WezTerm (or Windows Terminal)
2. Start a zellij session: `zellij`
3. Open a second terminal and attach: `zellij attach <session-name>`
4. Close the first terminal window (or crash it via Task Manager → End Process)
5. **Expected:** Second terminal continues working, session persists
6. **Actual:** Second terminal also disconnects, session is gone

## Root cause

**File:** `zellij-client/src/lib.rs` — `spawn_server()` (Windows variant)

The server is spawned with:

```rust
const CREATE_NO_WINDOW: u32 = 0x08000000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
cmd.spawn()?;
```

These flags provide:
- `CREATE_NO_WINDOW` — server gets valid standard handles without a visible window
- `CREATE_NEW_PROCESS_GROUP` — server won't receive Ctrl+C from the console

**Neither flag escapes the parent's Job Object.** Modern terminal emulators create Job Objects with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, which means:

```
Terminal Process (WezTerm/Windows Terminal)
  └── Job Object (KILL_ON_JOB_CLOSE)
        ├── zellij client     ← killed when terminal exits
        └── zellij server     ← also killed (still in the job!)
              └── ConPTY children
```

On Unix, `daemonize` performs a double-fork that fully detaches the server from the process tree. The Windows equivalent is `CREATE_BREAKAWAY_FROM_JOB`, which removes the child process from the parent's Job Object.

## Fix

**File:** `zellij-client/src/lib.rs` — `spawn_server()`

Add `CREATE_BREAKAWAY_FROM_JOB` (0x01000000) to the creation flags. This detaches the server from the terminal's Job Object, making it an independent process that survives terminal closure:

```rust
const CREATE_NO_WINDOW: u32 = 0x08000000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;

let base_flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;

// Try with breakaway first so the server survives terminal closure.
// Fall back without it if the parent job disallows breakaway.
cmd.creation_flags(base_flags | CREATE_BREAKAWAY_FROM_JOB);
match cmd.spawn() {
    Ok(_) => Ok(()),
    Err(_) => {
        // Rebuild command without breakaway flag
        cmd.creation_flags(base_flags);
        cmd.spawn()?;
        Ok(())
    },
}
```

The fallback is needed because `CREATE_BREAKAWAY_FROM_JOB` requires the parent Job Object to have `JOB_OBJECT_LIMIT_BREAKAWAY_OK` set. Most modern terminals allow it, but in restricted environments (e.g., sandboxed or enterprise-managed terminals) the call may fail with `ERROR_ACCESS_DENIED`.

After the fix, the process tree looks like:

```
Terminal Process (WezTerm/Windows Terminal)
  └── Job Object (KILL_ON_JOB_CLOSE)
        └── zellij client     ← killed when terminal exits

zellij server                  ← independent, survives terminal death
  └── ConPTY children
```

## Impact

Without this fix:
- Closing any terminal tab/window kills the zellij session for **all** attached clients
- WezTerm crashes destroy all sessions that were started from WezTerm
- The terminal multiplexer cannot provide session persistence on Windows
- Ghost sessions are left behind (marker files + session info cache without a running server)
