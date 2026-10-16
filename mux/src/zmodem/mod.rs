//! ZMODEM file transfer over an interactive shell session.
//!
//! Faz does not speak ZMODEM itself: the protocol is implemented by `lrzsz`'s
//! `sz` and `rz` binaries, which run as child processes. What this module owns
//! is the plumbing that lets those children talk to a remote shell through a
//! pane's pty:
//!
//!   * [`detect`] watches pty output for the start of a ZMODEM header so a
//!     transfer the remote initiated can be picked up automatically.
//!   * The transfer layer bridges the child's stdio to the pty while the pty
//!     is in a byte-transparent mode, bypassing the VT parser that would
//!     otherwise corrupt binary payloads.
//!
//! See the transfer module for why the child gets a socketpair rather than the
//! pty directly.

pub mod detect;
