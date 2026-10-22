//! Driving a local `sz`/`rz` child against a pane's pty for one transfer.
//!
//! This is the Faz-side adaptation of zssh's `run_hooked` + `supervise_transfer`.
//! The two things that differ from zssh:
//!
//!   * zssh owns the pty, so it can hand `master`/`slave` fds straight to the
//!     relaying code. Here the caller passes in the pane's pty master fd and
//!     the transfer stores it.
//!   * zssh cancels with a process-wide `SIGINT` handler that stores into a
//!     static. Faz has its own input path, so cancellation is an explicit
//!     call to [`Transfer::cancel`] instead of a global handler. The CAN burst
//!     that tells the remote to abort is the same.
//!
//! The child is spawned with `spawn_child`, not `Command::spawn`, so a test can
//! substitute a program that behaves like `sz`/`rz` without installing lrzsz.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::transfer::{make_pipe, prepare_transfer_tty, write_all, Relay};

/// Which direction a transfer runs in, which decides the local child.
///
/// `Hash` so callers can key per-pane state on (pane, direction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Local -> remote. Runs `sz <paths>`; the remote must be running `rz`.
    Upload,
    /// Remote -> local. Runs `rz`; the remote must be running `sz`.
    Download,
}

impl Direction {
    /// The lrzsz program that implements this direction locally.
    pub fn local_program(self) -> &'static str {
        match self {
            Direction::Upload => "sz",
            Direction::Download => "rz",
        }
    }

    /// The command the remote shell needs for this direction.
    pub fn remote_program(self) -> &'static str {
        match self {
            Direction::Upload => "rz",
            Direction::Download => "sz",
        }
    }
}

/// How long the relay keeps forwarding after the child exits.
///
/// Covers the child's final frame travelling through the socketpair, the relay,
/// and the pty's output buffer. Long enough for a slow link to accept the last
/// block; short enough that a dead peer does not pin the thread.
pub const RELAY_GRACE: Duration = Duration::from_secs(1);

/// How long the transfer may go without the relay moving a byte before it is
/// declared stuck.
///
/// This is a stall detector, not a duration limit. Bounding the total time
/// truncates any transfer larger than the bound allows: a 46 MB file at a few
/// hundred kilobytes per second takes minutes, and killing it at thirty seconds
/// looks exactly like a hang. What actually indicates a broken transfer is that
/// nothing is moving in either direction.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Where to run the local child, and with what arguments.
#[derive(Debug, Clone)]
pub struct LocalCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Working directory for the child. Uploads resolve relative paths here.
    pub cwd: Option<std::path::PathBuf>,
}

impl LocalCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(it.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

/// Build the local command for a direction.
///
/// `paths` are the local files for an upload or the destination path for a
/// download.
pub fn local_command(
    direction: Direction,
    paths: &[String],
    cwd: Option<&std::path::Path>,
) -> LocalCommand {
    // \`sz -e\` escapes every control character in the payload.
    //
    // Without it, lrzsz only escapes the bytes ZMODEM itself needs. Everything
    // else travels raw, and this channel is not a plain wire: the bytes are
    // read by an ssh client sitting on the pty, which scans its input for local
    // escape sequences. A payload containing \`\n~v\`, \`~.\` or a bare interrupt
    // is acted on by ssh instead of being forwarded — the session raises its log
    // level, or drops, and the transfer dies with the receiver asking for
    // blocks it never got. Small files often survive because they may not
    // contain the sequence; a large one eventually does.
    // Spawn the resolved path rather than the bare name: the app's PATH does
    // not include Homebrew, so a bare "sz" fails to exec even though the
    // availability check just succeeded.
    let program = resolve_program(direction.local_program())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| direction.local_program().to_string());
    let mut cmd = LocalCommand::new(program);
    if direction == Direction::Upload {
        cmd = cmd.arg("-e");
    }
    let cmd = cmd.args(paths.iter().cloned());
    match cwd {
        Some(dir) => cmd.cwd(dir),
        None => cmd,
    }
}

/// Result of a completed transfer.
#[derive(Debug)]
pub enum Outcome {
    /// The child exited on its own.
    Finished(ExitStatus),
    /// [`Transfer::cancel`] was called; the CAN burst was sent and the child
    /// reaped.
    Cancelled,
    /// The child could not be waited for within [`CHILD_TIMEOUT`].
    TimedOut,
    /// The child could not be spawned.
    SpawnFailed(io::Error),
}

/// How a transfer ended, for reporting back to the UI.
impl Outcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, Outcome::Finished(s) if s.success())
    }
}

