//! Starting a ZMODEM transfer against a pane.
//!
//! This ties the three participants together:
//!
//!   * the pane's output parser, which must be told where to divert remote
//!     bytes (via [`ZmodemState`]);
//!   * the local `sz`/`rz` child, driven by [`Transfer`];
//!   * the caller, which learns the outcome.
//!
//! ## Why the caller keeps the `Arc<dyn Pane>`
//!
//! The transfer needs the pty's fd, which is owned by the pane's `MasterPty`.
//! If the pane were dropped mid-transfer the fd would dangle. [`run_transfer`]
//! takes the `Arc` and holds it for the transfer's whole lifetime, so the pty
//! outlives every use of its fd.
//!
//! ## Threading
//!
//! `run_transfer` blocks until the transfer ends, which may be minutes. It is
//! meant to be called from a worker thread; the GUI spawns one per transfer.
//! The pane's parser thread keeps running throughout and does the actual byte
//! forwarding, so nothing here needs to read the pty.

use std::os::fd::RawFd;
use std::sync::Arc;

use crate::pane::Pane;
use crate::zmodem::session_transfer::{Direction, LocalCommand, Outcome, Transfer};
use crate::zmodem::state::ZmodemState;

/// Why a transfer could not be started.
#[derive(Debug)]
pub enum StartError {
    /// The pane has no local pty, so there is nothing to transfer over. This
    /// covers client panes (remote mux) and overlay panes.
    NoPty,
    /// A transfer is already running on this pane.
    AlreadyActive,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NoPty => write!(f, "this pane has no local terminal to transfer over"),
            StartError::AlreadyActive => write!(f, "a transfer is already running in this pane"),
        }
    }
}

impl std::error::Error for StartError {}

/// Run one transfer to completion on `pane`.
///
/// Blocks until the child exits, the transfer is cancelled, or it times out.
/// Returns the outcome so the caller can report success or failure.
///
/// The caller is responsible for holding `pane` alive; see the module docs.
pub fn run_transfer(
    pane: &Arc<dyn Pane>,
    state: &Arc<ZmodemState>,
    direction: Direction,
    command: LocalCommand,
) -> Result<Outcome, StartError> {
    if state.is_active() {
        return Err(StartError::AlreadyActive);
    }
    let Some(pty_fd) = pane.zmodem_pty_fd() else {
        return Err(StartError::NoPty);
    };

    // The transfer needs the *slave* to be byte-transparent, not the master:
    // termios is a slave property, and the remote shell's line discipline
    // (ICRNL, IXON, ISTRIP) corrupts an 8-bit payload. Opening the slave by
    // path is how this process gets at it — portable-pty only hands out the
    // master fd.
    let slave = pane.zmodem_pty_slave_path().and_then(|path| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map(std::os::fd::OwnedFd::from)
            .map_err(|err| {
                log::warn!("zmodem: cannot open pty slave {}: {err}", path.display());
                err
            })
            .ok()
    });

    // The pane may have started buffering before this transfer existed, in
    // which case `begin` carries those bytes over. Drain them into the child
    // once the sink is live, or the remote's opening frames are lost.
    // Progress goes into the pane's own terminal state, so it appears where the
    // transfer is happening rather than in a separate window. It is text, not
    // protocol data, so it takes the normal parsing path.
    let progress_pane = Arc::clone(pane);
    let transfer = {
        let t = match slave {
            Some(slave) => {
                use std::os::fd::AsRawFd;
                Transfer::start_with_slave(direction, pty_fd, slave.as_raw_fd(), command)
                    // Keep the slave open for the transfer's lifetime: closing it
                    // would let the pty revert to the shell's termios mid-session.
                    .on_slave_keepalive(slave)
            }
            None => Transfer::start(direction, pty_fd, command),
        };
        // The relay takes the pty's read side, so the pane's roundtrip thread
        // stops reading while the transfer runs.
        t.on_state(Arc::clone(state))
    }
        .on_progress(move |bytes: &[u8]| {
            // lrzsz explains itself only on stderr, and the pane shows the
            // bytes but not the verdict. Keep the diagnostic lines in the log
            // so a failed transfer says *why* instead of just a byte count.
            if let Ok(text) = std::str::from_utf8(bytes) {
                for line in text.lines() {
                    let line = line.trim();
                    if !line.is_empty()
                        && (line.contains("Retry")
                            || line.contains("rror")
                            || line.contains("incomplete")
                            || line.contains("Timeout")
                            || line.contains("ZCAN"))
                    {
                        log::warn!("zmodem: peer said: {line}");
                    }
                }
            }
            let mut parser = termwiz::escape::parser::Parser::new();
            let mut actions = Vec::new();
            parser.parse(bytes, |action| actions.push(action));
            if !actions.is_empty() {
                progress_pane.perform_actions(actions);
            }
        })
        .on_sink_ready({
        // The parser's side of the switch: while this fd is installed, the
        // pane's output goes here instead of to the VT parser.
        let state = Arc::clone(state);
        move |sink| {
            // `begin` takes ownership of the fd, so dup it first: the relay
            // thread keeps the original.
            let owned = match dup_fd(sink) {
                Ok(owned) => owned,
                Err(err) => {
                    log::error!("zmodem: could not install transfer sink: {err}");
                    return;
                }
            };
            let cancel = state.begin(owned, direction);
            let _ = cancel;

            // Report what the parser diverted once the transfer is over. The
            // relay cannot answer this: with the parser owning the pty's read
            // side, its peer-to-child counter is structurally zero.
            {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    while state.is_active() {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    log::info!(
                        "zmodem: parser diverted {} byte(s) from the peer to the local child",
                        state.peer_bytes_to_child()
                    );
                });
            }

            // Do not replay the peer's opening frame.
            //
            // An earlier version handed the buffered ZRINIT/ZRQINIT to the
            // child so the handshake could start from where it left off. That
            // works when the peer is waiting passively, but the peer retransmits
            // its opening frame on a timer: by the time the user has picked a
            // destination the local child would receive a copy of a frame the
            // remote already sent, answer it, and then answer the remote's real
            // one — two handshakes for one session, which the peer aborts.
            //
            // The remote is still waiting on its side; the child re-opens the
            // handshake itself with ZRQINIT, which is the sequence `rz` is
            // already prepared for.
            let buffered = state.buffered();
            if !buffered.is_empty() {
                log::info!(
                    "zmodem: discarding {} buffered peer byte(s); the child opens its own handshake",
                    buffered.len()
                );
            }
        }
    });

    let outcome = transfer.run();
    // Clear the sink so the pane resumes normal parsing. Doing this before
    // returning means the parser is never left diverting into a closed fd.
    state.finish();
    Ok(outcome)
}

