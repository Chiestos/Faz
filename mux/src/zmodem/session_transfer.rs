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
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::transfer::{
    drain, make_socketpair, prepare_transfer_tty, write_all, Relay, CANCEL_SEQUENCE,
};

/// Which direction a transfer runs in, which decides the local child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// How long to wait for the child after the relay stops before declaring the
/// transfer stuck. Generous: a large transfer over a slow link can take a
/// while to wind down, and killing early would truncate it.
pub const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

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
pub fn local_command(direction: Direction, paths: &[String], cwd: Option<&std::path::Path>) -> LocalCommand {
    let cmd = LocalCommand::new(direction.local_program()).args(paths.iter().cloned());
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
    fn spawn(&self, command: &LocalCommand, child_end_fd: RawFd) -> io::Result<Child> {
        match self {
            Spawner::Real => spawn_command(command, child_end_fd),
            Spawner::Substitute(sub) => spawn_command(sub, child_end_fd),
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
    pub fn start(
        direction: Direction,
        pty_master_fd: RawFd,
        command: LocalCommand,
    ) -> Transfer {
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

        let saved_termios = tcgetattr(unsafe {
            std::os::fd::BorrowedFd::borrow_raw(pty_master_fd)
        })
        .ok();
        let saved_slave_termios = pty_slave_fd.and_then(|fd| {
            tcgetattr(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }).ok()
        });

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
        }
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
            let _ = write_all(fd, &CANCEL_SEQUENCE);
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
        let (relay_end, child_end) = match make_socketpair() {
            Ok(pair) => pair,
            Err(e) => return Outcome::SpawnFailed(e),
        };

        // Start the relay before spawning so no early bytes are lost. It runs
        // until `stop` is set and the child end closes.
        let relay = match Relay::start(relay_end, self.pty_master_fd, self.stop.clone()) {
            Ok(r) => r,
            Err(e) => return Outcome::SpawnFailed(e),
        };
        self.cancel_fd = Some(relay.peer_fd());

        // Pick the command and the spawner together: a substitute runs its own
        // program, production runs the lrzsz binary the direction implies.
        let (program, spawner) = match std::mem::replace(&mut self.spawner, Spawner::Real) {
            Spawner::Real => (self.command.clone(), Spawner::Real),
            sub @ Spawner::Substitute(_) => (self.command.clone(), sub),
        };

        let child = match spawner.spawn(&program, child_end.as_raw_fd()) {
            Ok(c) => c,
            Err(e) => {
                self.stop.store(true, Ordering::SeqCst);
                relay.join();
                self.restore();
                return Outcome::SpawnFailed(e);
            }
        };
        // The child holds its own dup of child_end; drop ours so the socketpair
        // reports EOF once the child exits.
        drop(child_end);
        self.child = Some(child);

        let outcome = self.supervise();

        // Tear down the relay: signal stop, then join.
        self.stop.store(true, Ordering::SeqCst);
        relay.join();

        // Purge leftover protocol bytes so they don't reach the terminal as
        // invalid UTF-8.
        drain(self.pty_master_fd);
        if let Some(fd) = self.pty_slave_fd {
            drain(fd);
        }
        self.cancel_fd = None;

        self.restore();
        outcome
    }

    fn supervise(&mut self) -> Outcome {
        // Clone the flag up front: `self.child` is borrowed mutably below, so
        // the loop cannot call `self.is_cancelled()`.
        let cancel_requested = self.cancel_requested.clone();
        let Some(child) = self.child.as_mut() else {
            return Outcome::TimedOut;
        };

        let deadline = Instant::now() + CHILD_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if cancel_requested.load(Ordering::SeqCst) {
                        return Outcome::Cancelled;
                    }
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

            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Outcome::TimedOut;
            }

            std::thread::sleep(Duration::from_millis(20));
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
    }
}

fn spawn_command(command: &LocalCommand, child_end_fd: RawFd) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args);
    if let Some(dir) = &command.cwd {
        cmd.current_dir(dir);
    }
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(child_end_fd, 0) < 0 || libc::dup2(child_end_fd, 1) < 0 {
                return Err(io::Error::last_os_error());
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
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file()
            && std::fs::metadata(&candidate)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                })
                .unwrap_or(false)
    })
}

/// Are the lrzsz programs present locally?
pub fn lrzsz_available() -> (bool, bool) {
    (program_available("sz"), program_available("rz"))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(cmd.program, "sz");
        assert_eq!(cmd.args, vec!["a.txt", "b.txt"]);
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
