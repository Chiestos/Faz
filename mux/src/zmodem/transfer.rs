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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Size of the socketpair buffer between the relay and the local child.
///
/// This is a flow-control knob, not just a performance one, and it has to stay
/// small. Every byte that enters this buffer is already committed to the peer:
/// the relay drains it into the pty as fast as the pty accepts, and the pty
/// drains into the ssh link. A large buffer lets the local child run far ahead
/// of the link, so it streams a megabyte before the peer has confirmed a single
/// block; the peer's line discipline then overruns and frames arrive torn,
/// which shows up as a corrupted filename in ZFILE and a CRC failure.
///
/// zssh uses the 8 KiB default and completes transfers over the same kind of
/// link. Matching it keeps the child paced to the peer.
pub const SOCKETPAIR_BUFFER: usize = 32 * 1024;

/// Create a connected `AF_UNIX` stream socketpair.
///
/// Returns `(relay_end, child_end)`: the relay thread owns `relay_end`;
/// `child_end` is dup'd onto the local `sz`/`rz` child's stdin and stdout.
pub fn make_socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    grow_socket_buffer(&a);
    grow_socket_buffer(&b);
    Ok((a, b))
}

/// Widen a socketpair buffer so a slow reader does not stall the writer.
///
/// Best-effort: the kernel caps the value at `net.local.stream.sendspace`, and
/// a refusal is not a reason to fail the transfer.
fn grow_socket_buffer(fd: &OwnedFd) {
    let size = SOCKETPAIR_BUFFER as libc::c_int;
    for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                &size as *const libc::c_int as *const _,
                std::mem::size_of_val(&size) as libc::socklen_t,
            );
        }
    }
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

        // Clear every special-character slot.
        //
        // `cfmakeraw` does not do this on macOS, and a ZMODEM payload is
        // binary: a byte that happens to equal a control character is acted on
        // rather than delivered. Measured on the pty this code drives, `0x0f`
        // (VDISCARD) and `0x16` (VLNEXT) disappear from a raw burst — enough to
        // strip the leading `*` from a frame header and turn it into a CRC
        // failure the peer reports as `Bad CRC` then `Got ZCAN`.
        //
        // `VMIN`/`VTIME` are not special characters and must stay meaningful:
        // setting them to `_POSIX_VDISABLE` as well makes the read side
        // non-blocking (`VMIN=0`, `VTIME=0`), which silently truncates reads
        // and drops bytes outright.
        t.control_chars = [libc::_POSIX_VDISABLE; nix::sys::termios::NCCS];
        t.control_chars[libc::VMIN] = 1;
        t.control_chars[libc::VTIME] = 0;

        let _ = termios::tcsetattr(borrowed, SetArg::TCSANOW, &t);
    }
}

/// Keep a pty's transfer settings from being changed underneath the relay.
///
/// The peer's terminal state and our local pty are coupled through the remote
/// client: when the far end's `rz` enters raw mode, the client re-applies
/// matching settings to the local pty, overwriting what
/// [`prepare_transfer_tty`] installed. A single setup call is therefore not
/// enough — the first time the peer switches mode mid-handshake, the binary
/// stream is subject to line-discipline processing again and frames arrive
/// corrupted.
///
/// Re-assert the settings until `stop` is set, and report when a drift was
/// actually observed so the log says whether this was happening at all.
pub fn guard_transfer_tty(fd: RawFd, stop: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    let wanted = termios::tcgetattr(borrow(fd)).ok()?;
    // Say exactly what the pty is set to. "Settings stayed put" only proves
    // nothing changed; it does not prove the settings were right, and a pty
    // that alters bytes mid-flight looks identical from the outside.
    log::info!(
        "zmodem: pty termios iflag={:?} oflag={:?} lflag={:?} cflag={:?} vmin={} vtime={} veol={}",
        wanted.input_flags,
        wanted.output_flags,
        wanted.local_flags,
        wanted.control_flags,
        wanted.control_chars[libc::VMIN],
        wanted.control_chars[libc::VTIME],
        wanted.control_chars[libc::VEOL],
    );
    std::thread::Builder::new()
        .name("zmodem-tty-guard".into())
        .spawn(move || {
            let mut drifts = 0u32;
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let Ok(current) = termios::tcgetattr(borrow(fd)) else {
                    break;
                };
                if current.input_flags != wanted.input_flags
                    || current.output_flags != wanted.output_flags
                    || current.local_flags != wanted.local_flags
                    || current.control_chars != wanted.control_chars
                {
                    drifts += 1;
                    let _ = termios::tcsetattr(borrow(fd), SetArg::TCSANOW, &wanted);
                }
            }
            if drifts > 0 {
                log::warn!(
                    "zmodem: pty settings were changed {drifts} time(s) during the transfer; \
                     restored each time"
                );
            } else {
                log::info!("zmodem: pty settings stayed put during the transfer");
            }
        })
        .ok()
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

