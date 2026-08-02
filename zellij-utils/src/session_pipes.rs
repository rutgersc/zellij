//! Enumerate the Windows named-pipe namespace to find which session servers are
//! actually bound right now.
//!
//! A pipe is refcounted by the kernel and vanishes when its last handle closes,
//! so its presence is proof a server is serving *that* session id. None of the
//! other signals lying around are:
//!
//! - the registry's `state "running"` is a word written at startup, left behind
//!   when a server dies without updating it;
//! - the socket-dir marker file is a plain file `ipc_bind` writes and nothing
//!   ever deletes;
//! - the PID inside that marker is a recycled integer — an observed registry
//!   held two ids claiming one live PID, only one of which had a pipe.
//!
//! `std::fs::read_dir(r"\\.\pipe\")` fails with `os error 3` (std stats the path
//! first and the NPFS root isn't a directory in the sense it expects), which is
//! why this drops to `FindFirstFileW` directly. The namespace itself lists fine.

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW,
};

use crate::consts::ZELLIJ_SOCK_DIR;

/// Session ids (the UUID filenames) whose server pipe is currently bound.
///
/// `None` means the namespace could not be read at all — callers must treat that
/// as "unknown", never as "nothing is alive", or a transient failure would
/// blank every session at once.
pub fn live_session_ids() -> Option<HashSet<String>> {
    // `ipc_bind` names each pipe with the full marker path, so the namespace
    // entry reads `<sock_dir>\<uuid>`. Match that prefix and keep the tail.
    let prefix = {
        let mut p = ZELLIJ_SOCK_DIR.to_string_lossy().to_string();
        if !p.ends_with('\\') {
            p.push('\\');
        }
        p
    };
    Some(
        enumerate_pipe_namespace()?
            .into_iter()
            // The server binds `<path>` and `<path>-reply`; only the former is
            // the session's identity.
            .filter(|n| !n.ends_with("-reply"))
            .filter_map(|n| n.strip_prefix(&prefix).map(str::to_string))
            .filter(|tail| !tail.contains('\\'))
            .collect(),
    )
}

fn enumerate_pipe_namespace() -> Option<Vec<String>> {
    // Built from char codes rather than a literal so no escaping layer between
    // here and the OS can eat a backslash.
    let sep = char::from_u32(92)?;
    let pattern = format!("{sep}{sep}.{sep}pipe{sep}*");
    let wide: Vec<u16> = std::ffi::OsStr::new(&pattern)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let handle = unsafe { FindFirstFileW(wide.as_ptr(), &mut data) };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut names = Vec::new();
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        names.push(
            OsString::from_wide(&data.cFileName[..len])
                .to_string_lossy()
                .to_string(),
        );
        if unsafe { FindNextFileW(handle, &mut data) } == 0 {
            break;
        }
    }
    unsafe { FindClose(handle) };
    Some(names)
}
