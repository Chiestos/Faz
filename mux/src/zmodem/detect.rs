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

/// Which side initiated, inferred from the first header's frame type.
///
/// `sz` opens with ZRQINIT (it has something to send); `rz` opens with
/// ZRINIT (it is ready to receive). Seeing ZRQINIT therefore means the remote
/// is sending, so we receive; ZRINIT means the remote wants to receive, so we
/// send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    /// The remote is offering a file: we receive.
    RemoteSends,
    /// The remote is asking for a file: we send.
    RemoteReceives,
    /// A header started, but the frame type is not in this chunk yet, or it is
    /// a frame that does not identify the initiator (a mid-session header).
    Unknown,
}

/// ZMODEM frame type codes that appear as the first two hex digits of a ZHEX
/// header.
const ZRQINIT: u8 = 0x00;
const ZRINIT: u8 = 0x01;

/// Result of scanning one chunk of pty output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanResult {
    /// No header in this chunk; keep feeding output to the VT parser.
    NotStarted,
    /// A header begins at this offset within the chunk (the offset of the
    /// first `*`). Bytes before the offset are ordinary terminal output;
    /// bytes from the offset on belong to the ZMODEM session.
    Started { offset: usize, initiator: Initiator },
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

        if let Some((pos, initiator)) = find_header(&window) {
            self.tail.clear();
            let offset = if pos >= carried { pos - carried } else { 0 };
            return ScanResult::Started { offset, initiator };
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

/// Find the first `**` + ZDLE triple.
///
/// Returns the index of its first `*` and, when the frame type is already
/// present in the buffer, which side initiated the session.
///
/// The frame type follows as either `B` + two hex digits (ZHEX, what `sz`/`rz`
/// emit for control frames) or a raw byte (binary frames). Only ZHEX is
/// decoded here: a binary first frame does not identify the initiator, and the
/// GUI can still ask the user which direction to run.
fn find_header(haystack: &[u8]) -> Option<(usize, Initiator)> {
    if haystack.len() < 3 {
        return None;
    }
    for i in 0..=haystack.len() - 3 {
        if haystack[i] != b'*' || haystack[i + 1] != b'*' || haystack[i + 2] != ZDLE {
            continue;
        }
        return Some((i, initiator_at(haystack, i + 3)));
    }
    None
}

/// Offset of the last complete control frame in `bytes`, if any.
///
/// A peer waiting for its counterpart retransmits its opening frame (ZRINIT
/// from `rz`, ZRQINIT from `sz`) every few seconds. Those retransmissions pile
/// up while the destination is being chosen, and handing the local child the
/// whole pile makes it see a peer that restarted its side of the handshake mid
/// conversation — which it answers with a CAN burst that aborts the transfer.
///
/// Only the newest copy describes the state the peer is actually in, so this
/// returns where it starts and the caller drops everything before it.
pub fn last_control_frame_start(bytes: &[u8]) -> Option<usize> {
    let mut last = None;
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' && bytes[i + 2] == ZDLE {
            last = Some(i);
            i += 3;
            continue;
        }
        i += 1;
    }
    last
}

/// Decode the initiator from a ZHEX frame type at `idx`, if present.
fn initiator_at(buf: &[u8], idx: usize) -> Initiator {
    // ZHEX: 'B' then two hex digits.
    if buf.get(idx) != Some(&b'B') {
        return Initiator::Unknown;
    }
    let (Some(hi), Some(lo)) = (buf.get(idx + 1), buf.get(idx + 2)) else {
        return Initiator::Unknown;
    };
    let Some(frame) = hex_byte(*hi, *lo) else {
        return Initiator::Unknown;
    };
    match frame {
        ZRQINIT => Initiator::RemoteSends,
        ZRINIT => Initiator::RemoteReceives,
        _ => Initiator::Unknown,
    }
}

fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    let hi = (hi as char).to_digit(16)?;
    let lo = (lo as char).to_digit(16)?;
    Some(((hi << 4) | lo) as u8)
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
        assert_eq!(
            d.scan(&h),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::RemoteSends
            }
        );
    }

    #[test]
    fn zrinit_means_the_remote_wants_to_receive() {
        // `rz` on the remote opens with ZRINIT, asking us to send.
        let mut d = Detector::new();
        let mut h = b"**".to_vec();
        h.push(ZDLE);
        h.extend_from_slice(b"B01");
        assert_eq!(
            d.scan(&h),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::RemoteReceives
            }
        );
    }

    #[test]
    fn header_without_a_frame_type_is_unknown() {
        let mut d = Detector::new();
        assert_eq!(
            d.scan(b"**\x18"),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::Unknown
            }
        );
    }

    #[test]
    fn header_with_a_partial_frame_type_is_unknown() {
        let mut d = Detector::new();
        assert_eq!(
            d.scan(b"**\x18B0"),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::Unknown
            }
        );
    }

    #[test]
    fn header_with_a_non_control_frame_type_is_unknown() {
        // ZFILE (0x04) appears mid-session, not as the initiator.
        let mut d = Detector::new();
        assert_eq!(
            d.scan(b"**\x18B04"),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::Unknown
            }
        );
    }

    #[test]
    fn finds_header_after_output() {
        let mut d = Detector::new();
        let mut data = b"remote$ sz file.bin\r\n".to_vec();
        data.extend_from_slice(&zrqinit());
        assert_eq!(
            d.scan(&data),
            ScanResult::Started {
                offset: b"remote$ sz file.bin\r\n".len(),
                initiator: Initiator::RemoteSends,
            }
        );
    }

    #[test]
    fn header_split_between_stars() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"*"), ScanResult::NotStarted);
        assert_eq!(d.scan(b"*"), ScanResult::NotStarted);
        assert_eq!(
            d.scan(&[ZDLE]),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::Unknown
            }
        );
    }

    #[test]
    fn header_split_after_both_stars() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"**"), ScanResult::NotStarted);
        assert_eq!(
            d.scan(&[ZDLE, b'B', b'0', b'0']),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::RemoteSends
            }
        );
    }

    #[test]
    fn header_recognised_before_the_hex_frame_arrives() {
        // The `**Zdle` triple is enough to know a session started; the frame
        // type byte after it belongs to the transfer, not to the detection.
        let mut d = Detector::new();
        assert_eq!(
            d.scan(b"**\x18"),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::Unknown
            }
        );
    }

    #[test]
    fn stray_single_star_before_a_real_header() {
        let mut d = Detector::new();
        // `*x` is not a prefix, but the `**Zdle` right after it is a header.
        assert_eq!(
            d.scan(b"*x**\x18"),
            ScanResult::Started {
                offset: 2,
                initiator: Initiator::Unknown
            }
        );
    }

    #[test]
    fn double_star_then_other_byte_is_not_a_header() {
        let mut d = Detector::new();
        assert_eq!(d.scan(b"**x"), ScanResult::NotStarted);
        // The trailing `x` clears the carry; a later real header is found.
        assert_eq!(
            d.scan(&zrqinit()),
            ScanResult::Started {
                offset: 0,
                initiator: Initiator::RemoteSends
            }
        );
    }

    #[test]
    fn three_stars_still_matches() {
        let mut d = Detector::new();
        let mut data = b"***".to_vec();
        data.push(ZDLE);
        assert_eq!(
            d.scan(&data),
            ScanResult::Started {
                offset: 1,
                initiator: Initiator::Unknown
            }
        );
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

    /// A complete ZRINIT as `rz` emits it: 21 bytes.
    fn zrinit() -> Vec<u8> {
        let mut v = b"**".to_vec();
        v.push(ZDLE);
        v.extend_from_slice(b"B0100000023be50\r\x8a\x11");
        v
    }

    #[test]
    fn last_control_frame_finds_a_single_header() {
        let frame = zrinit();
        assert_eq!(last_control_frame_start(&frame), Some(0));
    }

    #[test]
    fn last_control_frame_points_at_the_newest_retransmission() {
        // While the picker is open the peer keeps repeating ZRINIT. Replaying
        // every copy makes the child think the handshake restarted.
        let frame = zrinit();
        let mut buf = Vec::new();
        buf.extend_from_slice(&frame);
        buf.extend_from_slice(&frame);
        assert_eq!(last_control_frame_start(&buf), Some(frame.len()));
    }

    #[test]
    fn last_control_frame_is_none_without_a_header() {
        assert_eq!(last_control_frame_start(b"plain shell output"), None);
        assert_eq!(last_control_frame_start(b""), None);
    }

    #[test]
    fn last_control_frame_ignores_output_before_the_header() {
        let frame = zrinit();
        let mut buf = b"noise\r\n".to_vec();
        buf.extend_from_slice(&frame);
        assert_eq!(last_control_frame_start(&buf), Some(7));
    }
}