/// Which directions the relay copies.
///
/// zssh owns its pty and therefore reads it, so its relay is bidirectional.
/// In Faz the pane's output parser already reads the pty and forwards bytes to
/// the transfer's socketpair, so a relay that also read the pty would race the
/// parser for the same bytes. Faz therefore uses [`RelayMode::ChildToPty`] and
/// leaves the pty-to-child direction to the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayMode {
    /// Both directions. Used when nothing else reads the pty.
    Bidirectional,
    /// socketpair -> pty only. The pane's output parser supplies the other
    /// direction, so the relay must not touch the pty's read side.
    ChildToPty,
}

/// Bridges the local child's socketpair and the pty master.
///
/// The thread exits when `stop` is set or the copied direction reports
/// EOF/error.
pub struct Relay {
    handle: Option<JoinHandle<()>>,
    /// A dup of the relay end, so the cancel path can inject CAN bytes toward
    /// the remote through the same channel the child's data uses.
    peer: OwnedFd,
    /// Bytes the child has written that the relay has accepted.
    ///
    /// Shared with the supervisor, which uses movement in either direction to
    /// tell a slow transfer from a stuck one.
    forwarded: Arc<AtomicU64>,
}

impl Relay {
    pub fn start(
        relay_end: OwnedFd,
        pty_master_fd: RawFd,
        stop: Arc<AtomicBool>,
    ) -> io::Result<Relay> {
        Relay::start_with_mode(relay_end, pty_master_fd, stop, RelayMode::Bidirectional)
    }

    /// Start the relay in a specific mode. See [`RelayMode`].
    pub fn start_with_mode(
        relay_end: OwnedFd,
        pty_master_fd: RawFd,
        stop: Arc<AtomicBool>,
        mode: RelayMode,
    ) -> io::Result<Relay> {
        // Dup so the caller (cancel path) can write CAN bytes without racing
        // ownership with the thread.
        let peer = relay_end.try_clone()?;
        let forwarded = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&forwarded);

        let handle = std::thread::Builder::new()
            .name("zmodem-relay".into())
            .spawn(move || relay_loop(relay_end, pty_master_fd, &stop, mode, counter))?;

