//! Fork-only: pipe security descriptor that grants the current user SID
//! Full Access regardless of how the creating process was elevated.
//!
//! Why this exists: on Windows, when you bind a named pipe with no explicit
//! security attributes, the pipe inherits the *default DACL* of the creating
//! token. For a normal interactive (UAC-filtered) token, that DACL contains
//! an ACE for the user SID, so any other process of the same user can open
//! the pipe. But OpenSSH on Windows hands SSH'd-in admins an *unfiltered*
//! High-integrity primary token, whose default DACL grants Administrators
//! and SYSTEM but omits the user SID. The pipe inherits that — and a normal
//! desktop pwsh (filtered token, no Administrators) gets ACCESS_DENIED when
//! it tries to attach, even though it's the same user.
//!
//! By stamping an explicit DACL that names the user SID, the pipe is
//! reachable from any of the user's contexts (filtered or unfiltered).

use std::{io, ptr, slice};

use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use widestring::U16CString;
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL},
    Security::{
        Authorization::ConvertSidToStringSidW, GetTokenInformation, TokenUser, PSID, TOKEN_QUERY,
        TOKEN_USER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub fn user_pipe_security_descriptor() -> io::Result<SecurityDescriptor> {
    let sid = current_user_sid_string()?;
    // FA = FILE_ALL_ACCESS, SY = LocalSystem, BA = Built-in Administrators.
    // The third ACE is the load-bearing one: it's missing from the default
    // DACL of an SSH-spawned admin token.
    let sddl = format!("D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{})", sid);
    let wide = U16CString::from_str(&sddl)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    SecurityDescriptor::deserialize(&wide)
}

fn current_user_sid_string() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _token_guard = HandleGuard(token);

    // Probe for required size. The first call fails with ERROR_INSUFFICIENT_BUFFER
    // (122) but writes the needed length into `needed`.
    let mut needed: u32 = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let token_user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_to_string(token_user.User.Sid)
}

fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut wide_ptr: *mut u16 = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut wide_ptr) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _free_guard = LocalFreeGuard(wide_ptr.cast());

    let mut len = 0;
    while unsafe { *wide_ptr.add(len) } != 0 {
        len += 1;
    }
    let units = unsafe { slice::from_raw_parts(wide_ptr.cast_const(), len) };
    Ok(String::from_utf16_lossy(units))
}

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalFreeGuard(*mut core::ffi::c_void);
impl Drop for LocalFreeGuard {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0 as HLOCAL);
        }
    }
}
