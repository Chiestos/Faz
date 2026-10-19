//! ZMODEM file transfer over an interactive shell session.
//!
//! Faz does not speak ZMODEM itself: the protocol is implemented by `lrzsz`'s
//! `sz` and `rz` binaries, which run as child processes. What this module owns
//! is the plumbing that lets those children talk to a remote shell through a
//! pane's pty:
//!
//!   * [`detect`] watches pty output for the start of a ZMODEM header so a
//!     transfer the remote initiated can be picked up automatically.
//!   * [`transfer`] bridges the child's stdio to the pty while the pty is in
//!     a byte-transparent mode, bypassing the VT parser that would otherwise
//!     corrupt binary payloads.
//!
//! [`state`] is the shared cell between the parser thread, the transfer
//! thread, and the UI.
//!
//! See the transfer module for why the child gets a socketpair rather than the
//! pty directly.

pub mod detect;
pub mod progress;
pub mod runner;
pub mod session_transfer;
pub mod state;
pub mod transfer;

pub use detect::{Detector, ScanResult};
pub use runner::{missing_lrzsz, run_transfer, StartError};
pub use session_transfer::{
    local_command, lrzsz_available, program_available, Direction, LocalCommand, Outcome, Transfer,
};
