//! Markdown export of review feedback: comments with diff context, ready
//! to paste into any agent prompt.

// writing into a String is infallible, so the `write!` results are discarded
use std::fmt::Write as _;

use crate::model::{DiffLine, DiffModel};
use crate::session::{Comment, CommentStatus, Session};

pub struct FeedbackOptions<'a> {
    /// Header text; the caller builds it (repo/branch/count need `HeadInfo`).
    pub title: &'a str,
    /// When set, only comments anchored to this file are exported.
    pub file_filter: Option<&'a str>,
    pub include_resolved: bool,
}

pub fn to_markdown(session: &Session, model: &DiffModel, opts: &FeedbackOptions<'_>) -> String {
    let mut comments: Vec<&Comment> = session
        .comments
        .iter()
        .filter(|c| opts.include_resolved || c.status != CommentStatus::Resolved)
        .filter(|c| opts.file_filter.is_none_or(|f| c.anchor.file == f))
        .collect();
    comments.sort_by(|a, b| (&a.anchor.file, a.anchor.line).cmp(&(&b.anchor.file, b.anchor.line)));

    let mut out = String::new();
    let _ = writeln!(out, "## {}", opts.title);
    for comment in comments {
        out.push('\n');
        render_comment(&mut out, comment, model, opts.include_resolved);
    }
    out
}

fn render_comment(out: &mut String, comment: &Comment, model: &DiffModel, include_resolved: bool) {
    let anchor = &comment.anchor;
    let _ = match (anchor.line, anchor.line_end) {
        (Some(line), Some(end)) => writeln!(out, "### {}:{line}-{end}", anchor.file),
        (Some(line), None) => writeln!(out, "### {}:{line}", anchor.file),
        _ => writeln!(out, "### {}", anchor.file),
    };

    if let Some(line) = anchor.line {
        match context_snippet(model, &anchor.file, line, anchor.on_old_side) {
            Some(snippet) => {
                // a fence longer than any backtick run in the snippet, so
                // diff content containing ``` can't break out of it
                let longest_run = snippet
                    .iter()
                    .map(|(_, text)| longest_backtick_run(text))
                    .max()
                    .unwrap_or(0);
                let fence = "`".repeat((longest_run + 1).max(3));
                let _ = writeln!(out, "{fence}");
                for (origin, text) in snippet {
                    let _ = writeln!(out, "{origin}{text}");
                }
                let _ = writeln!(out, "{fence}");
            }
            None => out.push_str("_(outdated)_\n"),
        }
    } else if anchor.is_outdated(model) {
        out.push_str("_(outdated)_\n");
    }

    for body_line in comment.body.lines() {
        let _ = writeln!(out, "> {body_line}");
    }
    for reply in &comment.replies {
        let mut lines = reply.body.lines();
        if let Some(first) = lines.next() {
            let _ = writeln!(out, "> > {}: {first}", reply.author);
        }
        for rest in lines {
            let _ = writeln!(out, "> > {rest}");
        }
    }
    if include_resolved && comment.status == CommentStatus::Resolved {
        out.push_str("_(resolved)_\n");
    }
}

/// The anchored line plus its immediate neighbors within the same hunk,
/// each tagged with its diff origin char (' ', '-', '+'). `None` when the
/// file or line has left the model (the comment is outdated). Shared by
/// the markdown export and the MCP comment payloads.
pub fn context_snippet(
    model: &DiffModel,
    file: &str,
    line: u32,
    on_old_side: bool,
) -> Option<Vec<(char, String)>> {
    let file = model.files.iter().find(|f| f.path == file)?;
    for hunk in &file.hunks {
        let Some(idx) = hunk
            .lines
            .iter()
            .position(|l| line_no(l, on_old_side) == Some(line))
        else {
            continue;
        };
        let start = idx.saturating_sub(1);
        let end = (idx + 2).min(hunk.lines.len());
        let snippet = hunk
            .lines
            .get(start..end)?
            .iter()
            .map(|l| (l.kind.origin(), l.text.clone()))
            .collect();
        return Some(snippet);
    }
    None
}

