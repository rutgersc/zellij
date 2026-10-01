//! Each pane's process runs inside its own Job Object, so closing the pane
//! takes down the whole tree. `TerminateProcess` on the direct child alone
//! leaks everything below it: Git Bash's `bash.exe` is a launcher, and the
//! real shell plus whatever it started (agents, servers) outlive the pane.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::ResumeThread;

fn jobs() -> &'static Mutex<HashMap<u32, usize>> {
    static JOBS: OnceLock<Mutex<HashMap<u32, usize>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn create_kill_on_close_job() -> std::io::Result<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            CloseHandle(job);
            return Err(err);
        }
        Ok(job)
    }
}

/// Put a process spawned with `CREATE_SUSPENDED` into a fresh job, then let
/// it run. Assigning before the first instruction means no grandchild can be
/// born outside the job.
pub(super) fn contain_and_resume(process: HANDLE, thread: HANDLE, pid: u32) {
    let assigned = create_kill_on_close_job().and_then(|job| unsafe {
        if AssignProcessToJobObject(job, process) == 0 {
            let err = std::io::Error::last_os_error();
            CloseHandle(job);
            Err(err)
        } else {
            Ok(job)
        }
    });
    match assigned {
        Ok(job) => {
            jobs().lock().unwrap().insert(pid, job as usize);
        },
        Err(e) => log::warn!(
            "pid {pid} runs without a job object, closing its pane will leak its descendants: {e}"
        ),
    }
    unsafe { ResumeThread(thread) };
}

/// Kill the whole tree of a pane's process. `false` when the pid has no job.
pub(super) fn terminate_tree(pid: u32) -> bool {
    match jobs().lock().unwrap().remove(&pid) {
        Some(job) => {
            unsafe {
                TerminateJobObject(job as HANDLE, 1);
                CloseHandle(job as HANDLE);
            }
            true
        },
        None => false,
    }
}

/// The pane's direct child exited on its own. Closing the job kills whatever
/// it left behind, the same end state as closing the pane.
pub(super) fn release(pid: u32) {
    if let Some(job) = jobs().lock().unwrap().remove(&pid) {
        unsafe { CloseHandle(job as HANDLE) };
    }
}
