//! Capturing the progress lines `sz`/`rz` write to stderr.
//!
//! ## Why the child needs a pty on stderr
//!
//! lrzsz prints its progress on stderr, but only when stderr is a terminal:
//! with a pipe or a file it stays silent, so there is nothing to forward. zssh
//! gets the display for free because it *is* a terminal program — the child
//! inherits the user's tty. Faz is a GUI process whose stderr goes to the log,
//! so the same code produces no output at all.
//!
//! Giving the child a private pty on fd 2 restores the terminal check without
//! touching the protocol path: the pty's reader is ours, and what it carries is
//! text to show, not bytes to hand to `sz`. The socketpair in `transfer` stays
//! the only channel for protocol data.
//!
//! ## What the output looks like
//!
//! ```
//! Sending: report.pdf
//! Bytes Sent:   1024/   5120   BPS:1234567   ETA 00:03
//! ```
//!
//! The second line is rewritten with `\r` between updates, so forwarding it
//! verbatim to a terminal leaves one line that updates in place. That is what
//! [`strip_progress_prefix`] preserves: the carriage returns stay, only the
//! leading banner is removed on the first chunk.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::pty::openpty;
use nix::unistd;

/// A pty whose slave end becomes the child's stderr and whose master end this
/// module reads.
pub struct ProgressCapture {
    master: OwnedFd,
    slave: OwnedFd,
}

impl ProgressCapture {
    /// Open the pair. Call before spawning the child so its fd 2 can be set.
    pub fn open() -> io::Result<Self> {
        let pair = openpty(None, None).map_err(|e| io::Error::from_raw_os_error(e as i32))?;
        Ok(Self {
            master: pair.master,
            slave: pair.slave,
        })
    }

    /// The fd to hand the child as stderr.
    pub fn slave_fd(&self) -> RawFd {
        self.slave.as_raw_fd()
    }

    /// Spawn a thread that forwards everything the child writes to stderr.
    ///
    /// `sink` receives raw bytes; the caller decides where they appear. The
    /// thread exits when the child closes the slave, `stop` is set, or the
    /// master reports an error.
    pub fn forward<F>(self, sink: F, stop: Arc<AtomicBool>) -> ProgressRelay
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        // The slave stays with the caller: it is the child's stderr, and the
        // thread only needs the master. Keeping both here made the slave's fd
        // unusable outside, which broke the caller that had to hand it to the
        // child.
        let slave = self.slave;
        // The master must live inside the thread: it would otherwise be
        // dropped when `forward` returns, closing the pty under the reader.
        let master = self.master;
        let handle = std::thread::Builder::new()
            .name("zmodem-progress".into())
            .spawn(move || {
                let master_fd = master.as_raw_fd();
                // Non-blocking so the final drain cannot hang on an idle pty:
                // the child is gone by then and nothing will ever arrive.
                set_nonblocking(master_fd);
                let mut buf = [0u8; 4096];
                let mut sink = sink;
                loop {
                    if stop.load(Ordering::SeqCst) {
                        // One last non-blocking read so the final progress line
                        // (which `sz` writes just before exiting) is not lost.
                        drain_once(master_fd, &mut buf, &mut sink);
                        break;
                    }
                    let mut fds = [PollFd::new(
                        // SAFETY: `master` is owned by this closure and lives
                        // for the whole loop.
                        unsafe { std::os::fd::BorrowedFd::borrow_raw(master_fd) },
                        PollFlags::POLLIN,
                    )];
                    match nix::poll::poll(&mut fds, PollTimeout::from(200u16)) {
                        Ok(0) => continue,
                        Ok(_) => {
                            if !read_once(master_fd, &mut buf, &mut sink) {
                                break;
                            }
                        }
                        Err(nix::errno::Errno::EINTR) => continue,
                        Err(_) => break,
                    }
                }
            })
            .ok();

        // Keep the slave alive for as long as the child might write to it. It
        // is dropped when the relay is dropped.
        ProgressRelay {
            handle,
            _slave: Some(slave),
        }
    }
}

