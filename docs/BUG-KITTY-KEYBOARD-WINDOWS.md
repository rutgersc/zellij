# Bug: Kitty keyboard protocol sequences not parsed on Windows

## Summary

Zellij on Windows does not consume kitty keyboard protocol (CSI u) escape sequences from the host terminal. Instead of being parsed as key events, the sequences leak through to shell panes as literal text. This makes `Ctrl+Shift+<key>` and other modifier-rich keybindings impossible on Windows.

## Reproduction

1. Run zellij inside **Windows Terminal 1.25+** (which supports kitty keyboard protocol)
2. Configure a keybinding using `Ctrl Shift` modifier, e.g.:
   ```kdl
   bind "Ctrl Shift f" { ToggleFloatingPanes; }
   ```
3. Press `Ctrl+Shift+F` inside a zellij pane

**Expected:** Zellij intercepts the key and toggles floating panes.

**Actual:** The raw escape sequence `[102;6u` appears as text in the shell. Zellij does not consume it.

Other examples observed:
- `Ctrl+F` → `[102;5u` leaks
- `Ctrl+N` → `[110;5u` leaks
- `Ctrl+O` → `[111;5u` leaks
- `Ctrl+Shift+L` → `[108;6u` leaks

These are valid CSI u sequences: `ESC [ <keycode> ; <modifiers> u` where modifier 5 = Ctrl, 6 = Ctrl+Shift.

## Diagnosis

### The keyboard input pipeline on Windows

```
Terminal Emulator (WezTerm/Windows Terminal)
    ↓ (creates ConPTY for zellij)
ConPTY (pseudoterminal layer)
    ↓ (VT byte stream on stdin)
Zellij reads stdin via tokio::io::stdin()
    → zellij-client/src/os_input_output.rs:57-61 (AsyncStdinReader)
    ↓ (should parse key events here)
Zellij routes to keybind handler or forwards to pane
```

### Terminal behavior verified

**Windows Terminal 1.25+:**
- Sends kitty CSI u sequences through ConPTY ✓
- `Ctrl+Shift+F` → `ESC[102;6u` arrives on the VT stream
- PowerShell's `[Console]::ReadKey()` (Win32 console API) can't read these — they leak as text
- But VT-aware apps reading stdin SHOULD see them

**WezTerm (with `enable_kitty_keyboard = true`):**
- Does NOT send kitty sequences through ConPTY ✗
- Sends correct Win32 console input events (PowerShell's `[Console]::ReadKey()` sees `Modifiers: Shift, Control`)
- But zellij reads the VT stream, not Win32 events, so it only sees `^F` (Shift lost)

**Alacritty:**
- Same behavior as WezTerm — Shift modifier lost in VT stream

### Where the bug is

Zellij reads stdin as raw bytes via `tokio::io::stdin()`:

**File:** `zellij-client/src/os_input_output.rs:41-61`
```rust
pub struct AsyncStdinReader {
    stdin: tokio::io::Stdin,
    buffer: Vec<u8>,
}

impl AsyncStdin for AsyncStdinReader {
    async fn read(&mut self) -> io::Result<Vec<u8>> {
        use tokio::io::AsyncReadExt;
        let n = self.stdin.read(&mut self.buffer).await?;
        Ok(self.buffer[..n].to_vec())
    }
}
```

The bytes are then parsed by termwiz. The issue is that when Windows Terminal sends kitty CSI u sequences, zellij's input parser either:
1. Does not recognize them as key events, or
2. Fails to match them against keybindings, or
3. Forwards them to the active pane before checking keybindings

Related: PR #4150 ("reencode termwiz keycodes as current_buffer isn't reliable") addresses a similar class of bugs where the input buffer can contain multiple instructions that get misparsed.

### Input parsing entry point

The raw bytes flow through:

1. `zellij-client/src/lib.rs:453` — `async_stdin.read()` receives bytes
2. `zellij-client/src/input_handler.rs:164-166` — `InputInstruction::KeyEvent` dispatched
3. Keybind matching happens here — if it fails, the raw bytes are forwarded to the pane

### Key files to investigate

| File | Purpose |
|------|---------|
| `zellij-client/src/os_input_output.rs:41-61` | Stdin reader (raw bytes from ConPTY) |
| `zellij-client/src/lib.rs:425-453` | Async stdin read loop |
| `zellij-client/src/input_handler.rs:164-168` | Key event dispatch and keybind matching |
| `zellij-server/src/route.rs:249-258` | Server-side key routing with kitty protocol flag |
| `zellij-server/src/panes/grid.rs:617-618` | Per-pane kitty keyboard protocol state |

### What the fix likely needs

The input parser (termwiz-based) needs to correctly parse CSI u sequences (`ESC [ <keycode> ; <modifiers> u`) on Windows and convert them to proper `KeyEvent` structs with modifier flags before keybind matching. Currently these sequences appear to pass through unparsed and get forwarded as raw bytes to the pane.

## Environment

- Windows 11 Pro 10.0.22631
- Windows Terminal Preview 1.25.622.0
- WezTerm (latest, with `enable_kitty_keyboard = true`)
- Zellij built from source (branch: fix-pane-renaming-windows)
- `support_kitty_keyboard_protocol` set to default (true)

## Related issues

- [PR #4150 — reencode termwiz keycodes as current_buffer isn't reliable](https://github.com/zellij-org/zellij/pull/4150)
- [Issue #4745 — Windows implementation issues](https://github.com/zellij-org/zellij/issues/4745)
- [Issue #4178 — Escape key prints 27;129u in Neovim/Helix](https://github.com/zellij-org/zellij/issues/4178)