        Ok(Relay {
            handle: Some(handle),
            peer,
            forwarded,
        })
    }

    /// Fd the cancel path writes CAN bytes to.
    pub fn peer_fd(&self) -> RawFd {
        self.peer.as_raw_fd()
    }

    /// Bytes the child has written that the relay has passed on.
    pub fn forwarded(&self) -> u64 {
        self.forwarded.load(Ordering::Relaxed)
    }

    /// A clone of the live counter, for a supervisor watching for stalls.
    pub fn forwarded_handle(&self) -> &Arc<AtomicU64> {
        &self.forwarded
    }

    /// Stop the thread and wait for it.
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn relay_loop(
    relay_end: OwnedFd,
    pty_master_fd: RawFd,
    stop: &AtomicBool,
    mode: RelayMode,
    forwarded: Arc<AtomicU64>,
) {
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

    let mut child_to_pty: u64 = 0;
    let mut pty_to_child: u64 = 0;
    let mut head = Vec::new();
    let mut back_head = Vec::new();
    // Child output already read but not yet accepted by the pty.
    //
    // Written non-blocking and drained a slice at a time. A blocking write
    // parks this loop until the pty accepts everything, and while it is parked
    // the peer's replies are not read: the receiver asks for a block it is
    // missing, the request sits in the pty, and the child waits for an
    // acknowledgement that will not be read. Small files finish inside one
    // window and never notice; larger ones stop dead.
    let mut pending: Vec<u8> = Vec::new();
    // Frame-type histogram for the peer's replies. A total cannot distinguish
    // acknowledgements from retransmission requests, and the two mean opposite
    // things: one says the data arrived, the other says it did not.
    let mut back_frames: std::collections::BTreeMap<String, u32> = Default::default();
    let mut buf = [0u8; 64 * 1024];
    while !stop.load(Ordering::SeqCst) {
        // Wait for writability too when there is queued output, otherwise a
        // backed-up pty costs a full timeout before the next attempt.
        let pty_events = if pending.is_empty() {
            PollFlags::POLLIN
        } else {
            PollFlags::POLLIN | PollFlags::POLLOUT
        };
        let mut fds = [
            PollFd::new(borrow(sock), PollFlags::POLLIN),
            PollFd::new(borrow(pty_master_fd), pty_events),
        ];
        // Short timeout so `stop` is noticed promptly even with no traffic.
        match nix::poll::poll(&mut fds, PollTimeout::from(200u16)) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }

        // pty master -> socketpair, exactly as zssh orders it.
        //
        // Peer first, matching zssh's coupling: a blocked forward write must
        // not delay an acknowledgement the child is waiting on.
        if mode == RelayMode::Bidirectional && revents_readable(&fds[1]) {
            let result = relay_copy(pty_master_fd, sock, &mut buf, stop);
            let moved = result.bytes();
            match result {
                CopyResult::Eof | CopyResult::Error => break,
                _ => {}
            }
            if moved > 0 {
                count_back_frames(&buf[..moved as usize], &mut back_frames);
            }
            if back_head.len() < 96 && moved > 0 {
                let take = (moved as usize).min(96 - back_head.len());
                back_head.extend_from_slice(&buf[..take]);
            }
            pty_to_child += moved;
        }

        // Drain what is already queued, as much as the pty will take now.
        if !pending.is_empty() {
            let n = write_some(pty_master_fd, &pending);
            pending.drain(..n);
            child_to_pty += n as u64;
            forwarded.fetch_add(n as u64, Ordering::Relaxed);
        }

        // socketpair -> pty master. Read only while the queue has room, so a
        // peer that stops reading pushes back on the child through the
        // socketpair rather than growing memory here.
        // A full queue means "do not read the child". `poll` will keep
        // reporting the socketpair readable, so back off rather than spin.
        if pending.len() >= MAX_PENDING {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        if revents_readable(&fds[0]) && pending.len() < MAX_PENDING {
            // Plain copy, as zssh does. No byte is inspected, held back, or
            // reordered: `sz -e` has already escaped every control byte, so
            // nothing in this stream can be mistaken for a terminal or ssh
            // escape sequence.
            match unistd::read(sock, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = &buf[..n];
                    record_outgoing(&mut head, data);
                    pending.extend_from_slice(data);
                }
                Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {}
                Err(_) => break,
            }
        }
    }

    log::info!(
        "zmodem: relay ended ({mode:?}): child->pty {child_to_pty} byte(s), \
         relay pty->child {pty_to_child} byte(s)\n\
         zmodem: child's opening bytes: {head:02x?}\n\
         zmodem: peer's opening bytes: {back_head:02x?}\n\
         zmodem: peer frames: {back_frames:?}"
    );
}

/// Log what the parser actually diverted, once a transfer has ended.
///
/// Reported separately from the relay's tally because the relay cannot see
/// this direction at all when the parser owns the pty's read side — its number
/// is structurally zero, not evidence about the peer.
pub fn log_diverted_totals(state: &super::state::ZmodemState) {
    log::info!(
        "zmodem: parser diverted {} byte(s) from the peer to the local child",
        state.peer_bytes_to_child()
    );
}

