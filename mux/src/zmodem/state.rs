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
use std::sync::{Arc, Condvar, Mutex};

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

/// What this pane's ZMODEM channel is doing right now.
///
/// One value, owned by one place, so "is a transfer running" has a single
/// answer. The previous design spread that across a sink slot, a buffering
/// flag, a relay-ownership flag, and a cooldown timer, and each of them could
/// disagree with the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZmodemMode {
    /// Normal terminal operation. The pane's parser reads and renders; input
    /// goes to the shell.
    #[default]
    Idle,
    /// A header was seen and the destination is being chosen. Peer bytes are
    /// held so the handshake survives the wait, but no child is running yet.
    Pending,
    /// The local `rz` is receiving a file.
    Receiving,
    /// The local `sz` is sending a file.
    Sending,
}

impl ZmodemMode {
    /// Does a transfer own the pty's byte stream?
    ///
    /// True for both transfer directions: while either holds the channel,
    /// nothing else may read or write it — not the parser, not the keyboard,
    /// not a paste, not an injected command. `Pending` is excluded because the
    /// child does not exist yet, so the parser still needs to consume output
    /// (to see the peer's retransmissions) and the user still needs the pane.
    pub fn owns_channel(self) -> bool {
        matches!(self, ZmodemMode::Receiving | ZmodemMode::Sending)
    }

    /// Is a session open that has not yet reached a transfer?
    pub fn is_pending(self) -> bool {
        matches!(self, ZmodemMode::Pending)
    }
}

/// Hands the pty's read side between the pane's roundtrip thread and a
/// transfer's relay, with no window in which both can read.
///
/// The pane's thread brackets every read with [`ReadGate::enter_pane_read`];
/// the transfer takes the gate for its whole lifetime with
/// [`ReadGate::claim`], which waits until the pane is *between* reads.
///
/// Blocking matters for correctness, not just CPU. A reader that checks the
/// flag and then calls `read` has two steps, and a claim landing between them
/// lets it consume a chunk that belonged to the child — the child sees a gap
/// and asks for a resend, and once that starts it never catches up.
#[derive(Default)]
pub struct ReadGate {
    inner: Mutex<GateState>,
    released: Condvar,
}

#[derive(Default)]
struct GateState {
    /// A transfer owns the pty's read side.
    held_by_relay: bool,
    /// A pane read is in progress right now.
    pane_reading: bool,
}

/// Proof that the pane's thread is between reads, held for one read call.
pub struct PaneRead<'a> {
    gate: &'a ReadGate,
}

impl Drop for PaneRead<'_> {
    fn drop(&mut self) {
        let mut inner = self.gate.inner.lock().unwrap();
        inner.pane_reading = false;
        self.gate.released.notify_all();
    }
}

impl ReadGate {
    /// Take the gate for the duration of one pty read.
    ///
    /// Blocks while a transfer holds the pty, and returns only when the read
    /// can proceed. Drop the guard as soon as `read` returns so a waiting
    /// transfer can take over.
    pub fn enter_pane_read(&self) -> PaneRead<'_> {
        let mut inner = self.inner.lock().unwrap();
        while inner.held_by_relay {
            inner = self.released.wait(inner).unwrap();
        }
        inner.pane_reading = true;
        drop(inner);
        PaneRead { gate: self }
    }

    /// Take the pty's read side for a transfer.
    ///
    /// Waits for any read already in progress, so the claim cannot land
    /// mid-chunk and strand part of a transfer in the pane's buffer.
    pub fn claim(&self) {
        let mut inner = self.inner.lock().unwrap();
        while inner.pane_reading {
            inner = self.released.wait(inner).unwrap();
        }
        inner.held_by_relay = true;
    }

    /// Release the pty's read side and wake the pane's thread.
    pub fn release(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.held_by_relay = false;
        self.released.notify_all();
    }

    /// Is a transfer holding the read side right now?
    pub fn is_held(&self) -> bool {
        self.inner.lock().unwrap().held_by_relay
    }
}

