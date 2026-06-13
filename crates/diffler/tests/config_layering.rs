//! End-to-end config layering through the real binary's `config --dump`.
//!
//! Env vars are set on the child process only: edition 2024 makes
//! `std::env::set_var` unsafe (and the workspace denies unsafe code), and
//! per-child env needs no cross-test serialization.

// helper fns run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

fn write_config(dir: &Path, contents: &str) {
    std::fs::create_dir_all(dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), contents).expect("config file");
}

fn init_repo(dir: &Path) {
    git2::Repository::init(dir).expect("git init");
}

/// Run `diffler <repo> config --dump <extra>` with a controlled environment.
fn dump(repo: &Path, xdg: Option<&Path>, home: Option<&Path>, extra: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_diffler"));
    cmd.arg(repo).arg("config").arg("--dump").args(extra);
    cmd.env_remove("XDG_CONFIG_HOME").env_remove("HOME");
    if let Some(xdg) = xdg {
        cmd.env("XDG_CONFIG_HOME", xdg);
    }
    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    cmd.output().expect("run diffler")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "diffler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn defaults_dump_when_no_config_exists() {
    let repo = tempfile::tempdir().expect("repo dir");
    let xdg = tempfile::tempdir().expect("xdg dir");
    init_repo(repo.path());

    let out = stdout(&dump(repo.path(), Some(xdg.path()), None, &[]));
    assert!(out.contains("theme = \"github-dark\""));
    assert!(out.contains("context_lines = 3"));
    assert!(out.contains("recent_commits = 10"));
    assert!(out.contains("enabled = true"));
    assert!(out.contains("port = 8417"));
    assert!(out.contains("# ui.theme = default"));
    assert!(out.contains("# mcp.port = default"));
    assert!(out.contains("# editor.command = default"));
}

#[test]
fn precedence_global_project_cli_via_xdg() {
    let repo = tempfile::tempdir().expect("repo dir");
    let xdg = tempfile::tempdir().expect("xdg dir");
    init_repo(repo.path());

    write_config(
        &xdg.path().join("diffler"),
        "[ui]\ntheme = \"global-theme\"\ncontext_lines = 7\n\n[mcp]\nport = 9000\n\n[keys.status]\nquit = \"q\"\n",
    );
    write_config(
        &repo.path().join(".diffler"),
        "[ui]\ntheme = \"project-theme\"\n\n[keys.status]\nrefresh = \"<c-r>\"\n",
    );

    let out = stdout(&dump(
        repo.path(),
        Some(xdg.path()),
        None,
        &["--port", "9999", "--no-mcp"],
    ));
    // per-field precedence: project beats global on theme, global survives on
    // context_lines and the quit key, CLI beats both on port/enabled
    assert!(out.contains("theme = \"project-theme\""));
    assert!(out.contains("context_lines = 7"));
    assert!(out.contains("port = 9999"));
    assert!(out.contains("enabled = false"));
    assert!(out.contains("quit = \"q\""));
    assert!(out.contains("refresh = \"<c-r>\""));

    assert!(out.contains("# ui.theme = project:"));
    assert!(out.contains("# ui.context_lines = global:"));
    assert!(out.contains("# mcp.port = cli"));
    assert!(out.contains("# mcp.enabled = cli"));
    assert!(out.contains("# keys.status.quit = global:"));
    assert!(out.contains("# keys.status.refresh = project:"));
}

#[test]
fn home_dot_config_is_used_when_xdg_unset() {
    let repo = tempfile::tempdir().expect("repo dir");
    let home = tempfile::tempdir().expect("home dir");
    init_repo(repo.path());

    write_config(
        &home.path().join(".config").join("diffler"),
        "[ui]\ntheme = \"home-theme\"\n",
    );

    let out = stdout(&dump(repo.path(), None, Some(home.path()), &[]));
    assert!(out.contains("theme = \"home-theme\""));
    assert!(out.contains("# ui.theme = global:"));
}

#[test]
fn cli_theme_flag_wins_over_files() {
    let repo = tempfile::tempdir().expect("repo dir");
    let xdg = tempfile::tempdir().expect("xdg dir");
    init_repo(repo.path());
    write_config(
        &xdg.path().join("diffler"),
        "[ui]\ntheme = \"global-theme\"\n",
    );

    let out = stdout(&dump(
        repo.path(),
        Some(xdg.path()),
        None,
        &["--theme", "cli-theme"],
    ));
    assert!(out.contains("theme = \"cli-theme\""));
    assert!(out.contains("# ui.theme = cli"));
}

#[test]
fn unknown_keys_surface_as_warnings() {
    let repo = tempfile::tempdir().expect("repo dir");
    let xdg = tempfile::tempdir().expect("xdg dir");
    init_repo(repo.path());
    write_config(&xdg.path().join("diffler"), "[ui]\nthme = \"oops\"\n");

    let out = stdout(&dump(repo.path(), Some(xdg.path()), None, &[]));
    assert!(out.contains("## warnings"));
    assert!(out.contains("unknown key `ui.thme`"));
}

#[test]
fn bad_toml_fails_and_names_the_file() {
    let repo = tempfile::tempdir().expect("repo dir");
    let xdg = tempfile::tempdir().expect("xdg dir");
    init_repo(repo.path());
    write_config(&xdg.path().join("diffler"), "[ui\ntheme = ");

    let output = dump(repo.path(), Some(xdg.path()), None, &[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = xdg.path().join("diffler").join("config.toml");
    assert!(
        stderr.contains(&expected.display().to_string()),
        "stderr should name the broken file: {stderr}"
    );
}

#[test]
fn outside_a_repo_project_layer_is_skipped() {
    let dir = tempfile::tempdir().expect("plain dir");
    let xdg = tempfile::tempdir().expect("xdg dir");

    let out = stdout(&dump(dir.path(), Some(xdg.path()), None, &[]));
    assert!(out.contains("theme = \"github-dark\""));
}

// A bad chord string must appear under ## warnings in
// --dump output, naming the dotted key path so the user knows what to fix.
#[test]
fn bad_chord_appears_in_dump_warnings() {
    let repo = tempfile::tempdir().expect("repo dir");
    let xdg = tempfile::tempdir().expect("xdg dir");
    init_repo(repo.path());
    write_config(
        &xdg.path().join("diffler"),
        "[keys.status]\nquit = \"q\"\nrefresh = \"<ctlr-r>\"\n",
    );

    let out = stdout(&dump(repo.path(), Some(xdg.path()), None, &[]));
    assert!(
        out.contains("## warnings"),
        "dump should have a warnings section"
    );
    assert!(
        out.contains("keys.status.refresh"),
        "warning should name the key path: {out}"
    );
    assert!(
        out.contains("<ctlr-r>"),
        "warning should quote the bad chord: {out}"
    );
    // good chord is still stored (not warned about)
    assert!(out.contains("quit = \"q\""));
}
