//! A Windows Job Object that takes every supervised service down with the
//! orchestrator.
//!
//! On Unix each child leads its own process group, so `terminate_child` can
//! signal `cargo run` and the service it spawned together. Windows has no
//! process groups; shutdown relies on `taskkill /T /F`, which only runs if the
//! orchestrator reaches `shutdown_children`. When the orchestrator panics, is
//! killed from Task Manager, or loses its console, nothing runs — and the real
//! service, a grandchild behind `cargo run`, survives with its port bound, so
//! the next `krab bootstrap` fails to start it.
//!
//! A job created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` closes that gap in
//! the kernel: every process assigned to the job, and every process those
//! processes start afterwards, is terminated when the last handle to the job
//! closes. The orchestrator holds the only handle for its whole lifetime, so
//! the OS closes it — and kills the tree — however the orchestrator exits.
//!
//! One gap remains and is accepted: a child is assigned just after it is
//! spawned, so a grandchild it started in that window would not be in the job.
//! `cargo run` spends far longer than that resolving and building before it
//! starts the service, which is the grandchild this exists for.

use std::io;
use std::os::windows::io::RawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// An owned job handle that kills its processes when dropped.
#[derive(Debug)]
pub(crate) struct KillOnCloseJob {
    handle: HANDLE,
}

// SAFETY: a job handle is a kernel object reference with no thread affinity;
// the Win32 job APIs may be called on it from any thread.
unsafe impl Send for KillOnCloseJob {}
// SAFETY: as above; the only operation shared across threads is
// `AssignProcessToJobObject`, which the kernel serialises.
unsafe impl Sync for KillOnCloseJob {}

impl KillOnCloseJob {
    /// Create an anonymous job with kill-on-close set.
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: null attributes and a null name create an anonymous job
        // with default security; the result is checked before use.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self { handle };

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a correctly sized, initialised struct of the type
        // the information class names, and it outlives the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            // `job` is dropped here, closing the handle.
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Put the process behind `process` into the job.
    ///
    /// `process` must be a live process handle with `PROCESS_SET_QUOTA` and
    /// `PROCESS_TERMINATE` access, which the handle `CreateProcess` returns to
    /// the parent always has.
    pub(crate) fn assign(&self, process: RawHandle) -> io::Result<()> {
        // SAFETY: both handles are valid for the duration of the call; the
        // job handle is owned by `self` and the caller owns `process`.
        let ok = unsafe { AssignProcessToJobObject(self.handle, process as HANDLE) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by `CreateJobObjectW`, is owned by
        // this value, and is closed exactly once. Closing it is what kills
        // the job's processes.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::KillOnCloseJob;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    /// `(pid, exe name)` of every live child of `parent`.
    fn children_of(parent: u32) -> Vec<(u32, String)> {
        let mut found = Vec::new();
        // SAFETY: a process snapshot is read with a correctly sized entry and
        // closed before returning.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return found;
            }
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                if entry.th32ParentProcessID == parent {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    found.push((
                        entry.th32ProcessID,
                        String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase(),
                    ));
                }
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        found
    }

    /// The situation the job exists for: the orchestrator's direct child
    /// (standing in for `cargo run`) starts a grandchild (standing in for the
    /// service), and the orchestrator goes away without running any shutdown
    /// code. Closing the job must take the grandchild with it.
    ///
    /// The outer `cmd` waits about a second before starting the inner one, so
    /// the child is assigned to the job before its grandchild exists — the
    /// same order `cargo run` produces, where building takes far longer.
    #[test]
    fn closing_the_job_kills_a_grandchild_the_child_started() {
        let job = KillOnCloseJob::new().expect("create job");
        let mut child = Command::new("cmd")
            .args([
                "/C",
                "ping -n 2 127.0.0.1 >NUL & cmd /C ping -n 60 127.0.0.1 >NUL",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");
        job.assign(child.as_raw_handle()).expect("assign to job");

        // Find the inner `cmd.exe`, the grandchild.
        let deadline = Instant::now() + Duration::from_secs(20);
        let grandchild_pid = loop {
            if let Some((pid, _)) = children_of(child.id())
                .into_iter()
                .find(|(_, exe)| exe == "cmd.exe")
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "the grandchild never started; children: {:?}",
                children_of(child.id())
            );
            std::thread::sleep(Duration::from_millis(100));
        };

        // Hold a handle before closing the job so a reused PID cannot be
        // mistaken for the grandchild.
        // SAFETY: a plain OpenProcess for SYNCHRONIZE; checked, then closed.
        let grandchild: HANDLE = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, grandchild_pid) };
        assert!(!grandchild.is_null(), "could not open the grandchild");
        // SAFETY: waiting on a valid process handle with a zero timeout.
        let alive_before = unsafe { WaitForSingleObject(grandchild, 0) } != WAIT_OBJECT_0;
        assert!(alive_before, "the grandchild exited before the job closed");

        drop(job);

        // SAFETY: as above, bounded wait.
        let exited = unsafe { WaitForSingleObject(grandchild, 10_000) } == WAIT_OBJECT_0;
        // SAFETY: the handle was opened above and is closed once.
        unsafe {
            CloseHandle(grandchild);
        }
        let _ = child.kill();
        let _ = child.wait();

        assert!(exited, "the grandchild survived the job being closed");
    }
}