/// Shared per-pane ZMODEM state.
#[derive(Default)]
pub struct ZmodemState {
    inner: Mutex<Option<ActiveTransfer>>,
    /// The one authoritative description of what the channel is doing.
    mode: Mutex<ZmodemMode>,
    /// When the last session ended. Used to ignore protocol frames a peer keeps
    /// retransmitting after a transfer, which would otherwise be detected as a
    /// new incoming session and prompt the user again.
    last_finished: Mutex<Option<std::time::Instant>>,
    /// Bytes the pane's parser has handed to the transfer's sink.
    ///
    /// The relay's own tally cannot report this direction: the parser owns the
    /// pty's read side, so the relay never sees peer-to-child bytes at all and
    /// its count there is structurally zero. Diagnostics that quoted it were
    /// reading a number that could only ever be zero.
    peer_bytes_to_child: std::sync::atomic::AtomicU64,
    /// Set while the transfer's relay owns the pty's read side.
    ///
    /// The pane's roundtrip thread must not read the pty for the duration:
    /// two readers on one pty take turns consuming bytes, so a ZMODEM frame
    /// arrives split across both and neither side can reassemble it. This is
    /// the difference from zssh, which owns its pty outright — Faz has to hand
    /// the read side over explicitly.
    ///
    /// A plain flag is not enough. Checking it and then calling `read` are two
    /// steps, and the relay can claim the pty in between; the roundtrip thread
    /// then reads a chunk that belonged to the transfer and drops it, which the
    /// child sees as a missing block and asks to be resent. Once that starts it
    /// never catches up. The gate makes the two steps atomic and gives the
    /// waiter somewhere to block instead of polling.
    read_gate: ReadGate,
}

impl ZmodemMode {
    fn from_direction(direction: super::Direction) -> Self {
        match direction {
            super::Direction::Upload => ZmodemMode::Sending,
            super::Direction::Download => ZmodemMode::Receiving,
        }
    }
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
    pub fn begin(&self, sink: OwnedFd, direction: super::Direction) -> Arc<AtomicBool> {
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
        drop(guard);
        // The child exists now, so the channel changes hands in the same step
        // that installs the sink. Leaving this to the caller would allow a
        // window where a sink exists but the mode still says Idle.
        *self.mode.lock().unwrap() = ZmodemMode::from_direction(direction);
        cancel
    }

    /// The current mode. Every other "is X happening" question derives from it.
    pub fn mode(&self) -> ZmodemMode {
        *self.mode.lock().unwrap()
    }

    /// Record that a header arrived and the destination is being chosen.
    pub fn enter_pending(&self) {
        let mut mode = self.mode.lock().unwrap();
        if *mode == ZmodemMode::Idle {
            *mode = ZmodemMode::Pending;
        }
    }

    /// The fd the output parser should divert raw bytes to, if a transfer is
    /// running on this pane.
    ///
    /// Called from the parser thread on every read. Takes the lock only long
    /// enough to copy the fd.
    pub fn sink(&self) -> Option<RawFd> {
        use std::os::fd::AsRawFd;
        let guard = self.inner.lock().unwrap();
        guard
            .as_ref()
            .and_then(|t| t.sink.as_ref().map(|fd| fd.as_raw_fd()))
    }

    /// Count bytes the parser diverted to the child.
    pub fn note_peer_bytes(&self, n: usize) {
        self.peer_bytes_to_child
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    }

    /// Hand the pty's read side to the transfer's relay, or take it back.
    ///
    /// While this is set the pane's roundtrip thread must not read the pty.
    /// Both readers see the same bytes otherwise, each getting a fragment, and
    /// a fragment is useless to a protocol that needs whole frames.
    pub fn set_relay_owns_pty(&self, owns: bool) {
        if owns {
            self.read_gate.claim();
        } else {
            self.read_gate.release();
        }
    }