/// One in-flight transfer.
///
/// Lifecycle: [`Transfer::start`] takes the pty master fd and prepares it for
/// binary data, [`Transfer::run`] drives the transfer to completion and
/// restores the previous termios, and `Drop` restores termios if `run` was
/// never reached (for example because the caller bailed out).
pub struct Transfer {
    direction: Direction,
    pty_master_fd: RawFd,
    /// Saved termios of the pty master, captured before switching modes.
    saved_termios: Option<nix::sys::termios::Termios>,
    /// Saved termios of the pty slave, if the caller has one. Restoring this
    /// matters: it governs the remote shell's line discipline once the
    /// transfer resumes, and a stale raw mode shows up as staircase newlines.
    saved_slave_termios: Option<nix::sys::termios::Termios>,
    pty_slave_fd: Option<RawFd>,
    command: LocalCommand,
    stop: Arc<AtomicBool>,
    cancel_requested: Arc<AtomicBool>,
    /// The relay's peer fd, kept so `cancel` can inject CAN bytes.
    cancel_fd: Option<RawFd>,
    child: Option<Child>,
    restored: bool,
    /// Injection point for tests: how the local child is spawned.
    spawner: Spawner,
    /// Called once the transfer's socketpair exists, with the fd the pane's
    /// output parser should write remote bytes to. The transfer thread invokes
    /// it before spawning the child so no early bytes are lost.
    ///
    /// In Faz this installs the fd into the pane's [`ZmodemState`], which is
    /// what makes the parser divert output here instead of feeding it to the
    /// VT parser.
    ///
    /// Returns the flag [`ZmodemState::cancel`] sets, so the transfer polls the
    /// same object the UI writes. Returning `None` keeps the private flag, for
    /// callers with no external canceller.
    on_sink_ready: Option<Box<dyn FnOnce(RawFd) -> Option<Arc<AtomicBool>> + Send>>,
    /// Receives the child's progress text, already stripped of lrzsz's banner.
    /// The caller decides where it appears; a no-op means "do not show it".
    progress_sink: Box<dyn FnMut(&[u8]) + Send>,
    /// Stops the progress reader thread.
    progress_stop: Arc<AtomicBool>,
    /// The pane's transfer state, so the relay can take the pty's read side
    /// for the duration. `None` only in tests that drive a bare transfer.
    state: Option<Arc<crate::zmodem::state::ZmodemState>>,
    /// Live count of bytes the relay has forwarded, for stall detection.
    forwarded: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// The pty slave, kept open for the transfer's lifetime so its termios
    /// stays under our control. Dropping it would let the shell's settings
    /// come back mid-session.
    slave_keepalive: Option<std::os::fd::OwnedFd>,
    /// Write end of the child's input pipe.
    ///
    /// Held here so it is closed with the transfer rather than when `run`
    /// returns to its caller: the pane's parser holds the same fd through the
    /// sink, and closing our copy is what lets a half-open sink fail cleanly
    /// instead of accepting writes nobody will read.
    child_stdin: Option<std::os::fd::OwnedFd>,
}

/// How to launch the local child. Production uses [`Spawner::Real`]; tests
/// substitute a program that behaves like `sz`/`rz`, to exercise the plumbing
/// without lrzsz or a remote peer.
pub enum Spawner {
    Real,
    /// Run this program instead of the lrzsz binary.
    Substitute(LocalCommand),
}

impl Spawner {
    fn spawn(
        &self,
        command: &LocalCommand,
        stdin_fd: RawFd,
        stdout_fd: RawFd,
        stderr_fd: Option<RawFd>,
    ) -> io::Result<Child> {
        match self {
            Spawner::Real => spawn_command(command, stdin_fd, stdout_fd, stderr_fd),
            Spawner::Substitute(sub) => spawn_command(sub, stdin_fd, stdout_fd, stderr_fd),
        }
    }
}

impl std::fmt::Debug for Spawner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Spawner::Real => write!(f, "Spawner::Real"),
            Spawner::Substitute(c) => write!(f, "Spawner::Substitute({})", c.program),
        }
    }
}

impl Transfer {
    /// Prepare a transfer over `pty_master_fd`.
    ///
    /// Switches the pty into byte-transparent mode immediately; call
    /// [`Transfer::restore`] or drop the value to put it back.
    pub fn start(direction: Direction, pty_master_fd: RawFd, command: LocalCommand) -> Transfer {
        Transfer::start_with(direction, pty_master_fd, None, command, Spawner::Real)
    }

    /// As [`Transfer::start`], but also manages the pty slave's line
    /// discipline. Passing the slave lets the transfer restore it afterwards,
    /// which is what keeps remote shell output intact after a session.
    pub fn start_with_slave(
        direction: Direction,
        pty_master_fd: RawFd,
        pty_slave_fd: RawFd,
        command: LocalCommand,
    ) -> Transfer {
        Transfer::start_with(
            direction,
            pty_master_fd,
            Some(pty_slave_fd),
            command,
            Spawner::Real,
        )
    }

    fn start_with(
        direction: Direction,
        pty_master_fd: RawFd,
        pty_slave_fd: Option<RawFd>,
        command: LocalCommand,
        spawner: Spawner,
    ) -> Transfer {
        use nix::sys::termios::tcgetattr;

        let saved_termios =
            tcgetattr(unsafe { std::os::fd::BorrowedFd::borrow_raw(pty_master_fd) }).ok();
        let saved_slave_termios = pty_slave_fd
            .and_then(|fd| tcgetattr(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }).ok());

