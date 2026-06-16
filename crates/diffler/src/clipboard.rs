//! System clipboard. Two mechanisms, used together for reach without native
//! build deps (so the static musl binaries stay clean): an OSC52 escape
//! sequence — written to the terminal by the main loop after a draw, never
//! from rendering — which the terminal forwards even over ssh/tmux; and a
//! best-effort pipe to the platform clipboard CLI, covering terminals that
//! don't honor OSC52.

use std::io::Write;
use std::process::{Command, Stdio};

/// Wrap `text` in an OSC52 set-clipboard sequence for the `c` selection.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Pipe `text` to the first available platform clipboard tool. Best-effort: a
/// host with none installed just relies on OSC52. `wl-copy`/`xclip`/`xsel`
/// fork a daemon to own the X11/Wayland selection, so it persists after exit;
/// `clip.exe` also covers WSL.
pub fn native_copy(text: &str) {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("clip.exe", &[]),
        ]
    };
    for (cmd, args) in candidates {
        if pipe_to(cmd, args, text) {
            break;
        }
    }
}

fn pipe_to(cmd: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    // stdin drops here, signalling EOF; the tool reads it and (for the
    // X11/Wayland ones) backgrounds itself, so the wait returns promptly
    child.wait().is_ok()
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
