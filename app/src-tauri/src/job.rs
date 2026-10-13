//! Windows Job Objects: a run's whole process tree in one handle.
//!
//! Windows has no process groups to signal. Each run's process is put in a job that kills
//! everything left in it when the handle closes, so Stop, quitting the app, or the app
//! crashing all take grandchildren (dev servers, watchers) down too.

use std::ffi::c_void;

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

pub struct Job(HANDLE);

// SAFETY: a job handle can be used from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// A new kill-on-close job containing process `pid`.
    pub fn for_process(pid: u32) -> Result<Job> {
        // SAFETY: plain Win32 calls; every handle opened here is closed here or owned by `Job`.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                bail!("CreateJobObject failed: {}", std::io::Error::last_os_error());
            }
            let job = Job(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                bail!("SetInformationJobObject failed: {}", std::io::Error::last_os_error());
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                bail!("OpenProcess failed: {}", std::io::Error::last_os_error());
            }
            let ok = AssignProcessToJobObject(job.0, process);
            CloseHandle(process);
            if ok == 0 {
                bail!("AssignProcessToJobObject failed: {}", std::io::Error::last_os_error());
            }
            Ok(job)
        }
    }

    /// End every process in the job.
    pub fn terminate(&self) {
        // SAFETY: the handle is valid until drop.
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: closing our own handle; KILL_ON_JOB_CLOSE ends anything still running.
        unsafe {
            CloseHandle(self.0);
        }
    }
}
