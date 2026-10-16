//! The byte-level plumbing that lets a local `sz`/`rz` child talk to a remote
//! peer through a pane's pty.
//!
//! Almost everything here is lifted from `zssh-rs` (GPL-2.0-or-later, a Rust
//! rewrite of zssh 1.5c by Matthieu Lucotte). The pieces are kept close to
//! their original form on purpose: the socketpair/relay design and the tty
//! mode switch encode measured behaviour that is easy to regress.
//!
//! ## Why the child gets a socketpair, not the pty
//!
//! The obvious design is to give `sz`/`rz` the pty master as stdin/stdout so
//! they talk straight to the remote shell. That works but makes `sz` uploads
//! crawl (~50 KB/s vs ~6 MB/s for the same link with `scp`). The cause is not
//! the pty's throughput — a bare pty round-trip is ~0.06 ms. It is how lrzsz's
//! `sz` behaves when its I/O fd is a tty:
//!
//!   * `sz` runs a `junkcount` heuristic (lsz.c): while streaming it probes the
//!     reverse channel with `rdchk()`; any unexpected byte bumps `junkcount`,
//!     and after 3 it drops from streaming frames (ZCRCG) to stop-and-wait
//!     frames (ZCRCW) that block for a ZACK after every block. On a tty the
//!     line-discipline timing (VTIME) and bursty, buffered delivery make
//!     `rdchk()` fire constantly, so `sz` spends the whole transfer in
//!     stop-and-wait.
//!
//! With a socketpair, `isatty()` is false, none of the tty heuristics engage,
//! and it streams at full speed. The remote side keeps its pty; a relay thread
//! copies bytes between the socketpair and the pty master.
//!
//! ## Why the tty must be byte-transparent
//!
//! ZMODEM payloads are binary. Any line-discipline processing corrupts them:
//! XON/XOFF would stall on a `0x13` data byte, and CR/LF translation would
//! rewrite `\r` and `\n` inside the stream. [`prepare_transfer_tty`] clears
//! exactly those bits and nothing else, and the caller restores the previous
//! termios when the transfer ends.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::sys::termios::{self, SetArg};
use nix::unistd;

/// Borrow an fd as `'static`. Only for the short-lived poll/termios calls
/// below, which never outlive the caller's borrow of the same fd.
fn borrow(fd: RawFd) -> BorrowedFd<'static> {
    unsafe { BorrowedFd::borrow_raw(fd) }
}

fn to_io(e: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

fn revents_readable(pfd: &PollFd) -> bool {
    pfd.revents()
        .map(|r| r.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR))
        .unwrap_or(false)
}

/// Write all of `data` to `fd`, retrying on EINTR and waiting for
/// writability on EAGAIN. Returns the number of bytes accepted.
pub fn write_all(fd: RawFd, mut data: &[u8]) -> io::Result<usize> {
    let total = data.len();
    while !data.is_empty() {
        match unistd::write(borrow(fd), data) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
            Ok(n) => data = &data[n..],
            Err(nix::errno::Errno::EINTR) => {}
            Err(nix::errno::Errno::EAGAIN) => {
                let mut pfd = [PollFd::new(borrow(fd), PollFlags::POLLOUT)];
                let _ = nix::poll::poll(&mut pfd, PollTimeout::from(200u16));
            }
            Err(e) => return Err(to_io(e)),
        }
    }
    Ok(total)
}

/// Create a connected `AF_UNIX` stream socketpair.
///
/// Returns `(relay_end, child_end)`: the relay thread owns `relay_end`;
/// `child_end` is dup'd onto the local `sz`/`rz` child's stdin and stdout.
pub fn make_socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    let ret =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((a, b))
}