enum CopyResult {
    Ok(usize),
    Eof,
    Error,
}

impl CopyResult {
    /// Bytes moved by this call, for the relay's closing tally.
    fn bytes(&self) -> u64 {
        match self {
            CopyResult::Ok(n) => *n as u64,
            CopyResult::Eof | CopyResult::Error => 0,
        }
    }
}

/// How much child output the relay may hold while the pty is backed up.
///
/// Bounded so a peer that stops reading cannot grow this without limit. Past
/// the cap the relay stops reading the child, which pushes back through the
/// socketpair — the same backpressure a blocking write would give, but without
/// parking the loop that reads the peer's replies.
const MAX_PENDING: usize = 256 * 1024;

/// Write as much of `data` as the destination accepts right now.
///
/// Returns the count taken; a short count means "try again next pass". Never
/// blocks, so an acknowledgement waiting on the other direction is still read
/// promptly.
fn write_some(fd: RawFd, data: &[u8]) -> usize {
    match unistd::write(borrow(fd), data) {
        Ok(n) => n,
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => 0,
        Err(_) => 0,
    }
}

/// Keep the first bytes actually written toward the peer, for the log.
fn record_outgoing(head: &mut Vec<u8>, data: &[u8]) {
    if head.len() < 96 && !data.is_empty() {
        let take = data.len().min(96 - head.len());
        head.extend_from_slice(&data[..take]);
    }
}

/// Tally the ZMODEM frame types in a chunk of the peer's output.
///
/// Hex headers are `**` ZDLE `B` followed by a frame number; binary headers are
/// `*` ZDLE followed by one byte. Both are counted, along with CAN runs, so a
/// log line says whether the peer was acknowledging or asking for a resend.
fn count_back_frames(data: &[u8], counts: &mut std::collections::BTreeMap<String, u32>) {
    let label = |kind: u8| match kind {
        0x00 => "ZRQINIT".to_string(),
        0x01 => "ZRINIT".to_string(),
        0x02 => "ZSINIT".to_string(),
        0x03 => "ZACK".to_string(),
        0x04 => "ZFILE".to_string(),
        0x05 => "ZSKIP".to_string(),
        0x06 => "ZNAK".to_string(),
        0x07 => "ZABORT".to_string(),
        0x08 => "ZFIN".to_string(),
        0x09 => "ZRPOS".to_string(),
        0x0a => "ZDATA".to_string(),
        0x0b => "ZEOF".to_string(),
        0x0c => "ZFERR".to_string(),
        0x0d => "ZCRC".to_string(),
        0x0e => "ZCHALLENGE".to_string(),
        0x0f => "ZCOMPL".to_string(),
        0x10 => "ZCAN".to_string(),
        0x11 => "ZFREECNT".to_string(),
        0x12 => "ZCOMMAND".to_string(),
        0x13 => "ZSTDERR".to_string(),
        other => format!("0x{other:02x}"),
    };
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == b'*' && data[i + 1] == b'*' && data[i + 2] == 0x18 && data[i + 3] == b'B' {
            let hi = (data[i + 4] as char).to_digit(16);
            let lo = (data[i + 5] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                *counts.entry(label(((h << 4) | l) as u8)).or_insert(0) += 1;
                i += 6;
                continue;
            }
        }
        if data[i] == b'*' && data[i + 1] == 0x18 && data[i + 2] != b'B' {
            *counts.entry(label(data[i + 2])).or_insert(0) += 1;
            i += 3;
            continue;
        }
        if data[i] == 0x18 && i + 4 < data.len() && data[i + 1..i + 5] == [0x18, 0x18, 0x18, 0x18] {
            *counts.entry("CAN-run".to_string()).or_insert(0) += 1;
            i += 5;
            continue;
        }
        i += 1;
    }
}

