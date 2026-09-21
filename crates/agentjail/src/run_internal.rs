//! POSIX process utilities used by [`crate::run`].
//!
//! Split out from `run.rs` to keep the file holding the public
//! `Jail` / `JailHandle` surface focused on the domain model. These
//! helpers are all OS-level: send a signal, reap a pid, translate a
//! `WaitStatus` into an exit code.

use crate::run::JailPid;
use rustix::process::{WaitOptions, WaitStatus, waitpid};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{Interest, unix::AsyncFd};

/// Kill a process and its entire process group.
pub(crate) fn kill_tree(pid: JailPid) {
    // SAFETY: libc::kill with any pid is defined; the negative pid
    // means "process group `-pid`". Best-effort — ignore errors (the
    // target may already be dead).
    unsafe {
        libc::kill(-pid.as_i32(), libc::SIGKILL);
        libc::kill(pid.as_i32(), libc::SIGKILL);
    }
}

/// Wait for a child to exit, returning a shell-style exit code.
///
/// Fast path: `pidfd_open(pid, 0)` + `AsyncFd::readable()` — a pidfd
/// becomes readable exactly when the child terminates, so tokio wakes
/// us the instant it happens. No polling. Requires Linux 5.3+.
///
/// Fallback: 50 ms `waitpid(NOHANG)` poll loop, for kernels without
/// pidfd or when the syscall is denied (nested sandboxes, etc.). After
/// the first failed attempt, we remember it so repeat calls skip the
/// probe.
pub(crate) async fn wait_for_pid(pid: JailPid) -> i32 {
    if pidfd_supported()
        && let Some(code) = wait_via_pidfd(pid).await
    {
        return code;
    }
    wait_via_poll(pid).await
}

/// Has `pidfd_open` worked this process-lifetime? Probed lazily. Once
/// a kernel responds ENOSYS (or any error we can't recover from), we
/// never probe again — a 50-jail burst would otherwise cost 50 failed
/// syscalls.
static PIDFD_OK: AtomicBool = AtomicBool::new(true);

fn pidfd_supported() -> bool {
    PIDFD_OK.load(Ordering::Relaxed)
}

async fn wait_via_pidfd(pid: JailPid) -> Option<i32> {
    // SAFETY: SYS_pidfd_open with flags=0 — returns a new fd or -1.
    // `pid` is a real child pid that we spawned.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_i32(), 0) };
    if raw < 0 {
        // ENOSYS on pre-5.3 kernels, EPERM when restricted. Either way,
        // disable the fast path for the remainder of this process.
        PIDFD_OK.store(false, Ordering::Relaxed);
        return None;
    }

    // SAFETY: `raw` is a freshly allocated fd; transferring ownership.
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let afd = match AsyncFd::with_interest(fd, Interest::READABLE) {
        Ok(a) => a,
        Err(_) => return None, // no runtime / can't register → fall back
    };

    // A pidfd reports readable exactly when the target process has
    // terminated. Drop the guard immediately — we don't read from it;
    // we just use the readiness edge.
    let _ = afd.readable().await.ok()?;

    match waitpid(pid.to_rustix(), WaitOptions::NOHANG) {
        Ok(Some(status)) => Some(extract_exit_code(status)),
        // The pidfd said "terminated" but waitpid said "not yet" —
        // that's possible only if the reaper lost a race (very rare).
        // Fall back to the poll path rather than spinning.
        _ => Some(wait_via_poll(pid).await),
    }
}

async fn wait_via_poll(pid: JailPid) -> i32 {
    loop {
        match waitpid(pid.to_rustix(), WaitOptions::NOHANG) {
            Ok(Some(status)) => return extract_exit_code(status),
            Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(_) => return -1,
        }
    }
}

/// Translate a POSIX `WaitStatus` into a shell-style exit code:
/// clean exit → its status; killed by signal N → `128 + N`; otherwise
/// `-1`.
pub(crate) fn extract_exit_code(status: WaitStatus) -> i32 {
    if status.exited() {
        status.exit_status().map(|c| c as i32).unwrap_or(-1)
    } else if status.signaled() {
        status
            .terminating_signal()
            .map(|s| 128 + s as i32)
            .unwrap_or(-1)
    } else {
        -1
    }
}
