//! $EDITOR integration. The TUI loop owns the terminal, so opening an
//! editor is a request: `App` queues an [`EditorRequest`], the main loop
//! suspends the terminal, runs the subprocess, restores the screen, and
//! hands the outcome back to `App::editor_finished`.

use std::path::Path;

use diffler_core::model::{FileDiff, FileStatus};

/// What to do with the editor's outcome once the terminal is back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorPurpose {
    /// Read the message file back and commit (gitcommit flow).
    Commit { msg_path: std::path::PathBuf },
    /// The human edited a file under review; refresh to pick up changes.
    OpenFile { path: String },
}

/// A subprocess the main loop must run with the terminal suspended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRequest {
    /// Full argv: program followed by its arguments.
    pub cmd: Vec<String>,
    pub purpose: EditorPurpose,
}

/// Editor command resolution: config → `$DIFFLER_EDITOR` → `$EDITOR` → `vi`.
/// Env values are passed in (not read here) so precedence stays testable.
pub fn resolve(config: Option<&str>, diffler_editor: Option<&str>, editor: Option<&str>) -> String {
    [config, diffler_editor, editor]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("vi")
        .to_owned()
}

/// Build the argv for opening `file` (optionally at `line`). The editor
/// string may carry its own flags (e.g. `code --wait`); the line-jump
/// syntax depends on the editor family, matched on the binary name.
pub fn command_for(editor: &str, file: &Path, line: Option<u32>) -> Vec<String> {
    let mut argv: Vec<String> = editor.split_whitespace().map(str::to_owned).collect();
    if argv.is_empty() {
        argv.push("vi".to_owned());
    }
    let stem = argv
        .first()
        .map(|program| {
            Path::new(program)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let file = file.to_string_lossy().into_owned();
    match (family(&stem), line) {
        (Family::PlusLine, Some(line)) => {
            argv.push(format!("+{line}"));
            argv.push(file);
        }
        (Family::Goto, Some(line)) => {
            argv.push("--goto".to_owned());
            argv.push(format!("{file}:{line}"));
        }
        _ => argv.push(file),
    }
    argv
}

enum Family {
    /// `editor +{line} file`
    PlusLine,
    /// `editor --goto file:line`
    Goto,
    Plain,
}

fn family(stem: &str) -> Family {
    match stem {
        "vi" | "vim" | "nvim" | "hx" | "kak" | "nano" | "micro" | "emacs" => Family::PlusLine,
        "code" | "code-insiders" | "codium" | "cursor" => Family::Goto,
        _ => Family::Plain,
    }
}

/// Spawn the editor and wait. Blocking by design: the caller runs this on a
/// blocking task while the TUI is suspended.
pub fn run(cmd: &[String]) -> std::io::Result<bool> {
    let (program, args) = cmd.split_first().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty editor command")
    })?;
    let status = std::process::Command::new(program).args(args).status()?;
    Ok(status.success())
}

/// Initial `COMMIT_EDITMSG` content: an empty message line followed by the
/// usual git comment block listing what will be committed.
pub fn commit_template(staged: &[FileDiff]) -> String {
    use std::fmt::Write as _;

    let mut out = String::from(
        "\n\
         # Please enter the commit message. Lines starting with '#' are ignored.\n\
         # An empty message aborts the commit.\n\
         #\n\
         # Staged:\n",
    );
    for file in staged {
        let _ = writeln!(out, "#\t{}: {}", status_label(file.status), file.path);
    }
    out
}

fn status_label(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        // an untracked file in the staged section only happens via fixtures;
        // staging turns it into an addition either way
        FileStatus::Added | FileStatus::Untracked => "new file",
    }
}