fn relay_copy(from: RawFd, to: RawFd, buf: &mut [u8], stop: &AtomicBool) -> CopyResult {
    match unistd::read(from, buf) {
        Ok(0) => CopyResult::Eof,
        Ok(n) => {
            let mut data = &buf[..n];
            while !data.is_empty() {
                if stop.load(Ordering::SeqCst) {
                    return CopyResult::Ok(0);
                }
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
            CopyResult::Ok(n)
        }
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => CopyResult::Ok(0),
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
    fn socketpair_buffer_stays_small_enough_to_pace_the_child() {
        // A large buffer here is not a throughput win: it lets the local child
        // run ahead of the ssh link, and the peer's line discipline overruns,
        // corrupting frames. Assert the value stays in the pacing range rather
        // than merely being set at all.
        let (a, _b) = make_socketpair().expect("socketpair");
        for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            let mut value: libc::c_int = 0;
            let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
            let rc = unsafe {
                libc::getsockopt(
                    a.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    &mut value as *mut libc::c_int as *mut _,
                    &mut len,
                )
            };
            assert_eq!(rc, 0, "getsockopt failed");
            assert!(
                value <= 128 * 1024,
                "socketpair buffer grew to {value} bytes; the child would outrun the link"
            );
        }
    }

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
    fn child_to_pty_mode_carries_child_output_only() {
        // This is the mode Faz uses: the pane's parser owns the pty's read
        // side, so the relay must not consume bytes the parser expects.
        //
        // socketpair note: writing to one end makes the bytes readable at the
        // *other* end. So "the parser's view" of the pty is `pty_side`'s peer,
        // and the relay holding `pty_side` is what could steal them.
        let (pty_side, parser_side) = make_socketpair().expect("pty stand-in");
        let (relay_end, child_end) = make_socketpair().expect("child pair");

        let stop = Arc::new(AtomicBool::new(false));
        let relay = Relay::start_with_mode(
            relay_end,
            pty_side.as_raw_fd(),
            stop.clone(),
            RelayMode::ChildToPty,
        )
        .expect("relay start");

        // child -> pty still works: the relay forwards it to `pty_side`.
        write_all(child_end.as_raw_fd(), b"to remote").expect("child write");
        let mut buf = [0u8; 32];
        let n = unistd::read(parser_side.as_raw_fd(), &mut buf).expect("parser read");
        assert_eq!(&buf[..n], b"to remote");

        // Put bytes on the pty for the parser. In ChildToPty mode the relay
        // must leave them alone, so they are still there to read.
        write_all(parser_side.as_raw_fd(), b"for the parser").expect("pty write");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let n = unistd::read(pty_side.as_raw_fd(), &mut buf).expect("read back from pty side");
        assert_eq!(
            &buf[..n],
            b"for the parser",
            "the relay stole bytes the parser needed"
        );

        // Nothing should have reached the child through the relay. `child_end`
        // is a blocking fd with no data on it, so use a zero-timeout poll
        // rather than a read that would hang forever on a passing test.
        let mut pfd = [PollFd::new(
            borrow(child_end.as_raw_fd()),
            PollFlags::POLLIN,
        )];
        let ready = nix::poll::poll(&mut pfd, PollTimeout::from(0u16)).unwrap_or(0);
        assert_eq!(ready, 0, "child received bytes it should not have");

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

#[cfg(test)]
mod relay_ordering_tests {
    use super::{make_socketpair, relay_copy};
    use nix::unistd;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::AtomicBool;

    fn fd_of(f: &std::os::fd::OwnedFd) -> i32 {
        f.as_raw_fd()
    }

    #[test]
    fn relay_copy_still_moves_a_full_buffer() {
        let (from, from_peer) = make_socketpair().expect("from pair");
        let (to, to_peer) = make_socketpair().expect("to pair");
        let payload = b"hello relay";
        unistd::write(&from_peer, payload).expect("seed");
        let mut buf = [0u8; 64];
        let moved = relay_copy(fd_of(&from), fd_of(&to), &mut buf, &AtomicBool::new(false));
        assert_eq!(moved.bytes(), payload.len() as u64);

        let mut got = [0u8; 64];
        let n = unistd::read(fd_of(&to_peer), &mut got).expect("read");
        assert_eq!(&got[..n], payload);
    }
}
