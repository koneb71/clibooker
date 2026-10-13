//! In-band progress markers.
//!
//! The run wrapper script reports progress by printing private OSC sequences
//! (`ESC ] 7701 ; <payload> BEL`) into the PTY. The reader strips them from the
//! terminal output and hands the payloads to the run manager. Markers may be split
//! across reads, so partial sequences are held back until complete.

const START: &[u8] = b"\x1b]7701;";
const END: u8 = 0x07;
/// A marker longer than this is not one of ours; pass the bytes through.
const MAX_MARKER: usize = 256;

#[derive(Default)]
pub struct MarkerParser {
    pending: Vec<u8>,
}

impl MarkerParser {
    /// Returns the output with markers removed, and the marker payloads in order.
    pub fn feed(&mut self, data: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data);
        let mut out = Vec::with_capacity(buf.len());
        let mut payloads = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            if buf[i] == 0x1b {
                let rest = &buf[i..];
                if rest.starts_with(START) {
                    match rest[START.len()..].iter().position(|b| *b == END) {
                        Some(end) => {
                            let payload = &rest[START.len()..START.len() + end];
                            payloads.push(String::from_utf8_lossy(payload).into_owned());
                            i += START.len() + end + 1;
                            continue;
                        }
                        None if rest.len() < MAX_MARKER => {
                            self.pending = rest.to_vec();
                            break;
                        }
                        None => {}
                    }
                } else if START.starts_with(rest) {
                    // Could be the beginning of a marker cut off by the read boundary.
                    self.pending = rest.to_vec();
                    break;
                }
            }
            out.push(buf[i]);
            i += 1;
        }
        (out, payloads)
    }

    /// Anything still held back (call when the stream ends).
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markers() {
        let mut p = MarkerParser::default();
        let (out, pl) = p.feed(b"a\x1b]7701;step;1\x07b\x1b[1mc\x1b]7701;exit;1;0\x07");
        assert_eq!(out, b"ab\x1b[1mc");
        assert_eq!(pl, vec!["step;1", "exit;1;0"]);
    }

    #[test]
    fn handles_split_markers() {
        let mut p = MarkerParser::default();
        let (o1, p1) = p.feed(b"hello\x1b]77");
        assert_eq!(o1, b"hello");
        assert!(p1.is_empty());
        let (o2, p2) = p.feed(b"01;step;2");
        assert!(o2.is_empty() && p2.is_empty());
        let (o3, p3) = p.feed(b"\x07world\x1b");
        assert_eq!(o3, b"world");
        assert_eq!(p3, vec!["step;2"]);
        let (o4, _) = p.feed(b"[0m");
        assert_eq!(o4, b"\x1b[0m");
    }

    #[test]
    fn unterminated_long_sequence_passes_through() {
        let mut p = MarkerParser::default();
        let mut data = START.to_vec();
        data.extend(std::iter::repeat_n(b'x', MAX_MARKER + 10));
        let (out, pl) = p.feed(&data);
        assert_eq!(out.len(), data.len());
        assert!(pl.is_empty());
    }
}
