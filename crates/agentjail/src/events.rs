//! Event streaming for jail monitoring.
//!
//! Allows callers to receive real-time updates from running jails.

use crate::run::JailPid;
use std::time::Duration;
use tokio::sync::mpsc;

/// Events emitted by a running jail.
#[derive(Debug, Clone)]
pub enum JailEvent {
    /// Jail started with given PID.
    Started { pid: JailPid },
    /// Line written to stdout.
    Stdout(String),
    /// Line written to stderr.
    Stderr(String),
    /// Jail completed with exit code.
    Completed { exit_code: i32, duration: Duration },
    /// Jail was killed.
    Killed,
    /// Jail timed out.
    TimedOut,
    /// Jail was killed by OOM killer.
    OomKilled,
    /// A live fork of this jail was created.
    Forked { fork_pid: JailPid },
}

/// Sender for jail events (held by the jail runner).
pub type EventSender = mpsc::UnboundedSender<JailEvent>;

/// Receiver for jail events (held by the caller for monitoring).
pub type EventReceiver = mpsc::UnboundedReceiver<JailEvent>;

/// Create a new event channel.
pub fn channel() -> (EventSender, EventReceiver) {
    mpsc::unbounded_channel()
}
