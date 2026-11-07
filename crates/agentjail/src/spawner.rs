//! The one thread every jail is forked from.
//!
//! The jail's setup process arms `PR_SET_PDEATHSIG(SIGKILL)` so it can't
//! outlive the embedder. Linux sends that signal when the *thread* that
//! forked the child exits, not the process. Forking from whatever thread
//! called [`Jail::spawn`](crate::Jail::spawn) would tie each jail to that
//! thread (a retired tokio blocking-pool thread, a short-lived
//! `std::thread`), and the running jail would be SIGKILLed with it.
//!
//! So every fork runs on a single, lazily started thread that lives as
//! long as the process ("agentjail-spawner"). The parent-death signal
//! then means "the embedder process died". Only the `fork` itself runs
//! there: the handshake, cgroup and veth setup stay on the caller's
//! thread (they act on pids and fds, which are process-wide).

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::{Mutex, PoisonError};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Run `f` on the spawner thread and block until it returns.
///
/// A panic in `f` is caught on the spawner (which keeps serving other
/// spawns) and resumed on the caller. `f` may borrow from the caller's
/// stack: this function doesn't return until `f` has run or been
/// dropped.
pub(crate) fn run<R, F>(f: F) -> io::Result<R>
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    let (tx, rx) = mpsc::sync_channel::<std::thread::Result<R>>(1);
    let job = move || {
        let r = panic::catch_unwind(AssertUnwindSafe(f));
        // Last use of `f`'s borrows: `f` has returned or unwound.
        let _ = tx.send(r);
    };
    let job: Box<dyn FnOnce() + Send + '_> = Box::new(job);
    // SAFETY: only the lifetime is erased. We block below until the job
    // is finished with its borrows: `recv` returns `Ok` after `f`
    // completed (the send is the job's last action involving them), and
    // `Err` only once `tx` was dropped, i.e. the job was dropped or has
    // finished. If `submit` fails, it drops the job before returning.
    let job: Job = unsafe { std::mem::transmute::<Box<dyn FnOnce() + Send + '_>, Job>(job) };
    submit(job)?;
    match rx.recv() {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(payload)) => panic::resume_unwind(payload),
        Err(_) => Err(io::Error::other("agentjail spawner thread dropped the job")),
    }
}

/// Queue `job` on the spawner thread, starting it on first use (or
/// again, should it ever have exited).
fn submit(mut job: Job) -> io::Result<()> {
    static SPAWNER: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);
    let mut tx = SPAWNER.lock().unwrap_or_else(PoisonError::into_inner);
    for _ in 0..2 {
        if tx.is_none() {
            *tx = Some(start()?);
        }
        let sender = tx.as_ref().expect("spawner sender was just set");
        match sender.send(job) {
            Ok(()) => return Ok(()),
            Err(mpsc::SendError(j)) => {
                job = j;
                *tx = None;
            }
        }
    }
    Err(io::Error::other("agentjail spawner thread is gone"))
}

fn start() -> io::Result<mpsc::Sender<Job>> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("agentjail-spawner".into())
        .spawn(move || {
            // Never take process-directed signals meant for the
            // embedder's own threads. The forked child starts from an
            // empty mask (see `run::child_entry`).
            block_all_signals();
            for job in rx {
                // `run` already catches panics in the job's work; this
                // only guards the loop itself.
                let _ = panic::catch_unwind(AssertUnwindSafe(job));
            }
        })?;
    Ok(tx)
}

fn block_all_signals() {
    // SAFETY: a zeroed sigset_t filled by sigfillset; the call only
    // changes this thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut set);
        libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_on_one_long_lived_named_thread() {
        let name = || std::thread::current().name().map(str::to_string);
        let a = run(name).unwrap();
        assert_eq!(a.as_deref(), Some("agentjail-spawner"));
        // From another, short-lived thread: same spawner thread.
        let id = run(|| std::thread::current().id()).unwrap();
        let id2 = std::thread::spawn(|| run(|| std::thread::current().id()).unwrap())
            .join()
            .unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn borrows_from_the_caller() {
        let mut v = vec![1, 2, 3];
        let n = run(|| {
            v.push(4);
            v.len()
        })
        .unwrap();
        assert_eq!(n, 4);
        assert_eq!(v, [1, 2, 3, 4]);
    }

    #[test]
    fn a_panic_reaches_the_caller_and_the_spawner_survives() {
        let before = run(|| std::thread::current().id()).unwrap();
        let r = std::panic::catch_unwind(|| run(|| panic!("boom")));
        assert!(r.is_err(), "panic should resume on the caller");
        let after = run(|| std::thread::current().id()).unwrap();
        assert_eq!(before, after, "spawner thread was replaced");
    }

    #[test]
    fn spawner_blocks_signals() {
        let blocked = run(|| unsafe {
            let mut cur: libc::sigset_t = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut cur);
            libc::sigismember(&cur, libc::SIGTERM) == 1
        })
        .unwrap();
        assert!(blocked);
    }
}
