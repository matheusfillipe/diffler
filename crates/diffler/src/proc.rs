//! Child processes diffler shells out to, kept away from the terminal.
//!
//! The TUI owns the tty. A `git` or `gh` credential prompt inherited onto that
//! tty reads the user's keystrokes from behind the drawn screen and waits
//! forever, invisible. Nulled stdin plus these flags turn a prompt into an
//! immediate failure the app can report.

/// Environment that forbids a child from asking the user anything.
pub const NO_PROMPT: [(&str, &str); 2] =
    [("GIT_TERMINAL_PROMPT", "0"), ("GH_PROMPT_DISABLED", "1")];
