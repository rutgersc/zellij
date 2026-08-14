//! Repoint the server's standard handles at its own console.
//!
//! `spawn_child_process` deliberately leaves `STARTF_USESTDHANDLES` unset so
//! that `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` supplies the child's standard
//! handles. That works because console handles are relative — they mean "my
//! console" — so an inherited one re-resolves against the child's own console,
//! which is the pseudoconsole.
//!
//! The trick only holds when *our* standard handles are console handles. The
//! client spawns us with inherited stdio (`zellij-client/src/lib.rs`), so a
//! server launched from a shell with redirected stdio (git-bash, a script, CI)
//! holds pipes instead. Those are absolute: every pane's shell inherits the
//! same dead pipe, reads EOF and exits, and writes into it instead of the
//! ConPTY. `CREATE_NO_WINDOW` already gave us a console, we just are not
//! pointing at it.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AllocConsole, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

// Not re-exported by windows-sys under the enabled features; values from the
// Windows SDK. Same pattern as os_input_output_windows.rs.
const GENERIC_READ: u32 = 0x80000000;
const GENERIC_WRITE: u32 = 0x40000000;

/// Point stdin/stdout/stderr at our own console so ConPTY children inherit
/// console handles rather than whatever the launching shell handed us.
///
/// Must run before any pane is spawned. Best effort: a server that cannot get a
/// console is no worse off than before.
pub fn repoint_std_handles_at_own_console() {
    let conin = open_console_device("CONIN$", GENERIC_READ | GENERIC_WRITE);
    let conout = open_console_device("CONOUT$", GENERIC_READ | GENERIC_WRITE);

    let (conin, conout) = match (conin, conout) {
        (Some(i), Some(o)) => (i, o),
        (i, o) => {
            // No console to point at — close whatever half we got and try to
            // create one. AllocConsole fails harmlessly if we already have one.
            close(i);
            close(o);
            if unsafe { AllocConsole() } == 0 {
                log::warn!(
                    "no console for the server process; ConPTY panes will inherit the \
                     launching shell's standard handles and their shells will exit immediately"
                );
                return;
            }
            match (
                open_console_device("CONIN$", GENERIC_READ | GENERIC_WRITE),
                open_console_device("CONOUT$", GENERIC_READ | GENERIC_WRITE),
            ) {
                (Some(i), Some(o)) => (i, o),
                (i, o) => {
                    close(i);
                    close(o);
                    log::warn!("allocated a console but could not open CONIN$/CONOUT$");
                    return;
                },
            }
        },
    };

    unsafe {
        SetStdHandle(STD_INPUT_HANDLE, conin);
        SetStdHandle(STD_OUTPUT_HANDLE, conout);
        SetStdHandle(STD_ERROR_HANDLE, conout);
    }
    // The handles stay open for the life of the process on purpose — they are
    // now the process-wide standard handles.
}

fn open_console_device(name: &str, access: u32) -> Option<HANDLE> {
    let wide: Vec<u16> = OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        None
    } else {
        Some(handle)
    }
}

fn close(handle: Option<HANDLE>) {
    if let Some(handle) = handle {
        unsafe { CloseHandle(handle) };
    }
}
