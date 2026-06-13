//! OSC52 clipboard sequences. The escape string is produced here and written
//! to the terminal stream by the main loop after a draw — never from inside
//! rendering, so the alternate screen stays intact. OSC52 is the only
//! clipboard mechanism (works over ssh/tmux, no native deps).

/// Wrap `text` in an OSC52 set-clipboard sequence for the `c` selection.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Standard base64 with padding, hand-rolled to avoid a direct dependency
/// for 25 testable lines.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |group: u32, shift: u32| -> char {
        let index = usize::try_from((group >> shift) & 0x3f).unwrap_or(0);
        ALPHABET.get(index).copied().unwrap_or(b'A') as char
    };
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk.first().copied().map_or(0, u32::from);
        let b1 = chunk.get(1).copied().map(u32::from);
        let b2 = chunk.get(2).copied().map(u32::from);
        let group = (b0 << 16) | (b1.unwrap_or(0) << 8) | b2.unwrap_or(0);
        out.push(symbol(group, 18));
        out.push(symbol(group, 12));
        out.push(if b1.is_some() { symbol(group, 6) } else { '=' });
        out.push(if b2.is_some() { symbol(group, 0) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_payload_in_escape_sequence() {
        let seq = osc52("hello");
        assert_eq!(seq, "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn multibyte_text_round_trips_through_the_payload() {
        let seq = osc52("héllo → world");
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with('\x07'));
    }
}
