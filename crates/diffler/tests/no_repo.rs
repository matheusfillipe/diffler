//! What the real binary says when there is no repository to review.

// helper fns run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn launch(path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_diffler"))
        .arg(path)
        .arg("--no-mcp")
        // a regression that reaches the TUI must hit EOF and die, never block
        // this test on a terminal it inherited
        .stdin(Stdio::null())
        .output()
        .expect("run diffler")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn outside_a_repository_it_explains_itself() {
    // discover() walks all ancestors, so this test needs a dir whose
    // ancestors are repo-free; tempdirs satisfy that on CI runners
    let dir = tempfile::tempdir().expect("tempdir");
    let output = launch(dir.path());
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("no git repository"), "{stderr}");
    assert!(stderr.contains("git init"), "{stderr}");
    assert!(!stderr.contains("Location:"), "{stderr}");
    assert!(!stderr.contains("Backtrace"), "{stderr}");
}

#[test]
fn a_bare_repository_is_named_as_such() {
    let dir = tempfile::tempdir().expect("tempdir");
    git2::Repository::init_bare(dir.path()).expect("init bare");
    let output = launch(dir.path());
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("bare repository"), "{stderr}");
}