        prepare_transfer_tty(pty_master_fd);
        if let Some(fd) = pty_slave_fd {
            prepare_transfer_tty(fd);
        }

        Transfer {
            direction,
            pty_master_fd,
            saved_termios,
            saved_slave_termios,
            pty_slave_fd,
            command,
            stop: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            cancel_fd: None,
            child: None,
            restored: false,
            spawner,
            on_sink_ready: None,
            progress_sink: Box::new(|_: &[u8]| {}),
            progress_stop: Arc::new(AtomicBool::new(false)),
            state: None,
            forwarded: None,
            slave_keepalive: None,
            child_stdin: None,
        }
    }

    /// Hold `slave` open for the transfer's lifetime.
    ///
    /// The caller opens the pty slave by path to change its termios; keeping
    /// the handle here means it cannot be closed underneath the transfer.
    pub fn on_slave_keepalive(mut self, slave: std::os::fd::OwnedFd) -> Self {
        self.slave_keepalive = Some(slave);
        self
    }

    /// Tell the transfer which pane state to claim the pty's read side from.
    pub fn on_state(mut self, state: Arc<crate::zmodem::state::ZmodemState>) -> Self {
        self.state = Some(state);
        self
    }

    /// Route the child's progress text to `sink`.
    ///
    /// The bytes are lrzsz's own display, with the banner stripped: a counter
    /// line rewritten in place with `\r`. Writing them to a terminal is what
    /// makes the display animate.
    pub fn on_progress(mut self, sink: impl FnMut(&[u8]) + Send + 'static) -> Self {
        self.progress_sink = Box::new(sink);
        self
    }

    /// Install a callback that receives the sink fd once the transfer's
    /// socketpair is ready.
    ///
    /// The fd is the socketpair end the pane's parser must write to. It is
    /// only valid for the duration of the transfer.
    pub fn on_sink_ready(
        mut self,
        cb: impl FnOnce(RawFd) -> Option<Arc<AtomicBool>> + Send + 'static,
    ) -> Self {
        self.on_sink_ready = Some(Box::new(cb));
        self
    }

    /// Test constructor: run `substitute` instead of the lrzsz binary.
    #[cfg(test)]
    fn start_for_test(
        direction: Direction,
        pty_master_fd: RawFd,
        command: LocalCommand,
        substitute: LocalCommand,
    ) -> Transfer {
        Transfer::start_with(
            direction,
            pty_master_fd,
            None,
            command,
            Spawner::Substitute(substitute),
        )
    }

    /// Ask the transfer to stop. Sends the ZMODEM CAN burst toward the remote
    /// so the peer aborts too, rather than continuing to transmit into a dead
    /// session.
    pub fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
        if let Some(fd) = self.cancel_fd {
            let _ = write_all(fd, &super::transfer::cancel_burst());
        }
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Spawn the child, run the relay until it finishes, then restore the tty.
    pub fn run(mut self) -> Outcome {
        // Two one-way pipes, not one socketpair.
        //
        // The child's input and output must not share an endpoint. With a
        // socketpair they do, and the pane's parser — which writes the peer's
        // bytes to the child's stdin — becomes a second writer on the endpoint
        // the relay reads the child's output from. The relay then reads the
        // peer's own bytes back out and writes them toward the peer. After an
        // aborted transfer that is a loop: the remote's progress lines come
        // back to it on its input and the shell runs the numbers as commands.
        let (to_child_read, to_child_write) = match make_pipe() {
            Ok(pair) => pair,
            Err(e) => return Outcome::SpawnFailed(e),
        };
        let (from_child_read, from_child_write) = match make_pipe() {
            Ok(pair) => pair,
            Err(e) => return Outcome::SpawnFailed(e),
        };

        // The pane's parser keeps reading the pty for the whole transfer and
        // hands the peer's bytes to the child through the sink; the relay only
        // carries the child's replies back out. Keeping the two directions on
        // separate threads is what keeps a slow peer from deadlocking the
        // transfer: with a single bidirectional relay, a peer that stops
        // reading fills the channel, the relay parks in `poll(POLLOUT)`, and
        // while it parks it never reads the child's ZACK/ZRPOS — the child
        // waits for an answer that is sitting unread, and the transfer stalls
        // at whatever percentage the buffer filled up at.
        //
        // Do not claim the read gate here. Claiming it would park the parser,
        // and with `RelayMode::ChildToPty` nobody would be reading the pty at
        // all.

        let relay = match Relay::start_with_mode(
            from_child_read,
            self.pty_master_fd,
            self.stop.clone(),
            crate::zmodem::transfer::RelayMode::ChildToPty,
        ) {
            Ok(r) => r,
            Err(e) => return Outcome::SpawnFailed(e),
        };
        self.cancel_fd = Some(relay.peer_fd());
        self.forwarded = Some(Arc::clone(relay.forwarded_handle()));

        // Hand the sink fd to the caller before the child starts, so the
        // pane's parser can begin diverting remote bytes here without a gap.
        // This is the input pipe's write end: the parser supplies the child's
        // stdin and nothing else.
        if let Some(cb) = self.on_sink_ready.take() {
            if let Some(cancel) = cb(to_child_write.as_raw_fd()) {
                self.cancel_requested = cancel;
            }
        }

        // Pick the command and the spawner together: a substitute runs its own
        // program, production runs the lrzsz binary the direction implies.
        let (program, spawner) = match std::mem::replace(&mut self.spawner, Spawner::Real) {
            Spawner::Real => (self.command.clone(), Spawner::Real),
            sub @ Spawner::Substitute(_) => (self.command.clone(), sub),
        };

        // A private pty on the child's stderr so lrzsz prints its progress at
        // all, plus a reader for it. Failure here is not fatal: the transfer
        // works without a progress display.
        let progress_capture = match super::progress::ProgressCapture::open() {
            Ok(c) => Some(c),
            Err(err) => {
                log::warn!("zmodem: no progress display available: {err}");
                None
            }
        };
        let stderr_fd = progress_capture.as_ref().map(|c| c.slave_fd());
        let progress_relay = progress_capture.map(|capture| {
            let stop = self.progress_stop.clone();
            let forward = std::mem::replace(
                &mut self.progress_sink,
                Box::new(|_: &[u8]| {}) as Box<dyn FnMut(&[u8]) + Send>,
            );
            let mut forward = forward;
            // The banner line lrzsz prints before the counter is dropped, so
            // the counter can update in place on one line.
            let mut first = true;
            capture.forward(
                move |bytes| {
                    let chunk = if first {
                        first = false;
                        super::progress::strip_progress_prefix(bytes)
                    } else {
                        bytes
                    };
                    if !chunk.is_empty() {
                        forward(chunk);
                    }
                },
                stop,
            )
        });

        log::info!(
            "zmodem: spawning {:?} args={:?} cwd={:?}",
            program.program,
            program.args,
            program.cwd
        );

        // Hold the pty's settings for the life of the transfer: the remote
        // side re-applies its own terminal modes as the peer switches, and
        // that reaches our pty through the remote client. Without this the
        // binary stream is subject to line discipline again and frames arrive
        // corrupted. The guard also reports whether drift happened at all,
        // which is the difference between "this was the cause" and "this was
        // not".
        let tty_guard = self
            .pty_slave_fd
            .and_then(|fd| crate::zmodem::transfer::guard_transfer_tty(fd, self.stop.clone()))
            .or_else(|| {
                crate::zmodem::transfer::guard_transfer_tty(self.pty_master_fd, self.stop.clone())
            });

        let child = match spawner.spawn(
            &program,
            to_child_read.as_raw_fd(),
            from_child_write.as_raw_fd(),
            stderr_fd,
        ) {
            Ok(c) => c,
            Err(e) => {
                self.stop.store(true, Ordering::SeqCst);
                relay.join();
                self.restore();
                return Outcome::SpawnFailed(e);
            }
        };
        // The child holds its own dups of both pipe ends; drop ours so the
        // output pipe reports EOF once the child exits.
        drop(to_child_read);
        drop(from_child_write);
        // The write end of the child's input must outlive the child being
        // reaped: closing it early would deliver EOF to a child that is still
        // starting up.
        self.child_stdin = Some(to_child_write);
        self.child = Some(child);

        let outcome = self.supervise();

        if self.is_cancelled() {
            // Ordered teardown for a cancel. Each step exists because skipping
            // it left bytes on some buffer that later reached the remote shell
            // as input — the daisy-chain of device-attribute replies typed into
            // the user's command line after every interrupted download.
            //
            // 1. Stop the child's output before anything else. The relay is
            //    the only path from the child to the peer; halting it here
            //    means no further child bytes can be forwarded.
            self.stop.store(true, Ordering::SeqCst);

            // 2. Tell the peer to stop. This goes out on the pty, the channel
            //    the payload used, because the child's pipe leads nowhere now.
            match crate::zmodem::transfer::write_all(
                self.pty_master_fd,
                &crate::zmodem::transfer::cancel_burst(),
            ) {
                Ok(_) => log::info!("zmodem: sent abort burst to the peer after {:?}", outcome),
                Err(err) => log::warn!("zmodem: could not abort the peer: {err:#}"),
            }

            // 3. Keep consuming until the peer has actually gone quiet. A
            //    cancelled sz keeps draining its socket buffers for as long as
            //    the link lets it, and everything that arrives after we resume
            //    normal parsing is rendered as terminal output.
            crate::zmodem::transfer::drain_until_quiet(self.pty_master_fd);

            // 4. Drop what is still queued toward the peer. The pty's output
            //    buffer holds bytes the ssh client has not read yet; left in
            //    place, the next thing the remote shell sees is that debris.
            crate::zmodem::transfer::discard_pending_output(self.pty_master_fd);
        } else {
            // Let the relay run a little longer after a clean exit.
            //
            // `sz` writes its terminating ZFIN just before exiting, and the
            // remote `rz` will not finish until it sees that frame. Stopping
            // the relay the instant the child is reaped races that write: the
            // frame can still be in the pipe, or in the pty's buffer, or in
            // flight to the peer. A short grace period lets it through.
            std::thread::sleep(RELAY_GRACE);
            self.stop.store(true, Ordering::SeqCst);
            crate::zmodem::transfer::drain_until_quiet(self.pty_master_fd);
        }

        relay.join();
        // The guard notices `stop` on its next tick and reports whether the
        // pty's settings drifted underneath us.
        if let Some(g) = tty_guard {
            let _ = g.join();
        }
        // Stop the progress thread: its last read happens under the stop flag.
        self.progress_stop.store(true, Ordering::SeqCst);
        if let Some(p) = progress_relay {
            p.join();
        }
        self.cancel_fd = None;

        // Tell the peer to stop if this ended any way other than cleanly —
        // including a user-requested cancel. The child is gone either way, so
        // nothing else will send an abort; the remote `rz`/`sz` would sit
        // waiting until its own timeout, which is what leaves a pane that looks
        // hung and a remote process that outlives the transfer.
        //
        // `cancel_fd` cannot be used for this: it is the pipe end the child
        // read from, so bytes written there went to the now-dead child rather
        // than to the peer. Write to the pty directly, which is the path every
        // byte the child sent took.
        if !outcome.succeeded() && !self.is_cancelled() {
            match crate::zmodem::transfer::write_all(
                self.pty_master_fd,
                &crate::zmodem::transfer::cancel_burst(),
            ) {
                Ok(_) => log::info!("zmodem: sent abort burst to the peer after {:?}", outcome),
                Err(err) => log::warn!("zmodem: could not abort the peer: {err:#}"),
            }
        }

        // Releases the read gate defensively. The relay no longer claims it —
        // the parser keeps reading the pty throughout — but an earlier phase
        // or a future caller may have, and release is idempotent.
        if let Some(state) = self.state.as_ref() {
            state.set_relay_owns_pty(false);
        }

        self.restore();
        outcome
    }

    fn supervise(&mut self) -> Outcome {
        // Clone the flag up front: `self.child` is borrowed mutably below, so
        // the loop cannot call `self.is_cancelled()`.
        let cancel_requested = self.cancel_requested.clone();
        // Cloned before the child is borrowed mutably below: the loop needs
        // both, and they live in the same struct.
        let forwarded = self.forwarded.clone();
        let peer_state = self.state.clone();
        let Some(child) = self.child.as_mut() else {
            return Outcome::TimedOut;
        };

        let mut last_progress = Instant::now();
        let mut last_bytes = 0u64;
        let forwarded_now = || {
            forwarded
                .as_ref()
                .map_or(0, |f| f.load(std::sync::atomic::Ordering::Relaxed))
        };
        let mut last_progress_bytes = forwarded_now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if cancel_requested.load(Ordering::SeqCst) {
                        return Outcome::Cancelled;
                    }
                    log::info!(
                        "zmodem: child exited with {status:?} (code {:?})",
                        status.code()
                    );
                    return Outcome::Finished(status);
                }
                Ok(None) => {}
                Err(e) => return Outcome::SpawnFailed(e),
            }

            if cancel_requested.load(Ordering::SeqCst) {
                // Cancel already sent CAN; make the child stop writing, then
                // reap it. SIGKILL is what lrzsz needs: it traps SIGTERM to
                // send its own abort, which would race our CAN burst.
                let _ = child.kill();
                let _ = child.wait();
                return Outcome::Cancelled;
            }

            // Progress is measured across both directions: the child writing
            // or the peer answering. Either one moving means the transfer is
            // alive, however long it takes overall.
            let moved = peer_state.as_ref().map_or(0, |s| s.peer_bytes_to_child());
            let fwd = forwarded_now();
            if moved != last_bytes || fwd != last_progress_bytes {
                last_bytes = moved;
                last_progress_bytes = fwd;
                last_progress = Instant::now();
            }
            if last_progress.elapsed() >= STALL_TIMEOUT {
                log::warn!(
                    "zmodem: no progress for {:?}; giving up on the transfer",
                    STALL_TIMEOUT
                );
                let _ = child.kill();
                let _ = child.wait();
                return Outcome::TimedOut;
            }

            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Put the pty line disciplines back. Idempotent.
    fn restore(&mut self) {
        use nix::sys::termios::{tcsetattr, SetArg};

        if self.restored {
            return;
        }
        self.restored = true;

        if let Some(t) = self.saved_termios.take() {
            let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.pty_master_fd) };
            let _ = tcsetattr(fd, SetArg::TCSANOW, &t);
        }
        if let (Some(t), Some(fd)) = (self.saved_slave_termios.take(), self.pty_slave_fd) {
            let bfd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            let _ = tcsetattr(bfd, SetArg::TCSANOW, &t);
        }
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        // A caller that bails out before `run` must not leave the pty raw.
        if self.child.is_none() {
            self.restore();
        }
        // Every exit path releases the read gate, including the early returns
        // that skip the tail of `run`. The relay no longer claims it, so this
        // is a defensive no-op in the normal flow; it still matters for any
        // path that did claim it, which would otherwise park the pane's
        // roundtrip thread forever.
        if let Some(state) = self.state.as_ref() {
            state.set_relay_owns_pty(false);
        }
    }
}

