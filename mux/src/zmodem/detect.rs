//! Detection of an incoming ZMODEM header in the pty output stream.
//!
//! A ZMODEM session begins with the peer emitting a header. Every header
//! starts with the two bytes `**` followed by ZDLE (`0x18`), then either a
//! hexadecimal frame (`B` + two hex digits, used by `sz`/`rz` for control
//! frames such as ZRQINIT/ZRINIT) or a binary frame.
//!
//! We only need to recognize the start, not parse the protocol: once a
//! session is detected the raw byte stream is diverted away from the VT
//! parser entirely, because it carries binary data that the parser would
//! mangle. Ending the session is the transfer supervisor's job, not ours.
//!
//! The scan must survive the peer's output arriving split across reads, so
//! the matcher carries a small amount of state between calls: it remembers a
//! trailing `*` (or `**`) that might be the prefix of a header.

/// ZMODEM Data Link Escape. Introduces every header and every escape inside
/// a data subpacket.
pub const ZDLE: u8 = 0x18;

/// The marker that introduces a ZMODEM header.
pub const HEADER_MARKER: [u8; 2] = [b'*', b'*'];

/// Result of scanning one chunk of pty output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanResult {
    /// No header in this chunk; keep feeding output to the VT parser.
    NotStarted,
    /// A header begins at this offset within the chunk (the offset of the
    /// first `*`). Bytes before the offset are ordinary terminal output;
    /// bytes from the offset on belong to the ZMODEM session.
    Started { offset: usize },
}

/// Streaming scanner for the ZMODEM header prefix.
///
/// Feed every chunk of pty output through [`Detector::scan`]. Once it reports
/// [`ScanResult::Started`] the caller diverts the remainder of the stream and
/// stops calling `scan` until the session ends and [`Detector::reset`] runs.
///
/// The implementation keeps a two-byte tail of the previous chunk, scans the
/// concatenation, and reports an offset relative to the current chunk. Keeping
/// the raw tail (rather than a small enum) means every split position is
/// handled by the same code path.
#[derive(Debug, Default)]
pub struct Detector {
    /// Up to two bytes carried over from the previous chunk.
    tail: Vec<u8>,
}

impl Detector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget any carried bytes. Call when a session ends so a stray `*` from
    /// the finished transfer cannot pair with a later one.
    pub fn reset(&mut self) {
        self.tail.clear();
    }

    /// Scan `chunk` for the start of a ZMODEM header.
    ///
    /// Returns the offset of the header's first `*` within `chunk`. When the
    /// header began in a previous chunk, the offset is `0`: its leading bytes
    /// were already consumed, so everything from this chunk belongs to the
    /// session.
    pub fn scan(&mut self, chunk: &[u8]) -> ScanResult {
        let carried = self.tail.len();

        // Build the search window as carried-tail + chunk so a header split at
        // any position is matched by one code path.
        let mut window = Vec::with_capacity(carried + chunk.len());
        window.extend_from_slice(&self.tail);
        window.extend_from_slice(chunk);

        if let Some(pos) = find_header(&window) {
            self.tail.clear();
            if pos >= carried {
                return ScanResult::Started {
                    offset: pos - carried,
                };
            }
            // The header began inside the carried bytes, so this chunk only
            // contributes its later bytes; everything from here is a session.
            return ScanResult::Started { offset: 0 };
        }

        // No header. Keep the last two bytes of the *window* so a `**` split
        // across the next boundary is still matched. Taking them from the
        // window rather than from `chunk` is what makes a star carried from the
        // previous chunk survive into the next one.
        self.tail.clear();
        let start = window.len().saturating_sub(2);
        self.tail.extend_from_slice(&window[start..]);
        ScanResult::NotStarted
    }
}

/// Find the first `**` + ZDLE triple, returning the index of its first `*`.
fn find_header(haystack: &[u8]) -> Option<usize> {
    if haystack.len() < 3 {
        return None;
    }
    (0..=haystack.len() - 3).find(|&i| {
        haystack[i] == b'*' && haystack[i + 1] == b'*' && haystack[i + 2] == ZDLE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hexadecimal control header `sz`/`rz` send first.
    fn zrqinit() -> Vec<u8> {
        let mut v = b"**".to_vec();
        v.push(ZDLE);
        v.extend_from_slice(b"B00");
        v
    }

    #[test]
    fn plain_output_is_not_a_header() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"hello world\r\n"), ScanResult::NotStarted);
        assert_eq!(d.scan(b"$ ls -la\r\n"), ScanResult::NotStarted);
    }

    #[test]
    fn finds_header_at_start() {
        let mut d = Detector::new();
        let h = zrqinit();
        assert_eq!(d.scan(&h), ScanResult::Started { offset: 0 });
    }

    #[test]
    fn finds_header_after_output() {
        let mut d = Detector::new();
        let mut data = b"remote$ sz file.bin\r\n".to_vec();
        data.extend_from_slice(&zrqinit());
        assert_eq!(
            d.scan(&data),
            ScanResult::Started {
                offset: b"remote$ sz file.bin\r\n".len()
            }
        );
    }

    #[test]
    fn header_split_between_stars() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"*"), ScanResult::NotStarted);
        assert_eq!(d.scan(b"*"), ScanResult::NotStarted);
        assert_eq!(d.scan(&[ZDLE]), ScanResult::Started { offset: 0 });
    }

    #[test]
    fn header_split_after_both_stars() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"**"), ScanResult::NotStarted);
        assert_eq!(d.scan(&[ZDLE, b'B', b'0', b'0']), ScanResult::Started { offset: 0 });
    }

    #[test]
    fn header_recognised_before_the_hex_frame_arrives() {
        // The `**Zdle` triple is enough to know a session started; the frame
        // type byte after it belongs to the transfer, not to the detection.
        let mut d = Detector::new();
        assert_eq!(d.scan(b"**\x18"), ScanResult::Started { offset: 0 });
    }

    #[test]
    fn stray_single_star_before_a_real_header() {
        let mut d = Detector::new();
        // `*x` is not a prefix, but the `**Zdle` right after it is a header.
        assert_eq!(d.scan(b"*x**\x18"), ScanResult::Started { offset: 2 });
    }

    #[test]
    fn double_star_then_other_byte_is_not_a_header() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"**x"), ScanResult::NotStarted);
        // The trailing `x` clears the carry; a later real header is found.
        assert_eq!(d.scan(&zrqinit()), ScanResult::Started { offset: 0 });
    }

    #[test]
    fn three_stars_still_matches() {
        let mut d = Detector::new();
        let mut data = b"***".to_vec();
        data.push(ZDLE);
        assert_eq!(d.scan(&data), ScanResult::Started { offset: 1 });
    }

    #[test]
    fn glob_output_does_not_trigger() {
        // `*.txt` and `**/foo` are ordinary shell output, not headers.
        let mut d = Detector::new();
        assert_eq!(d.scan(b"*.txt **/foo *"), ScanResult::NotStarted);
        assert_eq!(d.scan(b" bar"), ScanResult::NotStarted);
    }

    #[test]
    fn carry_does_not_leak_after_reset() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"**"), ScanResult::NotStarted);
        d.reset();
        // A bare ZDLE after reset must not complete a stale header.
        assert_eq!(d.scan(&[ZDLE, b'B', b'0', b'0']), ScanResult::NotStarted);
    }
}
