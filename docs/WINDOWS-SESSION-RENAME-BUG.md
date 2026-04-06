# Bug: Renamed sessions cannot be reattached on Windows

## Summary

After renaming a zellij session on Windows, switching away and back to it causes the
client to hang with a blank screen. The session is still running but unreachable by its
new name.

Related upstream issues:
- https://github.com/zellij-org/zellij/issues/3009
- https://github.com/zellij-org/zellij/issues/4029

## Reproduction

1. Start zellij, open the session manager (`Ctrl+O`, `w`)
2. Rename the current session (`Ctrl+R`, type new name, `Enter`)
3. Create or switch to a different session
4. Switch back to the renamed session
5. **Result:** blank screen, client hangs indefinitely

## Root cause

On Windows, zellij uses **named pipes** (kernel objects in `\\.\pipe\` namespace) for
IPC instead of Unix domain sockets. Named pipes are created once at server startup and
**cannot be renamed** — their identity is fixed in the kernel namespace.

The rename code only renames the **marker file** on disk. The kernel pipe still listens
under the old name, so the client constructs a pipe name from the new session name and
connects to a pipe that doesn't exist.

## Step-by-step code walkthrough

### Step 1: Server creates pipe at startup

**`zellij-server/src/lib.rs:714`** — the `server_listener` thread calls `ipc_bind`:

```rust
let listener = ipc_bind(&socket_path).unwrap();
```

**`zellij-utils/src/consts.rs:206-213`** — on Windows, `ipc_bind` creates a
`GenericNamespaced` named pipe and writes the PID to a marker file:

```rust
#[cfg(windows)]
pub fn ipc_bind(path: &std::path::Path) -> std::io::Result<Listener> {
    let name = path.to_string_lossy().to_string();          // e.g. "C:\\...\\old_name"
    let ns_name = name.to_ns_name::<GenericNamespaced>()?;  // → \\.\pipe\old_name
    let listener = ListenerOptions::new().name(ns_name).create_sync()?;
    std::fs::write(path, std::process::id().to_string())?;  // marker file: just PID
    Ok(listener)
}
```

A reply pipe is also created at `lib.rs:725` via `ipc_bind_reply` (`consts.rs:258-264`),
which binds `{path}-reply` as a second named pipe for server→client messages.

The listener then enters a blocking accept loop at `lib.rs:727`:

```rust
for stream in listener.incoming() { ... }
```

This loop runs for the **lifetime of the server** — the listener is never replaced.

### Step 2: User renames the session

The session manager plugin calls `rename_session()` at
**`default-plugins/session-manager/src/main.rs:969`**, which sends a
`RenameSession` action to the server.

### Step 3: Server renames the marker file (but not the pipe)

**`zellij-server/src/screen.rs:7931-7936`** — the `ScreenInstruction::RenameSession`
handler renames the marker file on disk:

```rust
let old_socket_file_path = ZELLIJ_SOCK_DIR.join(&old_session_name);
let new_socket_file_path = ZELLIJ_SOCK_DIR.join(&name);
if let Err(e) = std::fs::rename(old_socket_file_path, new_socket_file_path) {
    log::error!("Failed to rename ipc socket: {:?}", e);
}
```

On Unix this works because the socket **is** the file — `std::fs::rename` moves the
actual socket inode. On Windows, the marker file is just a plain text file containing
the PID. The kernel named pipe (`\\.\pipe\old_name`) is unaffected by `std::fs::rename`.

The session info folder is also renamed at `screen.rs:7941-7948`, and clients are
notified via `ServerToClientMsg::RenamedSession` at `screen.rs:7960-7966`.

### Step 4: Client tries to reconnect using the new name

When the user switches back to the renamed session, the client constructs the IPC path
from the session name at **`zellij-client/src/lib.rs:772-779`**:

```rust
let create_ipc_pipe = || -> std::path::PathBuf {
    let mut sock_dir = ZELLIJ_SOCK_DIR.clone();
    sock_dir.push(envs::get_session_name().unwrap());  // "new_name"
    sock_dir
};
```

### Step 5: Connection fails → infinite retry → hang

**`zellij-client/src/os_input_output.rs:278-290`** — `connect_to_server` retries in
an infinite loop:

```rust
fn connect_to_server(&self, path: &Path) {
    let socket;
    loop {
        match zellij_utils::consts::ipc_connect(path) {  // tries pipe "new_name"
            Ok(sock) => { socket = sock; break; },
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            },
        }
    }
    // ...
}
```

**`zellij-utils/src/consts.rs:186-191`** — `ipc_connect` derives the pipe name
directly from the path:

```rust
#[cfg(windows)]
pub fn ipc_connect(path: &std::path::Path) -> std::io::Result<Stream> {
    let name = path.to_string_lossy().to_string();          // "C:\\...\\new_name"
    let ns_name = name.to_ns_name::<GenericNamespaced>()?;  // → \\.\pipe\new_name
    LocalSocketStream::connect(ns_name)                     // FAILS: pipe doesn't exist
}
```

The pipe `\\.\pipe\new_name` was never created — the server is still listening on
`\\.\pipe\old_name`. The connection fails, the client retries every 50ms forever,
and the user sees a blank/hung terminal.

## Proposed fix: store original pipe name in marker file

The least invasive fix — no changes to the listener thread architecture.

**Idea:** The marker file already stores the PID. Also store the original pipe path.
After a rename, the marker file moves to `new_name` but its **content** still points
to the original pipe `old_name`. Clients read the marker file to find the real pipe.

### Changes required

| File | Function | Change |
|------|----------|--------|
| `zellij-utils/src/consts.rs:211` | `ipc_bind` (Windows) | Write `PID\npipe_path` instead of just `PID` |
| `zellij-utils/src/consts.rs:237` | `ipc_bind_async` (Windows) | Same |
| `zellij-utils/src/consts.rs:186-191` | `ipc_connect` (Windows) | Read marker file, use stored pipe path |
| `zellij-utils/src/consts.rs:245-252` | `ipc_connect_reply` (Windows) | Read marker file, derive reply pipe from stored path |
| `zellij-utils/src/sessions.rs:183` | `assert_socket` (Windows) | Parse only first line as PID |

### Why this works

- `ipc_bind` writes `12345\nC:\...\old_name` to the marker file at `C:\...\old_name`
- `std::fs::rename` moves it to `C:\...\new_name` — file content unchanged
- `ipc_connect` reads `C:\...\new_name`, extracts `C:\...\old_name` from line 2
- Connects to `\\.\pipe\old_name` — the pipe that actually exists

### Why not rebind the listener?

The listener runs in a blocking `for stream in listener.incoming()` loop
(`lib.rs:727`). Replacing it would require either:
- Making the loop interruptible (non-blocking + polling + signal channel)
- Switching to async accept (major refactor)
- Sending a dummy connection to unblock the iterator (fragile)

The marker file approach avoids all of this and is backward-compatible: if the marker
file has no second line (old format), `ipc_connect` falls back to the path-derived name.
