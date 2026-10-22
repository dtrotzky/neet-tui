//! Cancellation for background child processes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Shared handle to cancel a background job.
///
/// A job that blocks on I/O registers its child's pid so `cancel` can signal it
/// from another thread. The job must call [`Cancel::unregister`] before reaping
/// the child, so a pid is never signalled after it could have been reused.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Inner>);

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    pid: Mutex<Option<u32>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        // Signal while holding the lock: the job can't unregister (and then reap
        // the child, freeing its pid for reuse) until the kill is done.
        let mut slot = self.0.pid.lock().unwrap();
        if let Some(pid) = slot.take() {
            terminate(pid);
        }
        drop(slot);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub fn register(&self, pid: u32) {
        let mut slot = self.0.pid.lock().unwrap();
        if self.is_cancelled() {
            terminate(pid);
        } else {
            *slot = Some(pid);
        }
    }

    pub fn unregister(&self) {
        self.0.pid.lock().unwrap().take();
    }
}

fn terminate(pid: u32) {
    if let Some(pid) = to_pid(pid) {
        // SAFETY: plain syscall; the pid belongs to a child that has not been reaped yet.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

/// Rejects 0 and out-of-range values, which `kill` would treat as "our own group"
/// or a different target entirely.
fn to_pid(pid: u32) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|p| *p > 0)
}

/// Kills a whole process group (used for `sh -c "build && run"` chains).
pub fn kill_group(pgid: u32) {
    if let Some(pgid) = to_pid(pgid) {
        // SAFETY: plain syscall on a process group we created and have not reaped.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}
