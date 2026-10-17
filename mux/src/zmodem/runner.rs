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

    // The pane may have started buffering before this transfer existed, in
    // which case `begin` carries those bytes over. Drain them into the child
    // once the sink is live, or the remote's opening frames are lost.
    let transfer = Transfer::start(direction, pty_fd, command).on_sink_ready({
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
            let cancel = state.begin(owned);
            let _ = cancel;

            let buffered = state.buffered();
            if !buffered.is_empty() {
                match crate::zmodem::transfer::write_all(sink, &buffered) {
                    Ok(_) => log::info!(
                        "zmodem: replayed {} buffered byte(s) into the transfer",
                        buffered.len()
                    ),
                    Err(err) => {
                        log::warn!("zmodem: replaying buffered bytes failed: {err:#}");
                    }
                }
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
        assert_eq!(cmd.args, vec!["/tmp/a.bin"]);
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