fn line_no(line: &DiffLine, on_old_side: bool) -> Option<u32> {
    if on_old_side {
        line.old_no
    } else {
        line.new_no
    }
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for c in text.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use crate::model::{FileDiff, FileStatus, Hunk, HunkId, LineKind};
    use crate::session::Anchor;

    use super::*;

    fn anchor(file: &str, line: Option<u32>) -> Anchor {
        Anchor {
            file: file.to_owned(),
            line,
            line_end: None,
            on_old_side: false,
            line_text: None,
        }
    }

    fn diff_line(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: &str) -> DiffLine {
        DiffLine::new(kind, old_no, new_no, text.to_owned())
    }

    /// One file, one hunk: context(1/1), deleted(2), added(2), context(3/3).
    fn sample_model() -> DiffModel {
        DiffModel {
            files: vec![FileDiff {
                path: "src/auth.py".into(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                old_text: None,
                new_text: Some("one\nTWO\nthree\n".into()),
                hunks: vec![Hunk {
                    id: HunkId("h1".into()),
                    old_start: 1,
                    old_lines: 3,
                    new_start: 1,
                    new_lines: 3,
                    context: String::new(),
                    lines: vec![
                        diff_line(LineKind::Context, Some(1), Some(1), "one"),
                        diff_line(LineKind::Deleted, Some(2), None, "two"),
                        diff_line(LineKind::Added, None, Some(2), "TWO"),
                        diff_line(LineKind::Context, Some(3), Some(3), "three"),
                    ],
                }],
                hashes: crate::model::HashCache::default(),
            }],
        }
    }

    fn opts<'a>() -> FeedbackOptions<'a> {
        FeedbackOptions {
            title: "Review feedback",
            file_filter: None,
            include_resolved: false,
        }
    }

    #[test]
    fn single_line_comment_renders_heading_and_context_fence() {
        let mut s = Session::default();
        s.add_comment("mattf", anchor("src/auth.py", Some(2)), "why uppercase?");
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(md.starts_with("## Review feedback\n\n"));
        assert!(md.contains("### src/auth.py:2\n"));
        assert!(md.contains("```\n-two\n+TWO\n three\n```\n"));
        assert!(md.contains("> why uppercase?\n"));
        assert!(md.ends_with('\n'));
    }

    #[test]
    fn old_side_anchor_finds_deleted_line() {
        let mut s = Session::default();
        let mut a = anchor("src/auth.py", Some(2));
        a.on_old_side = true;
        s.add_comment("mattf", a, "what was wrong with two?");
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(md.contains("```\n one\n-two\n+TWO\n```\n"));
    }

    #[test]
    fn range_comment_renders_start_dash_end() {
        let mut s = Session::default();
        let mut a = anchor("src/auth.py", Some(3));
        a.line_end = Some(5);
        s.add_comment("mattf", a, "this whole block");
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(md.contains("### src/auth.py:3-5\n"));
    }

    #[test]
    fn file_filter_excludes_other_files() {
        let mut s = Session::default();
        s.add_comment("mattf", anchor("src/auth.py", Some(2)), "keep");
        s.add_comment("mattf", anchor("other.py", Some(1)), "drop");
        let o = FeedbackOptions {
            file_filter: Some("src/auth.py"),
            ..opts()
        };
        let md = to_markdown(&s, &sample_model(), &o);
        assert!(md.contains("> keep\n"));
        assert!(!md.contains("drop"));
    }

    #[test]
    fn resolved_skipped_by_default_included_and_marked_with_flag() {
        let mut s = Session::default();
        let id = s
            .add_comment("mattf", anchor("src/auth.py", Some(2)), "done already")
            .id
            .clone();
        assert!(s.resolve(&id));
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(!md.contains("done already"));

        let o = FeedbackOptions {
            include_resolved: true,
            ..opts()
        };
        let md = to_markdown(&s, &sample_model(), &o);
        assert!(md.contains("> done already\n"));
        assert!(md.contains("_(resolved)_\n"));
    }

    #[test]
    fn departed_file_renders_outdated_marker() {
        let mut s = Session::default();
        s.add_comment("mattf", anchor("gone.py", Some(7)), "still matters");
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(md.contains("### gone.py:7\n_(outdated)_\n"));
        assert!(md.contains("> still matters\n"));
    }

    #[test]
    fn departed_line_renders_outdated_marker() {
        let mut s = Session::default();
        s.add_comment("mattf", anchor("src/auth.py", Some(99)), "moved on");
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(md.contains("### src/auth.py:99\n_(outdated)_\n"));
    }

    #[test]
    fn file_level_comment_has_no_fence_when_file_present() {
        let mut s = Session::default();
        s.add_comment("mattf", anchor("src/auth.py", None), "overall: nice");
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(md.contains("### src/auth.py\n> overall: nice\n"));
        assert!(!md.contains("```"));
        assert!(!md.contains("_(outdated)_"));
    }

    #[test]
    fn replies_render_as_nested_quotes() {
        let mut s = Session::default();
        let id = s
            .add_comment("mattf", anchor("src/auth.py", Some(2)), "why?")
            .id
            .clone();
        assert!(s.reply(&id, "agent", "because tests\nand style"));
        let md = to_markdown(&s, &sample_model(), &opts());
        assert!(md.contains("> why?\n> > agent: because tests\n> > and style\n"));
    }

    #[test]
    fn fenced_context_survives_backticks_in_diff_content() {
        let mut model = sample_model();
        if let Some(line) = model.files[0].hunks[0].lines.get_mut(2) {
            line.text = "````md".into();
        }
        let mut s = Session::default();
        s.add_comment("mattf", anchor("src/auth.py", Some(2)), "fence bomb");
        let md = to_markdown(&s, &model, &opts());
        let fence = "`````";
        assert!(
            md.contains(&format!("{fence}\n")),
            "fence must outrun content runs: {md}"
        );
        let open = md.find(fence).expect("opening fence");
        let close = md.rfind(fence).expect("closing fence");
        assert!(close > open);
    }

    #[test]
    fn comments_order_by_file_then_line() {
        let mut s = Session::default();
        s.add_comment("mattf", anchor("z.py", Some(1)), "third");
        s.add_comment("mattf", anchor("src/auth.py", Some(3)), "second");
        s.add_comment("mattf", anchor("src/auth.py", Some(1)), "first");
        let md = to_markdown(&s, &sample_model(), &opts());
        let first = md.find("> first").expect("first present");
        let second = md.find("> second").expect("second present");
        let third = md.find("> third").expect("third present");
        assert!(first < second && second < third);
    }
}
