//! Graceful process termination for the PROCESSES panel.
//!
//! Only `SIGTERM` is ever sent: the goal is to *ask* a process to exit so it
//! can flush state and clean up, never to force it. The UI asks for an explicit
//! confirmation before anything is signalled, and targets that cannot be
//! signalled safely (init, amdtop itself, other users' processes) are refused
//! up front.

use std::fmt;
use std::io;

/// A process the user asked to stop, captured while the dialog is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillTarget {
    pub pid: i32,
    pub name: String,
    pub total_kb: u64,
    pub mem_kb: u64,
    pub swap_kb: u64,
}

/// Why a process may not be signalled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillRefusal {
    /// PID 0/1: the kernel or init. Never touch it.
    Init,
    /// Signalling the monitor itself would just kill the monitor.
    SelfProcess,
    /// Another user's process; only root may signal it.
    NotOwner,
}

impl fmt::Display for KillRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KillRefusal::Init => write!(f, "refusing to signal init (pid 1)"),
            KillRefusal::SelfProcess => write!(f, "refusing to signal amdtop itself"),
            KillRefusal::NotOwner => write!(f, "only processes you own can be signalled"),
        }
    }
}

/// Decide whether `pid` may be signalled by a process running as `euid`.
pub fn check_target(pid: i32, owner_uid: u32, euid: u32, self_pid: i32) -> Result<(), KillRefusal> {
    if pid <= 1 {
        return Err(KillRefusal::Init);
    }
    if pid == self_pid {
        return Err(KillRefusal::SelfProcess);
    }
    // Root may signal anything; everybody else stays within their own uid.
    if owner_uid != euid && euid != 0 {
        return Err(KillRefusal::NotOwner);
    }
    Ok(())
}

/// Ask the process to exit. SIGTERM only — there is deliberately no SIGKILL
/// path, so a stuck process keeps running instead of dying mid-write.
pub fn send_sigterm(pid: i32) -> io::Result<()> {
    // SAFETY: kill(2) with SIGTERM takes no pointers and cannot corrupt memory;
    // a failure is reported through errno below.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Effective user id of this process.
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and never fails.
    unsafe { libc::geteuid() }
}

/// PID of this process.
pub fn self_pid() -> i32 {
    std::process::id() as i32
}

/// Short, human-readable explanation for a failed `send_sigterm`.
pub fn describe_error(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::ESRCH) => "process already exited",
        Some(libc::EPERM) => "permission denied",
        _ => "could not send SIGTERM",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{KillRefusal, check_target, describe_error, effective_uid, send_sigterm};

    const ME: i32 = 4242;
    const MY_UID: u32 = 1000;

    #[test]
    fn own_process_can_be_signalled() {
        assert_eq!(check_target(1234, MY_UID, MY_UID, ME), Ok(()));
    }

    #[test]
    fn init_is_never_signalled() {
        assert_eq!(check_target(1, 0, 0, ME), Err(KillRefusal::Init));
        assert_eq!(check_target(0, 0, 0, ME), Err(KillRefusal::Init));
    }

    #[test]
    fn amdtop_never_signals_itself() {
        assert_eq!(
            check_target(ME, MY_UID, MY_UID, ME),
            Err(KillRefusal::SelfProcess)
        );
    }

    #[test]
    fn other_users_processes_are_refused_without_root() {
        assert_eq!(
            check_target(1234, 0, MY_UID, ME),
            Err(KillRefusal::NotOwner)
        );
        // root (euid 0) may signal them
        assert_eq!(check_target(1234, MY_UID, 0, ME), Ok(()));
    }

    #[test]
    fn errors_are_explained_in_plain_words() {
        let gone = std::io::Error::from_raw_os_error(libc::ESRCH);
        let denied = std::io::Error::from_raw_os_error(libc::EPERM);
        let other = std::io::Error::from_raw_os_error(libc::EINVAL);

        assert_eq!(describe_error(&gone), "process already exited");
        assert_eq!(describe_error(&denied), "permission denied");
        assert_eq!(describe_error(&other), "could not send SIGTERM");
    }

    #[test]
    fn sigterm_terminates_a_child_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");

        send_sigterm(child.id() as i32).expect("send SIGTERM");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !exited {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(exited, "sleep should exit after SIGTERM");
    }

    #[test]
    fn signalling_a_missing_pid_reports_an_error() {
        let error = send_sigterm(i32::MAX).expect_err("no such process");
        assert_eq!(describe_error(&error), "process already exited");
    }

    #[test]
    fn effective_uid_is_available() {
        // Just make sure the syscall wrapper is wired up.
        let _ = effective_uid();
    }
}
