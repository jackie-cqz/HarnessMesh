//! Only operates on a process started by this runtime. Never enumerates or kills
//! unrelated harness processes; process isolation is not a filesystem sandbox.
use anyhow::{Context, Result, bail};
use tokio::process::{Child, Command};

#[cfg(windows)]
mod platform {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::*,
    };
    pub struct ProcessTree {
        job: HANDLE,
    }
    // SAFETY: the owned kernel handle has no thread affinity and is closed once.
    unsafe impl Send for ProcessTree {}
    impl ProcessTree {
        pub fn configure(_command: &mut Command) {}
        pub fn attach(child: &Child) -> Result<Self> {
            // SAFETY: all pointers are valid for the duration of each call.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    bail!("cannot create per-run Windows Job Object");
                }
                let tree = Self { job };
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                ) == 0
                {
                    bail!("cannot configure per-run Job Object");
                }
                let process = child.raw_handle().context("child process has no handle")?;
                if AssignProcessToJobObject(job, process as HANDLE) == 0 {
                    bail!("cannot attach harness to tracked Job Object");
                }
                Ok(tree)
            }
        }
        pub fn terminate(&self) -> Result<()> {
            // SAFETY: this handle belongs only to this managed run's Job Object.
            if unsafe { TerminateJobObject(self.job, 1) } == 0 {
                bail!("cannot terminate managed process tree");
            }
            Ok(())
        }
        pub fn stopped(&self) -> Result<bool> {
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION =
                unsafe { std::mem::zeroed() };
            // SAFETY: the output buffer matches the requested information class.
            if unsafe {
                QueryInformationJobObject(
                    self.job,
                    JobObjectBasicAccountingInformation,
                    &mut accounting as *mut _ as *mut _,
                    std::mem::size_of_val(&accounting) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                bail!("cannot inspect managed process tree");
            }
            Ok(accounting.ActiveProcesses == 0)
        }
    }
    impl Drop for ProcessTree {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.job);
            }
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    pub struct ProcessTree {
        group: i32,
    }
    impl ProcessTree {
        pub fn configure(command: &mut Command) {
            command.process_group(0);
        }
        pub fn attach(child: &Child) -> Result<Self> {
            let group = i32::try_from(child.id().context("child has no pid")?)?;
            if group <= 1 {
                bail!("invalid managed process group");
            }
            Ok(Self { group })
        }
        pub fn terminate(&self) -> Result<()> {
            // SAFETY: negative PID targets the new group owned by this child.
            let result = unsafe { libc::kill(-self.group, libc::SIGKILL) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
        pub fn stopped(&self) -> Result<bool> {
            // SAFETY: signal zero checks only the owned group's existence.
            if unsafe { libc::kill(-self.group, 0) } == 0 {
                return Ok(false);
            }
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                Ok(true)
            } else {
                Err(std::io::Error::last_os_error().into())
            }
        }
    }
    impl Drop for ProcessTree {
        fn drop(&mut self) {
            let _ = self.terminate();
        }
    }
}
pub use platform::ProcessTree;