/// Prepare a pty fd to carry raw binary ZMODEM: 8-bit clean, no flow control,
/// no byte mangling.
///
/// The relay copies binary data across this pty, so any XON/XOFF
/// interpretation (which would stall on a `0x13` data byte) or CR/LF
/// translation (which would corrupt data) must be off. The baud rate is
/// irrelevant here — `sz`/`rz` read their line speed from the socketpair, not
/// the pty.
pub fn prepare_transfer_tty(fd: RawFd) {
    use nix::sys::termios::{ControlFlags, InputFlags, LocalFlags, OutputFlags};
    let borrowed = borrow(fd);
    if let Ok(mut t) = termios::tcgetattr(borrowed) {
        termios::cfmakeraw(&mut t);
        t.input_flags &= !(InputFlags::IXON
            | InputFlags::IXOFF
            | InputFlags::IXANY
            | InputFlags::ISTRIP
            | InputFlags::INLCR
            | InputFlags::IGNCR
            | InputFlags::ICRNL);
        t.output_flags &= !OutputFlags::OPOST;
        t.local_flags &= !(LocalFlags::ECHO | LocalFlags::ICANON | LocalFlags::ISIG);
        t.control_flags &= !ControlFlags::PARENB;
        t.control_flags &= !ControlFlags::CSIZE;
        t.control_flags |= ControlFlags::CS8;
        let _ = termios::tcsetattr(borrowed, SetArg::TCSANOW, &t);
    }
}

/// Read and discard any pending bytes on `fd` without blocking.
///
/// Used to purge leftover protocol bytes from the pty after a transfer so they
/// do not leak to the terminal as garbage. Temporarily sets `O_NONBLOCK`,
/// drains, then restores the flags.
pub fn drain(fd: RawFd) {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};

    let Ok(old) = fcntl(fd, FcntlArg::F_GETFL) else {
        return;
    };
    let old = OFlag::from_bits_truncate(old);
    if fcntl(fd, FcntlArg::F_SETFL(old | OFlag::O_NONBLOCK)).is_err() {
        return;
    }

    let mut buf = [0u8; 4096];
    // The remote's trailing bytes may still be in flight when draining starts,
    // so retry a few times before giving up. This keeps stray protocol bytes
    // from reaching the terminal without noticeably delaying resume.
    let mut idle_rounds = 0;
    while idle_rounds < 4 {
        match unistd::read(fd, &mut buf) {
            Ok(n) if n > 0 => idle_rounds = 0,
            Ok(_) => break,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::EAGAIN) => {
                idle_rounds += 1;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }

    let _ = fcntl(fd, FcntlArg::F_SETFL(old));
}

/// Bridges the local child's socketpair and the pty master.
///
/// The thread exits when `stop` is set or either end reports EOF/error.
pub struct Relay {
    handle: Option<JoinHandle<()>>,
    /// A dup of the relay end, so the cancel path can inject CAN bytes toward
    /// the remote through the same channel the child's data uses.
    peer: OwnedFd,
}

impl Relay {
    pub fn start(
        relay_end: OwnedFd,
        pty_master_fd: RawFd,
        stop: Arc<AtomicBool>,
    ) -> io::Result<Relay> {
        // Dup so the caller (cancel path) can write CAN bytes without racing
        // ownership with the thread.
        let peer = relay_end.try_clone()?;

        let handle = std::thread::Builder::new()
            .name("zmodem-relay".into())
            .spawn(move || relay_loop(relay_end, pty_master_fd, &stop))?;

        Ok(Relay {
            handle: Some(handle),
            peer,
        })
    }

    /// Fd the cancel path writes CAN bytes to.
    pub fn peer_fd(&self) -> RawFd {
        self.peer.as_raw_fd()
    }

    /// Stop the thread and wait for it.
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn relay_loop(relay_end: OwnedFd, pty_master_fd: RawFd, stop: &AtomicBool) {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};

    let sock = relay_end.as_raw_fd();
    // Non-blocking on both ends so a full write buffer or empty read never
    // wedges the loop.
    for fd in [sock, pty_master_fd] {
        if let Ok(flags) = fcntl(fd, FcntlArg::F_GETFL) {
            let flags = OFlag::from_bits_truncate(flags);
            let _ = fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK));
        }
    }

    let mut buf = [0u8; 64 * 1024];
    while !stop.load(Ordering::SeqCst) {
        let mut fds = [
            PollFd::new(borrow(sock), PollFlags::POLLIN),
            PollFd::new(borrow(pty_master_fd), PollFlags::POLLIN),
        ];
        // Short timeout so `stop` is noticed promptly even with no traffic.
        match nix::poll::poll(&mut fds, PollTimeout::from(200u16)) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }

        // socketpair -> pty master (what the local child sends to the remote)
        if revents_readable(&fds[0]) {
            match relay_copy(sock, pty_master_fd, &mut buf) {
                CopyResult::Eof | CopyResult::Error => break,
                CopyResult::Ok => {}
            }
        }
        // pty master -> socketpair (what the remote sends to the local child)
        if revents_readable(&fds[1]) {
            match relay_copy(pty_master_fd, sock, &mut buf) {
                CopyResult::Eof | CopyResult::Error => break,
                CopyResult::Ok => {}
            }
        }
    }
}