fn spawn_command(
    command: &LocalCommand,
    stdin_fd: RawFd,
    stdout_fd: RawFd,
    stderr_fd: Option<RawFd>,
) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args);
    if let Some(dir) = &command.cwd {
        cmd.current_dir(dir);
    }
    unsafe {
        cmd.pre_exec(move || {
            // stdin and stdout are different pipes on purpose: see the
            // comment in `run`. Sharing one endpoint lets the pane's parser
            // and the relay read each other's traffic.
            if libc::dup2(stdin_fd, 0) < 0 || libc::dup2(stdout_fd, 1) < 0 {
                return Err(io::Error::last_os_error());
            }
            // stderr carries lrzsz's progress display. A private pty keeps it
            // a terminal so the display is produced at all, and gives us a
            // reader for it.
            if let Some(fd) = stderr_fd {
                if libc::dup2(fd, 2) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            // Ignore the tty's SIGINT in the child. Its life is controlled by
            // our cancel path, so we can reliably send the CAN sequence first.
            if libc::signal(libc::SIGINT, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// Is `program` runnable from `PATH`?
///
/// Used by `faz doctor` and by the menu path to report a missing `lrzsz`
/// rather than failing deep inside a transfer.
pub fn program_available(program: &str) -> bool {
    resolve_program(program).is_some()
}

/// Locate `program`, preferring PATH but falling back to the usual install
/// locations.
///
/// A GUI app launched from Finder or the Dock does not inherit the shell's
/// PATH; on a stock Mac it sees only `/usr/bin:/bin:/usr/sbin:/sbin`. Anything
/// installed with Homebrew is therefore invisible to it, so `sz` and `rz`
/// would be reported missing from an app bundle while working fine in a
/// terminal on the same machine. zssh never meets this because it is launched
/// from a shell and inherits the full PATH.
pub fn resolve_program(program: &str) -> Option<PathBuf> {
    let from_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| config::is_executable_file(candidate))
    });
    if from_path.is_some() {
        return from_path;
    }

    // Same fallbacks the CLI uses when resolving a shell or editor.
    ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| config::is_executable_file(candidate))
}

/// Are the lrzsz programs present locally?
pub fn lrzsz_available() -> (bool, bool) {
    (program_available("sz"), program_available("rz"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::transfer::{make_socketpair, CANCEL_SEQUENCE};
    use nix::unistd;
    use std::os::fd::OwnedFd;

    /// A socketpair stands in for the pty master, so tests need no real pty.
    fn fake_pty() -> (OwnedFd, OwnedFd) {
        make_socketpair().expect("pty stand-in")
    }

    /// Build a substitute child that copies its stdin to its stdout until EOF,
    /// which is enough to exercise the relay and the lifecycle.
    fn echoing_child() -> LocalCommand {
        // `cat` is the simplest such program and is always present.
        LocalCommand::new("cat")
    }

    #[test]
    fn direction_picks_the_right_programs() {
        assert_eq!(Direction::Upload.local_program(), "sz");
        assert_eq!(Direction::Upload.remote_program(), "rz");
        assert_eq!(Direction::Download.local_program(), "rz");
        assert_eq!(Direction::Download.remote_program(), "sz");
    }

    #[test]
    fn local_command_carries_paths_and_cwd() {
        let cmd = local_command(
            Direction::Upload,
            &["a.txt".to_string(), "b.txt".to_string()],
            Some(std::path::Path::new("/tmp")),
        );
        assert!(
            cmd.program.ends_with("/sz"),
            "expected a resolved sz path, got {:?}",
            cmd.program
        );
        assert_eq!(cmd.args, vec!["-e", "a.txt", "b.txt"]);
        assert_eq!(cmd.cwd.as_deref(), Some(std::path::Path::new("/tmp")));
    }

    #[test]
    fn spawn_failure_is_reported_not_panicked() {
        let (pty_master, _remote) = fake_pty();
        let transfer = Transfer::start_for_test(
            Direction::Upload,
            pty_master.as_raw_fd(),
            LocalCommand::new("sz"),
            LocalCommand::new("/nonexistent/program/that/does/not/exist"),
        );
        let outcome = transfer.run();
        assert!(
            matches!(outcome, Outcome::SpawnFailed(_)),
            "expected SpawnFailed, got {:?}",
            outcome
        );
    }

    #[test]
    fn child_that_exits_immediately_finishes() {
        let (pty_master, _remote) = fake_pty();
        let transfer = Transfer::start_for_test(
            Direction::Upload,
            pty_master.as_raw_fd(),
            LocalCommand::new("sz"),
            LocalCommand::new("true"),
        );
        let outcome = transfer.run();
        assert!(outcome.succeeded(), "expected success, got {:?}", outcome);
    }

    #[test]
    fn failing_child_is_not_a_success() {
        let (pty_master, _remote) = fake_pty();
        let transfer = Transfer::start_for_test(
            Direction::Upload,
            pty_master.as_raw_fd(),
            LocalCommand::new("sz"),
            LocalCommand::new("false"),
        );
        let outcome = transfer.run();
        assert!(!outcome.succeeded(), "expected failure, got {:?}", outcome);
        assert!(matches!(outcome, Outcome::Finished(_)));
    }

    #[test]
    fn sink_ready_callback_receives_a_working_fd() {
        use std::sync::mpsc;
        let (pty_master, remote_side) = fake_pty();
        let (tx, rx) = mpsc::channel();

        let mut sub = LocalCommand::new("sh");
        sub.args = vec!["-c".into(), "printf from-transfer; exit 0".into()];

        let transfer = Transfer::start_for_test(
            Direction::Upload,
            pty_master.as_raw_fd(),
            LocalCommand::new("sz"),
            sub,
        )
        .on_sink_ready(move |fd| {
            // Prove the fd is usable: write through it, then hand it to the
            // test thread to read the child's reply.
            tx.send(fd).expect("send fd");
            None
        });

        let outcome = transfer.run();
        assert!(outcome.succeeded(), "got {outcome:?}");

        // The callback fired with a real fd. Data written to it reaches the
        // child, whose output the relay forwards to the remote side.
        let _fd = rx.recv().expect("callback ran");
        let mut buf = [0u8; 64];
        let mut got = Vec::new();
        for _ in 0..50 {
            match nix::unistd::read(remote_side.as_raw_fd(), &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    if got.ends_with(b"from-transfer") {
                        break;
                    }
                }
                Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EINTR) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        assert!(
            got.ends_with(b"from-transfer"),
            "child output did not reach the pty side: {:?}",
            String::from_utf8_lossy(&got)
        );
    }

    #[test]
    fn cancel_before_run_is_recorded() {
        let (pty_master, _remote) = fake_pty();
        let transfer = Transfer::start_for_test(
            Direction::Upload,
            pty_master.as_raw_fd(),
            LocalCommand::new("sz"),
            echoing_child(),
        );
        assert!(!transfer.is_cancelled());
        transfer.cancel();
        assert!(transfer.is_cancelled());
    }

    /// A cancelled transfer must still tell the remote to stop.
    ///
    /// Regression guard: the abort burst used to be skipped whenever the
    /// transfer was cancelled, on the theory that the peer had already been
    /// told. It had not — the kill happens locally — so the remote `sz`/`rz`
    /// kept waiting until its own timeout.
    #[test]
    fn cancel_sends_the_can_burst_to_the_peer() {
        let (pty_master, remote) = fake_pty();
        let transfer = Transfer::start(
            Direction::Download,
            pty_master.as_raw_fd(),
            LocalCommand::new("cat"),
        )
        .on_progress(|_: &[u8]| {});
        transfer.cancel();
        let outcome = transfer.run();
        assert!(matches!(outcome, Outcome::Cancelled), "got {outcome:?}");

        // The CAN burst travels the pty toward the peer. Read it back.
        let mut buf = [0u8; 128];
        let mut got = Vec::new();
        loop {
            match unistd::read(remote.as_raw_fd(), &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    if got.len() >= CANCEL_SEQUENCE.len() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        assert!(
            got.windows(CANCEL_SEQUENCE.len())
                .any(|w| w == CANCEL_SEQUENCE),
            "peer never saw the abort burst; got {got:02x?}"
        );
        // The burst ends with a line-clear so a shell that has already taken
        // the terminal back does not execute the CAN run as a command.
        assert_eq!(got.last(), Some(&0x15), "burst must end with Ctrl+U");
    }

    /// A cancel flag owned by someone else must reach the transfer.
    ///
    /// Regression guard: `run_transfer` used to discard the `Arc` that
    /// `ZmodemState::begin` returned, so `ZmodemState::cancel` set a flag no
    /// one was polling and Ctrl+C during a transfer did nothing.
    #[test]
    fn an_external_cancel_flag_stops_the_transfer() {
        let (pty_master, _remote) = fake_pty();
        let external = Arc::new(AtomicBool::new(false));
        let flag_for_callback = Arc::clone(&external);

        let transfer = Transfer::start(
            Direction::Download,
            pty_master.as_raw_fd(),
            LocalCommand::new("cat"),
        )
        .on_progress(|_: &[u8]| {})
        .on_sink_ready(move |_fd| Some(flag_for_callback));

        // What `ZmodemState::cancel` does from the UI thread.
        external.store(true, Ordering::SeqCst);
        assert!(matches!(transfer.run(), Outcome::Cancelled));
    }

    #[test]
    fn program_available_finds_a_real_binary() {
        // `sh` is on PATH everywhere this test runs.
        assert!(program_available("sh"));
        assert!(!program_available("definitely-not-a-real-program-xyz"));
    }

    #[test]
    fn substitute_child_gets_arguments_and_cwd() {
        // `pwd` with a cwd proves the cwd is applied; the output is ignored.
        let dir = std::env::temp_dir();
        let (pty_master, _remote) = fake_pty();
        let mut sub = LocalCommand::new("sh");
        sub.args = vec!["-c".into(), "exit 0".into()];
        sub.cwd = Some(dir);

        let transfer = Transfer::start_for_test(
            Direction::Upload,
            pty_master.as_raw_fd(),
            LocalCommand::new("sz"),
            sub,
        );
        assert!(transfer.run().succeeded());
    }

    #[test]
    fn stdio_of_substitute_is_the_socketpair() {
        // A child writing a fixed string must reach the "remote" end.
        let (pty_master, remote_side) = fake_pty();
        let mut sub = LocalCommand::new("sh");
        sub.args = vec!["-c".into(), "printf hello; exit 0".into()];

        let transfer = Transfer::start_for_test(
            Direction::Upload,
            pty_master.as_raw_fd(),
            LocalCommand::new("sz"),
            sub,
        );
        let outcome = transfer.run();
        assert!(outcome.succeeded(), "got {:?}", outcome);

        // The relay stops when the child exits; whatever it forwarded is on the
        // remote end. Read non-blocking.
        let mut buf = [0u8; 32];
        let n = nix::unistd::read(remote_side.as_raw_fd(), &mut buf).unwrap_or(0);
        assert_eq!(&buf[..n], b"hello");
    }
}

#[cfg(test)]
mod stall_timeout_tests {
    use super::STALL_TIMEOUT;

    #[test]
    fn the_timeout_is_a_stall_detector_not_a_duration_cap() {
        // Bounding total time truncates large transfers: a 46 MB file over a
        // few hundred KB/s takes minutes, and killing it early is
        // indistinguishable from a hang. The bound applies only to periods with
        // no movement at all.
        assert!(
            STALL_TIMEOUT >= std::time::Duration::from_secs(10),
            "too short to tell a slow link from a dead one"
        );
    }
}

#[cfg(test)]
mod escape_flag_tests {
    use super::{local_command, Direction};

    #[test]
    fn uploads_escape_every_control_byte() {
        // The upload channel is read by an ssh client, which acts on escape
        // sequences in its input. `sz -e` escapes all control characters so no
        // payload byte can be mistaken for one.
        let paths = vec!["file.bin".to_string()];
        let cmd = local_command(Direction::Upload, &paths, None);
        assert!(
            cmd.program.ends_with("/sz"),
            "expected a resolved sz path, got {:?}",
            cmd.program
        );
        assert!(
            cmd.args.contains(&"-e".to_string()),
            "upload must run sz with -e, got {:?}",
            cmd.args
        );
    }

    #[test]
    fn the_path_is_still_passed_after_the_flag() {
        let paths = vec!["a b.bin".to_string()];
        let cmd = local_command(Direction::Upload, &paths, None);
        assert_eq!(cmd.args.last(), Some(&"a b.bin".to_string()));
    }

    #[test]
    fn downloads_are_unaffected() {
        // `rz` has no equivalent flag and needs none: nothing it writes is a
        // payload the peer's environment would interpret.
        let cmd = local_command(Direction::Download, &[], None);
        assert!(
            cmd.program.ends_with("/rz"),
            "expected a resolved rz path, got {:?}",
            cmd.program
        );
        assert!(!cmd.args.iter().any(|a| a == "-e"));
    }
}

#[cfg(test)]
mod program_lookup_tests {
    use super::{program_available, resolve_program};

    #[test]
    fn a_program_on_path_resolves() {
        let found = resolve_program("sh").expect("sh must resolve");
        assert!(
            found.is_absolute(),
            "expected an absolute path, got {found:?}"
        );
        assert!(program_available("sh"));
    }

    #[test]
    fn a_missing_program_does_not_resolve() {
        assert!(resolve_program("definitely-not-a-real-program-xyz").is_none());
        assert!(!program_available("definitely-not-a-real-program-xyz"));
    }

    #[test]
    fn the_fallback_covers_where_package_managers_install() {
        // A GUI app launched from Finder sees only the system PATH, so
        // anything from Homebrew is invisible without a fallback. This pins
        // the intent rather than a specific binary: on a machine without
        // Homebrew there is nothing to find, and the function still returns
        // None instead of panicking.
        let homebrew_sz = std::path::Path::new("/opt/homebrew/bin/sz");
        if homebrew_sz.exists() {
            let resolved = resolve_program("sz").expect("sz should resolve");
            assert!(
                resolved.is_absolute(),
                "expected an absolute path, got {resolved:?}"
            );
            assert!(program_available("sz"));
        }
    }
}
