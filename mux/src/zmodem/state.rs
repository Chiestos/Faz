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

use super::transfer::CANCEL_SEQUENCE;

/// How much of a detected session to hold while the user picks a destination.
///
/// The remote's first frames arrive before `rz` can be attached — `rz` needs
/// the destination directory, which needs the user. Dropping those bytes makes
/// the peer retransmit at best and fail its handshake at worst, so they are
/// buffered. Capped because a user may take a while: past the cap the session
/// is abandoned rather than growing without bound.
pub const PENDING_CAPACITY: usize = 1024 * 1024;

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
    /// Bytes that arrived before a sink existed. Drained into the sink on
    /// [`ZmodemState::begin`].
    buffered: Vec<u8>,
    /// Whether the parser should still divert bytes into `buffered`.
    ///
    /// True while the session is open with no sink; false once a sink is
    /// installed or the session is aborted.
    buffering: bool,
}

/// Shared per-pane ZMODEM state.
#[derive(Default)]
pub struct ZmodemState {
    inner: Mutex<Option<ActiveTransfer>>,
    /// When the last session ended. Used to ignore protocol frames a peer keeps
    /// retransmitting after a transfer, which would otherwise be detected as a
    /// new incoming session and prompt the user again.
    last_finished: Mutex<Option<std::time::Instant>>,
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
        // Carry over anything buffered before the sink existed. Taking the
        // whole slot is simpler than moving one field out of it.
        let buffered = guard.take().map(|t| t.buffered).unwrap_or_default();
        *guard = Some(ActiveTransfer {
            sink: Some(sink),
            cancel: Arc::clone(&cancel),
            buffered,
            buffering: false,
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

    /// Is a session open with no sink installed yet?
    pub fn is_buffering(&self) -> bool {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().is_some_and(|t| t.buffering)
    }

    /// Bytes received before a sink was installed, in arrival order.
    ///
    /// The transfer drains this into the child once it is running, so the
    /// remote's opening frames are not lost while the destination is chosen.
    pub fn buffered(&self) -> Vec<u8> {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().map(|t| t.buffered.clone()).unwrap_or_default()
    }

    /// Record bytes that arrived before a sink existed.
    ///
    /// Returns `false` when the buffer is full, which the caller should treat
    /// as "abandon this session": the peer has sent more than a megabyte with
    /// nobody listening, so the handshake has already failed.
    pub fn buffer_incoming(&self, bytes: &[u8]) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let Some(t) = guard.as_mut() else {
            return false;
        };
        if t.sink.is_some() {
            // A sink exists; the caller should have written there instead.
            return true;
        }
        if !t.buffering {
            // The session was abandoned; keep the old handshake, ignore new
            // bytes so the pane is not frozen.
            return true;
        }
        if t.buffered.len() + bytes.len() > PENDING_CAPACITY {
            return false;
        }
        t.buffered.extend_from_slice(bytes);
        true
    }

    /// Open a session slot with no sink yet, so incoming bytes can be buffered
    /// while the UI decides what to do with them.
    pub fn begin_buffering(&self) {
        let mut guard = self.inner.lock().unwrap();
        if guard.is_none() {
            *guard = Some(ActiveTransfer {
                sink: None,
                cancel: Arc::new(AtomicBool::new(false)),
                buffered: Vec::new(),
                buffering: true,
            });
        }
    }

    /// Is a session too recent for a new detection to be plausible?
    ///
    /// A peer that did not see the final frame keeps retransmitting its header
    /// for a while. Those frames look exactly like a new session starting, and
    /// prompting the user for a transfer they did not ask for is worse than
    /// missing one they did: they can start it from the menu.
    pub fn in_cooldown(&self) -> bool {
        /// Long enough to cover lrzsz's retry interval, short enough that a
        /// user who starts a real transfer right after one finishes is not
        /// blocked for long.
        const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(3);
        self.last_finished
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < COOLDOWN)
    }

