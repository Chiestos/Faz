//! Per-pane ZMODEM state: what the output parser consults, and what the UI
//! drives.
//!
//! A transfer has three participants:
//!
//!   * the pane's output parser thread, which must stop feeding bytes to the
//!     VT parser and hand them to the local child instead;
//!   * the transfer's own thread, which owns the child and the relay;
//!   * the UI, which starts a transfer and needs to know when it ends.
//!
//! [`ZmodemState`] is the shared cell between them. The parser thread only
//! ever reads [`ZmodemState::sink`], which is a plain fd lookup, so the hot
//! path stays cheap: one mutex lock and an `Option<RawFd>` copy per read.

use std::os::fd::{OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A transfer that the parser thread must divert bytes to.
///
/// Owned by the state cell. The fd is the relay end of the transfer's
/// socketpair: bytes written here reach the local `sz`/`rz` child.
struct ActiveTransfer {
    /// Write end for the parser thread. `None` once the transfer has finished,
    /// which is what flips the parser back to normal parsing.
    sink: Option<OwnedFd>,
    /// Set by [`ZmodemState::cancel`]; the transfer thread polls it.
    cancel: Arc<AtomicBool>,
}

/// Shared per-pane ZMODEM state.
#[derive(Default)]
pub struct ZmodemState {
    inner: Mutex<Option<ActiveTransfer>>,
}

impl ZmodemState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install an active transfer and return its cancel flag.
    ///
    /// `sink` is the relay end of the transfer's socketpair; the caller keeps
    /// the child end. Replaces any previous transfer, which by construction has
    /// already finished — [`ZmodemState::finish`] clears the slot before the
    /// transfer thread reports completion.
    pub fn begin(&self, sink: OwnedFd) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut guard = self.inner.lock().unwrap();
        *guard = Some(ActiveTransfer {
            sink: Some(sink),
            cancel: Arc::clone(&cancel),
        });
        cancel
    }

    /// The fd the output parser should divert raw bytes to, if a transfer is
    /// running on this pane.
    ///
    /// Called from the parser thread on every read. Takes the lock only long
    /// enough to copy the fd.
    pub fn sink(&self) -> Option<RawFd> {
        use std::os::fd::AsRawFd;
        let guard = self.inner.lock().unwrap();
        guard.as_ref().and_then(|t| t.sink.as_ref().map(|fd| fd.as_raw_fd()))
    }

    /// End the transfer and put the parser back into normal parsing.
    ///
    /// Dropping the sink closes the relay end, which makes the relay thread
    /// see EOF and exit.
    pub fn finish(&self) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(mut t) = guard.take() {
            t.sink = None;
        }
    }

    /// Ask an in-flight transfer to abort. Sends nothing itself; the transfer
    /// thread owns the CAN burst so it travels the same path as the payload.
    pub fn cancel(&self) {
        let guard = self.inner.lock().unwrap();
        if let Some(t) = guard.as_ref() {
            t.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn is_active(&self) -> bool {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().is_some_and(|t| t.sink.is_some())
    }
}

impl std::fmt::Debug for ZmodemState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ZmodemState(active={})", self.is_active())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::transfer::make_socketpair;

    #[test]
    fn idle_state_has_no_sink() {
        let state = ZmodemState::new();
        assert_eq!(state.sink(), None);
        assert!(!state.is_active());
    }

    #[test]
    fn begin_exposes_the_sink_and_finish_clears_it() {
        let state = ZmodemState::new();
        let (relay_end, _child_end) = make_socketpair().expect("socketpair");

        let cancel = state.begin(relay_end);
        assert!(state.is_active());
        assert!(state.sink().is_some());

        state.finish();
        assert_eq!(state.sink(), None);
        assert!(!state.is_active());
        // The cancel flag outlives the slot so a finishing thread can read it.
        assert!(!cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_sets_the_flag_of_the_active_transfer() {
        let state = ZmodemState::new();
        let (relay_end, _child_end) = make_socketpair().expect("socketpair");
        let cancel = state.begin(relay_end);

        assert!(!cancel.load(Ordering::SeqCst));
        state.cancel();
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_without_a_transfer_is_a_no_op() {
        let state = ZmodemState::new();
        state.cancel();
        assert!(!state.is_active());
    }

    #[test]
    fn finish_is_idempotent() {
        let state = ZmodemState::new();
        let (relay_end, _child_end) = make_socketpair().expect("socketpair");
        state.begin(relay_end);
        state.finish();
        state.finish();
        assert!(!state.is_active());
    }

    #[test]
    fn a_second_begin_replaces_the_first() {
        let state = ZmodemState::new();
        let (a, _a_child) = make_socketpair().expect("socketpair a");
        let (b, _b_child) = make_socketpair().expect("socketpair b");

        let first = state.begin(a);
        let second = state.begin(b);

        // The new cancel flag is the live one; the old transfer's is orphaned.
        state.cancel();
        assert!(!first.load(Ordering::SeqCst));
        assert!(second.load(Ordering::SeqCst));
        assert!(state.is_active());
    }
}
