//! Native ACP process lifetime. Mirrors Instance's kernel job ownership;
//! remote services remain owned by their remote installation.
use anyhow::{Context, Result, ensure};
use std::{mem, ptr};
use tokio::process::{Child, Command};
use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};

/// Closing the last owned handle terminates the job's local descendants.
pub(crate) struct ProcessJob(usize);
impl ProcessJob {
    /// Establish ownership before the installer or ACP process can execute.
    pub(crate) async fn spawn(command: &mut Command) -> Result<(Child, Self)> {
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let result = Self::attach(&child).and_then(|job| {
            resume_suspended(&child)?;
            Ok(job)
        });
        match result {
            Ok(job) => Ok((child, job)),
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(error)
            }
        }
    }

    fn attach(child: &Child) -> Result<Self> {
        let process = child
            .raw_handle()
            .context("ACP process exited during launch")?;
        // SAFETY: valid OS process handle and initialized API structures. The
        // unnamed job handle is non-inheritable and is owned by this guard.
        unsafe {
            let job = CreateJobObjectW(ptr::null(), ptr::null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error())
                    .context("Cannot create ACP process job");
            }
            let guard = Self(job as usize);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("Cannot configure ACP process cleanup");
            }
            if AssignProcessToJobObject(job, process as _) == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Cannot attach ACP process to its job");
            }
            Ok(guard)
        }
    }
}

// Tokio retains the process handle, not the initial thread handle. The child
// is still suspended, so it cannot create another thread before attachment.
fn resume_suspended(child: &Child) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::{Diagnostics::ToolHelp::*, Threading::*},
    };
    let pid = child.id().context("Local process exited during launch")?;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    ensure!(
        snapshot != INVALID_HANDLE_VALUE,
        "Cannot inspect the suspended process"
    );
    let mut entry: THREADENTRY32 = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of::<THREADENTRY32>() as u32;
    let mut found = None;
    let mut available = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while available {
        if entry.th32OwnerProcessID == pid {
            found = Some(entry.th32ThreadID);
            break;
        }
        available = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    let id = found.context("Suspended process has no initial thread")?;
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, id) };
    ensure!(
        !thread.is_null(),
        "Cannot open the suspended process thread"
    );
    let resumed = unsafe { ResumeThread(thread) };
    unsafe {
        CloseHandle(thread);
    }
    ensure!(resumed == 1, "Cannot resume the suspended process");
    Ok(())
}
impl Drop for ProcessJob {
    fn drop(&mut self) {
        // SAFETY: this guard owns exactly one non-inheritable job handle.
        unsafe {
            CloseHandle(self.0 as _);
        }
    }
}