    /// End the transfer and put the parser back into normal parsing.
    ///
    /// Dropping the sink closes the relay end, which makes the relay thread
    /// see EOF and exit.
    pub fn finish(&self) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(mut t) = guard.take() {
            t.sink = None;
            t.buffered.clear();
        }
        drop(guard);
        *self.last_finished.lock().unwrap() = Some(std::time::Instant::now());
    }

    /// Abort a pending session that never reached a transfer.
    ///
    /// No child exists yet, so nothing will send CAN on our behalf and the
    /// remote stays in `rz`/`sz` until it times out — which is what left the
    /// pane wedged. This writes the CAN burst straight to the pty.
    ///
    /// Returns whether the write happened.
    pub fn cancel_pending(&self, pty_master_fd: std::os::fd::RawFd) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let took = guard.take();
        let Some(t) = took else {
            log::info!(
                "zmodem: cancel_pending on fd {pty_master_fd} found no pending session; \
                 nothing to abort"
            );
            return false;
        };
        let buffered = t.buffered.len();
        // Nothing was started, so this is a pure abort: no buffered replay.
        drop(t);
        drop(guard);
        // Two bursts: lrzsz aborts after five CANs, and repeating the burst
        // hedges against the first landing while the remote was between
        // frames.
        let bytes = CANCEL_SEQUENCE;
        match crate::zmodem::transfer::write_all(pty_master_fd, &bytes) {
            Ok(n) => {
                *self.last_finished.lock().unwrap() = Some(std::time::Instant::now());
                log::info!(
                    "zmodem: sent {n}-byte cancel burst to remote on fd {pty_master_fd} \
                     (dropped {buffered} buffered bytes)"
                );
                true
            }
            Err(err) => {
                log::warn!(
                    "zmodem: could not send cancel to the remote on fd {pty_master_fd}: {err:#}"
                );
                false
            }
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
    use std::os::fd::AsRawFd;

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
    fn begin_buffering_opens_a_slot_without_a_sink() {
        let state = ZmodemState::new();
        assert!(!state.is_active());
        assert!(!state.is_buffering());

        state.begin_buffering();
        assert!(state.is_buffering(), "session open");
        assert!(!state.is_active(), "but no sink yet");
        assert_eq!(state.sink(), None);
    }

    #[test]
    fn bytes_arriving_before_the_sink_are_buffered() {
        let state = ZmodemState::new();
        state.begin_buffering();
        assert!(state.buffer_incoming(b"**\x18B00"));
        assert!(state.buffer_incoming(b"more"));
        assert_eq!(state.buffered(), b"**\x18B00more");
    }

    #[test]
    fn beginning_a_transfer_carries_the_buffer_over() {
        let state = ZmodemState::new();
        state.begin_buffering();
        state.buffer_incoming(b"opening frames");

        let (relay_end, _child) = make_socketpair().expect("socketpair");
        state.begin(relay_end);

        assert!(state.is_active());
        assert!(!state.is_buffering());
        assert_eq!(
            state.buffered(),
            b"opening frames",
            "the session's opening bytes must survive until the sink exists"
        );
    }

    #[test]
    fn buffering_past_the_capacity_is_refused() {
        let state = ZmodemState::new();
        state.begin_buffering();
        let chunk = vec![0u8; PENDING_CAPACITY / 2];
        assert!(state.buffer_incoming(&chunk));
        assert!(state.buffer_incoming(&chunk));
        // This one would exceed the cap.
        assert!(
            !state.buffer_incoming(b"x"),
            "overflow must be reported so the caller abandons the session"
        );
    }

    #[test]
    fn buffering_with_no_session_is_refused() {
        let state = ZmodemState::new();
        assert!(!state.buffer_incoming(b"stray"));
    }

    #[test]
    fn buffering_after_a_sink_exists_reports_success_without_storing() {
        // The caller should be writing to the sink at that point; returning
        // true keeps it from treating the situation as an overflow.
        let state = ZmodemState::new();
        let (relay_end, _child) = make_socketpair().expect("socketpair");
        state.begin(relay_end);
        assert!(state.buffer_incoming(b"ignored"));
        assert!(state.buffered().is_empty());
    }

    #[test]
    fn cancelling_a_pending_session_writes_the_can_burst() {
        // No child exists, so nothing else would tell the remote to stop. The
        // write goes straight to the pty; a socketpair stands in for it here.
        let (pty_side, remote_side) = crate::zmodem::transfer::make_socketpair().expect("pair");
        let state = ZmodemState::new();
        state.begin_buffering();
        state.buffer_incoming(b"**\x18B01");

        let wrote = state.cancel_pending(pty_side.as_raw_fd());
        assert!(wrote, "cancel must reach the pty");

        let mut buf = [0u8; 64];
        let n = nix::unistd::read(remote_side.as_raw_fd(), &mut buf).expect("read");
        assert!(n >= 5, "lrzsz aborts after 5 CANs, got {n}");
        assert!(buf[..n].iter().all(|b| *b == 0x18));

        assert!(!state.is_buffering(), "session is over");
        assert!(state.buffered().is_empty(), "nothing replays later");
        assert!(state.in_cooldown(), "retransmits are ignored for a while");
    }

    #[test]
    fn cancelling_without_a_session_is_a_no_op() {
        let (pty_side, _remote) = crate::zmodem::transfer::make_socketpair().expect("pair");
        let state = ZmodemState::new();
        assert!(!state.cancel_pending(pty_side.as_raw_fd()));
    }

    #[test]
    fn finish_clears_the_buffer() {
        let state = ZmodemState::new();
        state.begin_buffering();
        state.buffer_incoming(b"data");
        state.finish();
        assert!(!state.is_buffering());
        assert!(state.buffered().is_empty());
    }

    #[test]
    fn a_finished_session_puts_the_pane_in_cooldown() {
        let state = ZmodemState::new();
        assert!(!state.in_cooldown(), "nothing finished yet");

        let (relay_end, _child) = make_socketpair().expect("socketpair");
        state.begin(relay_end);
        assert!(!state.in_cooldown(), "an active transfer is not a cooldown");

        state.finish();
        assert!(
            state.in_cooldown(),
            "a peer retransmitting after the transfer ends must not be detected"
        );
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