enum CopyResult {
    Ok,
    Eof,
    Error,
}

/// Read once from `from` and write it all to `to`, handling non-blocking
/// EAGAIN and partial writes. Returns `Eof` when `from` is closed.
fn relay_copy(from: RawFd, to: RawFd, buf: &mut [u8]) -> CopyResult {
    match unistd::read(from, buf) {
        Ok(0) => CopyResult::Eof,
        Ok(n) => {
            let mut data = &buf[..n];
            while !data.is_empty() {
                match unistd::write(borrow(to), data) {
                    Ok(0) => return CopyResult::Error,
                    Ok(w) => data = &data[w..],
                    Err(nix::errno::Errno::EINTR) => {}
                    Err(nix::errno::Errno::EAGAIN) => {
                        // Destination buffer full; wait for writability.
                        let mut pfd = [PollFd::new(borrow(to), PollFlags::POLLOUT)];
                        let _ = nix::poll::poll(&mut pfd, PollTimeout::from(200u16));
                    }
                    Err(_) => return CopyResult::Error,
                }
            }
            CopyResult::Ok
        }
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => CopyResult::Ok,
        Err(_) => CopyResult::Error,
    }
}

/// The ZMODEM abort sequence: a run of CAN (`0x18`) bytes.
///
/// lrzsz aborts on 5 consecutive CANs; send plenty to be sure. Written to the
/// relay's peer fd, it travels to the pty master and on to the remote, the
/// same path the child's data took.
pub const CANCEL_SEQUENCE: [u8; 32] = [0x18u8; 32];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socketpair_is_connected_both_ways() {
        let (a, b) = make_socketpair().expect("socketpair");
        write_all(a.as_raw_fd(), b"hello").expect("write a");
        let mut buf = [0u8; 16];
        let n = unistd::read(b.as_raw_fd(), &mut buf).expect("read b");
        assert_eq!(&buf[..n], b"hello");

        write_all(b.as_raw_fd(), b"world").expect("write b");
        let n = unistd::read(a.as_raw_fd(), &mut buf).expect("read a");
        assert_eq!(&buf[..n], b"world");
    }

    #[test]
    fn relay_carries_bytes_in_both_directions() {
        // Stand in for the pty master with one end of a second socketpair, so
        // the test does not need a real pty.
        let (pty_side, test_side) = make_socketpair().expect("pty stand-in");
        let (relay_end, child_end) = make_socketpair().expect("child pair");

        let stop = Arc::new(AtomicBool::new(false));
        let relay =
            Relay::start(relay_end, pty_side.as_raw_fd(), stop.clone()).expect("relay start");

        // child -> "pty"
        write_all(child_end.as_raw_fd(), b"upload").expect("child write");
        let mut buf = [0u8; 32];
        let n = unistd::read(test_side.as_raw_fd(), &mut buf).expect("pty read");
        assert_eq!(&buf[..n], b"upload");

        // "pty" -> child
        write_all(test_side.as_raw_fd(), b"download").expect("pty write");
        let n = unistd::read(child_end.as_raw_fd(), &mut buf).expect("child read");
        assert_eq!(&buf[..n], b"download");

        stop.store(true, Ordering::SeqCst);
        relay.join();
    }

    #[test]
    fn cancel_sequence_is_can_bytes() {
        assert!(CANCEL_SEQUENCE.iter().all(|b| *b == 0x18));
        assert!(CANCEL_SEQUENCE.len() >= 5, "lrzsz aborts after 5 CANs");
    }

    #[test]
    fn write_all_handles_a_large_payload() {
        let (a, b) = make_socketpair().expect("socketpair");
        let payload = vec![0xA5u8; 512 * 1024];
        let writer = {
            let payload = payload.clone();
            std::thread::spawn(move || write_all(a.as_raw_fd(), &payload).expect("write"))
        };

        let mut got = Vec::with_capacity(payload.len());
        let mut buf = [0u8; 64 * 1024];
        while got.len() < payload.len() {
            match unistd::read(b.as_raw_fd(), &mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => panic!("read failed: {:?}", e),
            }
        }
        assert_eq!(got.len(), payload.len());
        assert_eq!(got, payload);
        writer.join().expect("writer thread");
    }
}