/// Turn the edited message file into a commit message: comment lines go,
/// trailing whitespace goes, and an effectively empty result is an abort.
pub fn strip_commit_message(raw: &str) -> Option<String> {
    let message = raw
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let message = message.trim_matches('\n');
    if message.trim().is_empty() {
        None
    } else {
        Some(message.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::model::FileDiff;

    use super::*;

    #[test]
    fn resolve_prefers_config_then_diffler_editor_then_editor() {
        assert_eq!(resolve(Some("hx"), Some("nvim"), Some("vim")), "hx");
        assert_eq!(resolve(None, Some("nvim"), Some("vim")), "nvim");
        assert_eq!(resolve(None, None, Some("vim")), "vim");
        assert_eq!(resolve(None, None, None), "vi");
    }

    #[test]
    fn resolve_skips_empty_and_blank_values() {
        assert_eq!(resolve(Some(""), Some("  "), Some("vim")), "vim");
        assert_eq!(resolve(Some(""), None, None), "vi");
    }

    #[test]
    fn vim_jumps_with_plus_line() {
        assert_eq!(
            command_for("vim", Path::new("/r/src/lib.rs"), Some(42)),
            vec!["vim", "+42", "/r/src/lib.rs"]
        );
    }

    #[test]
    fn helix_jumps_with_plus_line() {
        assert_eq!(
            command_for("hx", Path::new("/r/a.rs"), Some(7)),
            vec!["hx", "+7", "/r/a.rs"]
        );
    }

    #[test]
    fn vscode_jumps_with_goto() {
        assert_eq!(
            command_for("code", Path::new("/r/a.rs"), Some(7)),
            vec!["code", "--goto", "/r/a.rs:7"]
        );
    }

    #[test]
    fn unknown_editor_gets_the_plain_file() {
        assert_eq!(
            command_for("myedit", Path::new("/r/a.rs"), Some(7)),
            vec!["myedit", "/r/a.rs"]
        );
    }

    #[test]
    fn no_line_means_plain_file_for_every_family() {
        for editor in ["vim", "code", "myedit"] {
            assert_eq!(
                command_for(editor, Path::new("/r/a.rs"), None),
                vec![editor, "/r/a.rs"]
            );
        }
    }

    #[test]
    fn editor_string_keeps_its_own_flags_and_matches_on_the_binary() {
        assert_eq!(
            command_for("/usr/bin/code --wait", Path::new("/r/a.rs"), Some(3)),
            vec!["/usr/bin/code", "--wait", "--goto", "/r/a.rs:3"]
        );
    }

    #[test]
    fn empty_editor_string_falls_back_to_vi() {
        assert_eq!(
            command_for("  ", Path::new("/r/a.rs"), None),
            vec!["vi", "/r/a.rs"]
        );
    }

    fn file(path: &str, status: FileStatus) -> FileDiff {
        FileDiff {
            path: path.to_owned(),
            old_path: None,
            status,
            binary: false,
            old_text: None,
            new_text: None,
            hunks: Vec::new(),
        }
    }

    #[test]
    fn template_lists_staged_files_with_labels() {
        let staged = vec![
            file("ci.yml", FileStatus::Added),
            file("src/lib.rs", FileStatus::Modified),
        ];
        let template = commit_template(&staged);
        assert!(template.starts_with('\n'), "message area comes first");
        assert!(template.contains("# An empty message aborts the commit."));
        assert!(template.contains("#\tnew file: ci.yml"));
        assert!(template.contains("#\tmodified: src/lib.rs"));
        assert_eq!(
            strip_commit_message(&template),
            None,
            "the untouched template aborts"
        );
    }

    #[test]
    fn strip_drops_comments_and_trailing_whitespace() {
        let raw = "fix the thing   \n\n# comment\nbody line\n# trailing comment\n";
        assert_eq!(
            strip_commit_message(raw).as_deref(),
            Some("fix the thing\n\nbody line")
        );
    }

    #[test]
    fn strip_keeps_hashes_that_do_not_start_the_line() {
        assert_eq!(
            strip_commit_message("see issue #42\n").as_deref(),
            Some("see issue #42")
        );
    }

    #[test]
    fn strip_of_comments_or_whitespace_only_is_none() {
        assert_eq!(strip_commit_message(""), None);
        assert_eq!(strip_commit_message("   \n\n"), None);
        assert_eq!(strip_commit_message("# all\n# comments\n"), None);
    }
}
