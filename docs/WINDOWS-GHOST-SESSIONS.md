# Windows: Ghost Sessions (stuck/unkillable sessions)

On Windows, zellij sessions can become "ghosts" — they appear in `zellij ls` but their server process is gone. Attempting to kill, delete, or attach to them fails with:

```
Error occurred: Os { code: 2, kind: NotFound, message: "The system cannot find the file specified." }
```

This happens when the terminal crashes, is force-closed, or the zellij server exits uncleanly — the process dies but the session metadata and named pipe markers are left behind.

## Symptoms

- `zellij ls` shows the session (possibly as `EXITED - attach to resurrect`)
- `zellij kill-session <name>` fails with NotFound
- `zellij delete-session <name> --force` reports success but the session reappears
- `zellij attach <name>` hangs indefinitely
- `zellij kill-all-sessions` fails if any ghost session exists, blocking cleanup of other sessions

## Root cause

Session state is stored in two locations. Both must be removed to fully clean up a ghost session:

1. **Named pipe marker** — `%TEMP%\zellij\contract_version_1\<session_name>`
2. **Session info (layout + metadata)** — `%LOCALAPPDATA%\Zellij\cache\contract_version_1\session_info\<session_name>\`

The `kill-session` command tries to signal the server process (which no longer exists), causing the NotFound error. The `delete-session --force` removes the pipe marker but not the session info, and vice versa. As long as one location has state, the session can reappear.

## Manual cleanup

Remove both locations to fully purge a ghost session:

### PowerShell

```powershell
$session = "my-session-name"
Remove-Item -Recurse -Force "$env:TEMP\zellij\contract_version_1\$session" -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Zellij\cache\contract_version_1\session_info\$session" -ErrorAction SilentlyContinue
```

### Git Bash

```bash
session="my-session-name"
rm -rf "/tmp/zellij/contract_version_1/$session"
rm -rf "$LOCALAPPDATA/Zellij/cache/contract_version_1/session_info/$session"
```

To nuke all ghost sessions, remove the entire directories:

```powershell
Remove-Item -Recurse -Force "$env:TEMP\zellij\contract_version_1\*"
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Zellij\cache\contract_version_1\session_info\*"
```

## Related issues

- [#4745 — Windows implementation issues](https://github.com/zellij-org/zellij/issues/4745)
- [#4915 — Infinite hang post terminal crash on Windows](https://github.com/zellij-org/zellij/issues/4915)
- [#4413 — Session resurrection completely non-functional](https://github.com/zellij-org/zellij/issues/4413)
