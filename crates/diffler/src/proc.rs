//! Child processes diffler shells out to, kept away from the terminal.
//!
//! The TUI owns the tty. A `git` or `gh` credential prompt inherited onto that
//! tty reads the user's keystrokes from behind the drawn screen and waits
//! forever, invisible. Nulled stdin plus these flags turn a prompt into an
//! immediate failure the app can report.

/// Environment that forbids a child from asking the user anything.
pub const NO_PROMPT: [(&str, &str); 2] =
    [("GIT_TERMINAL_PROMPT", "0"), ("GH_PROMPT_DISABLED", "1")];

/// Hand `url` to the system's default browser and return at once.
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // we reap the opener off the UI thread so it never lingers as a zombie
    std::thread::spawn(move || child.wait());
    Ok(())
}