    /// Take the pty's read side for the duration of one read.
    ///
    /// The returned guard must be dropped as soon as the read returns, so a
    /// waiting transfer can take over.
    pub fn enter_pane_read(&self) -> PaneRead<'_> {
        self.read_gate.enter_pane_read()
    }

    /// Is the transfer's relay reading the pty right now?
    pub fn relay_owns_pty(&self) -> bool {
        self.read_gate.is_held()
    }

    /// Bytes the parser has diverted to the local child since this state began.
    pub fn peer_bytes_to_child(&self) -> u64 {
        self.peer_bytes_to_child
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Is a session open with no sink installed yet?
    pub fn is_buffering(&self) -> bool {
        self.mode() == ZmodemMode::Pending
    }

    /// Bytes received before a sink was installed, in arrival order.
    ///
    /// The transfer drains this into the child once it is running, so the
    /// remote's opening frames are not lost while the destination is chosen.
    pub fn buffered(&self) -> Vec<u8> {
        let guard = self.inner.lock().unwrap();
        guard
            .as_ref()
            .map(|t| t.buffered.clone())
            .unwrap_or_default()
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
            drop(guard);
            self.enter_pending();
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
        // The mode and the relay hand-off are cleared in the same critical
        // step as the slot. Any path that ends a session goes through here, so
        // a stale Pending/Sending/Receiving cannot outlive its transfer.
        *self.mode.lock().unwrap() = ZmodemMode::Idle;
        self.read_gate.release();
        *self.last_finished.lock().unwrap() = Some(std::time::Instant::now());
    }

    /// End a session the user cancelled, and forget the cooldown.
    ///
    /// The cooldown exists to ignore a peer retransmitting its header after a
    /// transfer that actually ran. A cancelled picker never exchanged a byte,
    /// so there is nothing to ignore — and leaving the timer set means the user
    /// runs `rz` again immediately and Faz silently declines to notice it for
    /// three seconds, which reads as a broken state machine.
    pub fn finish_cancelled(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.take();
        drop(guard);
        *self.mode.lock().unwrap() = ZmodemMode::Idle;
        self.read_gate.release();
        *self.last_finished.lock().unwrap() = None;
    }

    /// Abort a pending session that never reached a transfer.
    ///
    /// No child exists yet, so nothing will send CAN on our behalf and the
    /// remote stays in `rz`/`sz` until it times out — which is what left the
    /// pane wedged. This writes the CAN burst straight to the pty.
    ///
    /// The burst goes out whether or not a session is still recorded. The
    /// remote is the authority on whether it is waiting: it entered `rz`/`sz`
    /// on its own, and a cancel is only meaningful because of that. Gating the
    /// write on our own slot means a slot cleared anywhere else (a buffer
    /// overflow, a re-entry) silently turns "cancel" into "restore parsing",
    /// leaving the remote wedged with nothing left to time out cleanly.
    ///
    /// Returns whether the write happened.
    pub fn cancel_pending(&self, pty_master_fd: std::os::fd::RawFd) -> bool {
        let (took, buffered) = {
            let mut guard = self.inner.lock().unwrap();
            match guard.take() {
                Some(t) => {
                    let len = t.buffered.len();
                    (true, len)
                }
                None => (false, 0),
            }
        };
        if !took {
            log::info!(
                "zmodem: no pending session on fd {pty_master_fd}; cancelling the remote anyway"
            );
        }
        // The slot is gone, so the mode has to follow it in the same breath.
        // Any caller that then asks "is a session pending" must get Idle, not
        // a Pending with no session behind it.
        *self.mode.lock().unwrap() = ZmodemMode::Idle;
        self.read_gate.release();
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
        self.mode().owns_channel()
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

        let cancel = state.begin(relay_end, crate::zmodem::Direction::Upload);
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
        let cancel = state.begin(relay_end, crate::zmodem::Direction::Upload);

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
        state.begin(relay_end, crate::zmodem::Direction::Upload);

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
        state.begin(relay_end, crate::zmodem::Direction::Upload);
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
    fn cancelling_without_a_session_still_reaches_the_remote() {
        // The remote is what is waiting, not this slot. A cancel whose slot was
        // cleared elsewhere must still stop the peer, or the pane is wedged
        // with no session left to time out.
        let (pty_side, remote_side) = crate::zmodem::transfer::make_socketpair().expect("pair");
        let state = ZmodemState::new();
        assert!(state.cancel_pending(pty_side.as_raw_fd()));

        let mut buf = [0u8; 64];
        let n = nix::unistd::read(remote_side.as_raw_fd(), &mut buf).expect("read");
        assert!(n >= 5, "lrzsz aborts after 5 CANs, got {n}");
        assert!(buf[..n].iter().all(|b| *b == 0x18));
        assert!(state.in_cooldown(), "the peer's replies are ignored");
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
        state.begin(relay_end, crate::zmodem::Direction::Upload);
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
        state.begin(relay_end, crate::zmodem::Direction::Upload);
        state.finish();
        state.finish();
        assert!(!state.is_active());
    }

    #[test]
    fn a_second_begin_replaces_the_first() {
        let state = ZmodemState::new();
        let (a, _a_child) = make_socketpair().expect("socketpair a");
        let (b, _b_child) = make_socketpair().expect("socketpair b");

        let first = state.begin(a, crate::zmodem::Direction::Upload);
        let second = state.begin(b, crate::zmodem::Direction::Upload);

        // The new cancel flag is the live one; the old transfer's is orphaned.
        state.cancel();
        assert!(!first.load(Ordering::SeqCst));
        assert!(second.load(Ordering::SeqCst));
        assert!(state.is_active());
    }
}

#[cfg(test)]
mod mode_tests {
    use super::{ZmodemMode, ZmodemState};
    use crate::zmodem::Direction;

    fn state() -> ZmodemState {
        ZmodemState::new()
    }

    #[test]
    fn a_fresh_pane_is_idle() {
        assert_eq!(state().mode(), ZmodemMode::Idle);
        assert!(!state().is_active());
        assert!(!state().is_buffering());
    }

    #[test]
    fn detecting_a_header_moves_to_pending() {
        let s = state();
        s.begin_buffering();
        assert_eq!(s.mode(), ZmodemMode::Pending);
        assert!(s.is_buffering());
        // Pending does not own the channel: the picker still needs the pane.
        assert!(!s.is_active());
    }

    #[test]
    fn installing_the_child_claims_the_channel() {
        let s = state();
        s.begin_buffering();
        let (relay_end, _child) = crate::zmodem::transfer::make_socketpair().expect("pair");
        s.begin(relay_end, Direction::Upload);
        assert_eq!(s.mode(), ZmodemMode::Sending);
        assert!(s.is_active(), "a running transfer must own the channel");
        assert!(!s.is_buffering());
    }

    #[test]
    fn download_and_upload_are_distinguished() {
        let (a, _) = crate::zmodem::transfer::make_socketpair().expect("pair");
        let up = state();
        up.begin(a, Direction::Upload);
        assert_eq!(up.mode(), ZmodemMode::Sending);

        let (b, _) = crate::zmodem::transfer::make_socketpair().expect("pair");
        let down = state();
        down.begin(b, Direction::Download);
        assert_eq!(down.mode(), ZmodemMode::Receiving);
    }

    #[test]
    fn finishing_returns_to_idle_and_releases_the_read_side() {
        let s = state();
        let (relay_end, _child) = crate::zmodem::transfer::make_socketpair().expect("pair");
        s.begin(relay_end, Direction::Upload);
        s.set_relay_owns_pty(true);

        s.finish();
        assert_eq!(s.mode(), ZmodemMode::Idle);
        assert!(!s.is_active());
        assert!(!s.relay_owns_pty(), "the pane must get its read side back");
    }

    #[test]
    fn a_cancelled_pending_session_forgets_its_cooldown() {
        // The failure this guards: cancel the picker, run `rz` again, and Faz
        // ignores the new header for three seconds.
        let s = state();
        s.begin_buffering();
        s.finish_cancelled();
        assert_eq!(s.mode(), ZmodemMode::Idle);
        assert!(
            !s.in_cooldown(),
            "a cancel must not make the next transfer look like a retransmit"
        );
    }

    #[test]
    fn a_finished_transfer_still_cools_down() {
        // The opposite case: a transfer that really ran may be followed by the
        // peer retransmitting its header, and that must stay suppressed.
        let s = state();
        let (relay_end, _child) = crate::zmodem::transfer::make_socketpair().expect("pair");
        s.begin(relay_end, Direction::Upload);
        s.finish();
        assert!(s.in_cooldown());
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::ZmodemState;

    #[test]
    fn read_side_starts_with_the_pane() {
        let state = ZmodemState::new();
        assert!(!state.relay_owns_pty());
    }

    #[test]
    fn read_side_can_be_handed_over_and_returned() {
        let state = ZmodemState::new();
        state.set_relay_owns_pty(true);
        assert!(state.relay_owns_pty());
        state.set_relay_owns_pty(false);
        assert!(!state.relay_owns_pty());
    }
}

#[cfg(test)]
mod read_gate_tests {
    use super::ReadGate;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn an_idle_gate_lets_the_pane_read_immediately() {
        let gate = ReadGate::default();
        let started = Instant::now();
        drop(gate.enter_pane_read());
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn a_claim_blocks_the_pane_until_release() {
        let gate = Arc::new(ReadGate::default());
        gate.claim();

        let entered = Arc::new(AtomicBool::new(false));
        let waiter = {
            let gate = Arc::clone(&gate);
            let entered = Arc::clone(&entered);
            std::thread::spawn(move || {
                drop(gate.enter_pane_read());
                entered.store(true, Ordering::SeqCst);
            })
        };

        std::thread::sleep(Duration::from_millis(80));
        assert!(
            !entered.load(Ordering::SeqCst),
            "the pane must not read while a transfer holds the pty"
        );

        gate.release();
        waiter.join().expect("waiter");
        assert!(entered.load(Ordering::SeqCst), "release must wake the pane");
    }

    #[test]
    fn a_claim_waits_for_a_read_already_in_progress() {
        // The transfer must not take the pty mid-chunk: the rest of that chunk
        // would be read by the pane and lost, and the child would ask for a
        // resend of data that did arrive.
        let gate = Arc::new(ReadGate::default());
        let read = gate.enter_pane_read();

        let claimed = Arc::new(AtomicBool::new(false));
        let claimer = {
            let gate = Arc::clone(&gate);
            let claimed = Arc::clone(&claimed);
            std::thread::spawn(move || {
                gate.claim();
                claimed.store(true, Ordering::SeqCst);
            })
        };

        std::thread::sleep(Duration::from_millis(80));
        assert!(
            !claimed.load(Ordering::SeqCst),
            "the claim must wait for the read in progress"
        );

        drop(read);
        claimer.join().expect("claimer");
        assert!(claimed.load(Ordering::SeqCst));
    }

    #[test]
    fn release_is_idempotent() {
        let gate = ReadGate::default();
        gate.claim();
        gate.release();
        gate.release();
        drop(gate.enter_pane_read());
    }
}

#[cfg(test)]
mod handoff_timing_tests {
    use super::ReadGate;
    use std::time::{Duration, Instant};

    #[test]
    fn a_claim_on_an_idle_gate_is_immediate() {
        // The reported hang: the picker closes and `sz` does not start for
        // seventeen seconds because the claim waits behind a pane that is
        // parked in a poll holding the gate.
        let gate = ReadGate::default();
        let started = Instant::now();
        gate.claim();
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "claiming an idle gate took {:?}",
            started.elapsed()
        );
        gate.release();
    }

    #[test]
    fn a_claim_waits_only_for_a_read_in_flight() {
        // A bounded wait is fine — a chunk must not be split — but it must be
        // tied to the read, not to any poll the pane happens to be sitting in.
        let gate = std::sync::Arc::new(ReadGate::default());
        let read = gate.enter_pane_read();

        let started = Instant::now();
        let claimer = {
            let gate = std::sync::Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.claim();
                started.elapsed()
            })
        };

        std::thread::sleep(Duration::from_millis(20));
        drop(read);
        let waited = claimer.join().expect("claimer");
        assert!(
            waited < Duration::from_millis(500),
            "claim waited {waited:?} for a completed read"
        );
    }
}