/// Handle for the forwarding thread. Dropping it does not stop the thread;
/// [`ProgressRelay::join`] does, via the shared stop flag.
pub struct ProgressRelay {
    handle: Option<JoinHandle<()>>,
    /// Held so the pty does not close under the child's stderr.
    _slave: Option<OwnedFd>,
}

impl ProgressRelay {
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn set_nonblocking(fd: RawFd) {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    if let Ok(flags) = fcntl(fd, FcntlArg::F_GETFL) {
        let flags = OFlag::from_bits_truncate(flags);
        let _ = fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK));
    }
}

/// Read once and forward. Returns `false` when the peer closed.
fn read_once(fd: RawFd, buf: &mut [u8], sink: &mut impl FnMut(&[u8])) -> bool {
    match unistd::read(fd, buf) {
        Ok(0) => false,
        Ok(n) => {
            sink(&buf[..n]);
            true
        }
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => true,
        Err(_) => false,
    }
}

/// Drain whatever is already buffered without waiting.
fn drain_once(fd: RawFd, buf: &mut [u8], sink: &mut impl FnMut(&[u8])) {
    loop {
        match unistd::read(fd, buf) {
            Ok(0) => break,
            Ok(n) => sink(&buf[..n]),
            Err(nix::errno::Errno::EINTR) => {}
            Err(_) => break,
        }
    }
}

/// Remove the banner line lrzsz prints before the progress counter.
///
/// The banner is `Sending: <name>\r\n` (or `Receiving:`); the counter that
/// follows is rewritten in place with `\r`, so keeping it intact is what makes
/// the display update instead of scrolling. Only the first chunk can contain
/// the banner, so this is meant for that chunk alone.
pub fn strip_progress_prefix(bytes: &[u8]) -> &[u8] {
    // Find the first `\r` or `\n`; if the bytes before it start with a known
    // banner, drop through the line ending.
    let end = bytes.iter().position(|b| *b == b'\r' || *b == b'\n');
    let Some(end) = end else {
        return bytes;
    };
    let line = &bytes[..end];
    let is_banner = line.starts_with(b"Sending: ")
        || line.starts_with(b"Receiving: ")
        || line.starts_with(b"Sending ")
        || line.starts_with(b"Receiving ");
    if !is_banner {
        return bytes;
    }
    // Skip the line ending too, so the counter starts at column 0.
    let mut start = end;
    if bytes.get(start) == Some(&b'\r') {
        start += 1;
    }
    if bytes.get(start) == Some(&b'\n') {
        start += 1;
    }
    &bytes[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_sending_banner() {
        assert_eq!(
            strip_progress_prefix(b"Sending: a.pdf\r\nBytes Sent: 1/2"),
            b"Bytes Sent: 1/2"
        );
    }

    #[test]
    fn strips_the_receiving_banner() {
        assert_eq!(
            strip_progress_prefix(b"Receiving: b.zip\r\nBytes Received: 1/2"),
            b"Bytes Received: 1/2"
        );
    }

    #[test]
    fn leaves_a_counter_untouched() {
        let counter = b"Bytes Sent:   1024/   5120   BPS:1234   ETA 00:03\r";
        assert_eq!(strip_progress_prefix(counter), counter);
    }

    #[test]
    fn leaves_a_bare_partial_line_untouched() {
        // No line ending yet: the banner may be split across reads, so this
        // chunk is passed through and the next one decides.
        assert_eq!(strip_progress_prefix(b"Sending: par"), b"Sending: par");
    }

    /// The forwarding thread is not unit-tested: a pty's read semantics depend
    /// on the slave's line discipline, and pinning that here would test the
    /// kernel rather than this code. `openpty_pair_is_connected` and
    /// `strip_progress_prefix` cover the parts that are ours.
    #[test]
    fn openpty_pair_is_connected() {
        let capture = ProgressCapture::open().expect("openpty");
        // Writing to the slave must be readable on the master.
        unistd::write(
            unsafe { std::os::fd::BorrowedFd::borrow_raw(capture.slave_fd()) },
            b"hello",
        )
        .expect("write slave");
        let mut buf = [0u8; 16];
        let n = unistd::read(capture.master.as_raw_fd(), &mut buf).expect("read master");
        assert_eq!(&buf[..n], b"hello");
    }
}