/// `dup(2)` a raw fd into an owned one.
fn dup_fd(fd: RawFd) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::BorrowedFd;
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    borrowed.try_clone_to_owned()
}

/// Is a usable `lrzsz` present? Reports both programs so callers can name the
/// one that is missing.
pub fn missing_lrzsz() -> Option<&'static str> {
    match crate::zmodem::lrzsz_available() {
        (true, true) => None,
        (false, true) => Some("sz"),
        (true, false) => Some("rz"),
        (false, false) => Some("sz and rz"),
    }
}

/// Build the command for `direction`, with `paths` as the local files (upload)
/// or the destination directory (download).
pub fn transfer_command(
    direction: Direction,
    paths: &[String],
    cwd: Option<&std::path::Path>,
) -> LocalCommand {
    crate::zmodem::local_command(direction, paths, cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::transfer::make_socketpair;

    #[test]
    fn missing_lrzsz_matches_what_is_installed() {
        let (sz, rz) = crate::zmodem::lrzsz_available();
        let reported = missing_lrzsz();
        match (sz, rz) {
            (true, true) => assert_eq!(reported, None),
            (false, true) => assert_eq!(reported, Some("sz")),
            (true, false) => assert_eq!(reported, Some("rz")),
            (false, false) => assert_eq!(reported, Some("sz and rz")),
        }
    }

    #[test]
    fn dup_fd_produces_an_independent_handle() {
        let (a, b) = make_socketpair().expect("socketpair");
        use std::os::fd::AsRawFd;

        let dup = dup_fd(a.as_raw_fd()).expect("dup");
        // Writing through the dup reaches the same peer.
        crate::zmodem::transfer::write_all(dup.as_raw_fd(), b"via dup").expect("write");
        let mut buf = [0u8; 16];
        let n = nix::unistd::read(b.as_raw_fd(), &mut buf).expect("read");
        assert_eq!(&buf[..n], b"via dup");
    }

    #[test]
    fn transfer_command_for_upload_targets_sz_with_the_paths() {
        let cmd = transfer_command(
            Direction::Upload,
            &["/tmp/a.bin".to_string()],
            None,
        );
        assert_eq!(cmd.program, "sz");
        // `-e` escapes every control byte; without it the ssh client on the
        // pty can mistake payload bytes for its own escape sequences.
        assert_eq!(cmd.args, vec!["-e", "/tmp/a.bin"]);
    }

    #[test]
    fn transfer_command_for_download_targets_rz_in_the_chosen_directory() {
        let dir = std::path::Path::new("/tmp");
        let cmd = transfer_command(Direction::Download, &[], Some(dir));
        assert_eq!(cmd.program, "rz");
        assert!(cmd.args.is_empty());
        assert_eq!(cmd.cwd.as_deref(), Some(dir));
    }

    #[test]
    fn start_errors_have_readable_messages() {
        assert!(StartError::NoPty.to_string().contains("no local terminal"));
        assert!(StartError::AlreadyActive.to_string().contains("already running"));
    }

}
