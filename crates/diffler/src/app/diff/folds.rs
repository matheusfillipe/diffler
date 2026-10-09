//! Hunk folds in the diff pane. A folded hunk shows as its header row naming
//! what it hides, so `]`/`[` land on it folded or open. A fold is keyed by the
//! hunk's id, so a hunk the agent edits comes back open.

use std::collections::HashSet;

use diffler_core::model::{DiffLine, DiffModel, FileDiff, Hunk, LineKind};

use super::rows::line_row_text;
use super::{DiffRow, RowCopy, SplitRow};

/// What one `DiffRow::Fold` row stands for: one folded hunk.
#[derive(Debug, Clone)]
pub(crate) struct FoldGroup {
    /// The folded hunk's key, one entry, kept as a list for the lookups that
    /// read fold identity (`RowRef::Fold`, yank, search).
    pub keys: Vec<String>,
    pub hunk: usize,
    /// Every line the row hides, in order.
    pub lines: Vec<(usize, usize)>,
    pub label: String,
}

/// The key a hunk folds under.
pub(crate) fn hunk_key(hunk: &Hunk) -> String {
    hunk.id.0.clone()
}

/// The hunk header's own text plus what folding it hides, e.g.
/// `@@ -19,7 +19,7 @@ fn helper_4() {  ⋯ 7 lines +1 -1 · 1 comment`.
pub(crate) fn fold_label(hunk: &Hunk, comments: usize) -> String {
    let added = hunk
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .count();
    let deleted = hunk
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Deleted)
        .count();
    let ranges = format!(
        "@@ -{},{} +{},{} @@",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
    );
    let heading = if hunk.context.is_empty() {
        ranges
    } else {
        format!("{ranges} {}", hunk.context)
    };
    let noted = match comments {
        0 => String::new(),
        1 => " · 1 comment".to_owned(),
        n => format!(" · {n} comments"),
    };
    format!(
        "{heading}  ⋯ {} lines +{added} -{deleted}{noted}",
        hunk.lines.len()
    )
}

/// The `DiffLine`s a fold row's own `(hunk, line)` pairs point to, paired
/// with their position, for yank and search.
pub(crate) fn resolve_hidden<'a>(
    file: &'a FileDiff,
    lines: &[(usize, usize)],
) -> Vec<((usize, usize), &'a DiffLine)> {
    lines
        .iter()
        .filter_map(|&(hunk, line)| Some(((hunk, line), file.hunks.get(hunk)?.lines.get(line)?)))
        .collect()
}

/// The hidden lines as a plain-text buffer holds them, so a yank over a fold
/// row copies the code it stands for.
fn hidden_copy(file: &FileDiff, lines: &[(usize, usize)]) -> String {
    resolve_hidden(file, lines)
        .iter()
        .map(|(_, diff_line)| line_row_text(diff_line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace every folded hunk of `rows` (its header and everything under it
/// up to the next header) with one `DiffRow::Fold`. An open composer's rows
/// stay on screen.
pub(crate) fn apply(
    rows: &[DiffRow],
    copy: &[RowCopy],
    model: &DiffModel,
    folded: &HashSet<String>,
) -> (Vec<DiffRow>, Vec<RowCopy>, Vec<FoldGroup>) {
    let mut out_rows = Vec::with_capacity(rows.len());
    let mut out_copy = Vec::with_capacity(copy.len());
    let mut groups = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let (Some(row), Some(text)) = (rows.get(i), copy.get(i)) else {
            break;
        };
        let folded_hunk = match *row {
            DiffRow::Hunk { file, hunk } => model
                .files
                .get(file)
                .and_then(|f| Some((f, f.hunks.get(hunk)?)))
                .filter(|(_, h)| folded.contains(&hunk_key(h)))
                .map(|(f, h)| (file, hunk, f, h)),
            _ => None,
        };
        let Some((file_index, hunk_index, file, hunk)) = folded_hunk else {
            out_rows.push(*row);
            out_copy.push(text.clone());
            i += 1;
            continue;
        };
        let end = (i + 1..rows.len())
            .find(|&j| matches!(rows.get(j), Some(DiffRow::Hunk { .. })))
            .unwrap_or(rows.len());
        let body = rows.get(i + 1..end).unwrap_or_default();
        let lines: Vec<(usize, usize)> = body
            .iter()
            .filter_map(|row| match *row {
                DiffRow::Line { hunk, line, .. } => Some((hunk, line)),
                _ => None,
            })
            .collect();
        let comments = body
            .iter()
            .filter(|row| matches!(row, DiffRow::Comment { line: 0, .. }))
            .count();
        out_rows.push(DiffRow::Fold {
            file: file_index,
            group: groups.len(),
        });
        out_copy.push(RowCopy::Text(hidden_copy(file, &lines)));
        groups.push(FoldGroup {
            keys: vec![hunk_key(hunk)],
            hunk: hunk_index,
            lines,
            label: fold_label(hunk, comments),
        });
        for j in i + 1..end {
            if let (Some(row @ DiffRow::Composer { .. }), Some(text)) = (rows.get(j), copy.get(j)) {
                out_rows.push(*row);
                out_copy.push(text.clone());
            }
        }
        i = end;
    }
    (out_rows, out_copy, groups)
}

/// The side-by-side counterpart of [`apply`]: a folded hunk's header and
/// pairs become one `SplitRow::Fold` naming that hunk.
pub(crate) fn apply_split(
    split: Vec<SplitRow>,
    file: Option<&FileDiff>,
    folded: &HashSet<String>,
) -> Vec<SplitRow> {
    let is_folded = |hunk: usize| {
        file.and_then(|f| f.hunks.get(hunk))
            .is_some_and(|h| folded.contains(&hunk_key(h)))
    };
    let mut out = Vec::with_capacity(split.len());
    let mut skipping = false;
    for row in split {
        match row {
            SplitRow::Hunk { hunk } if is_folded(hunk) => {
                skipping = true;
                out.push(SplitRow::Fold { hunk });
            }
            SplitRow::Hunk { .. } => {
                skipping = false;
                out.push(row);
            }
            SplitRow::Composer { .. } => out.push(row),
            _ if skipping => {}
            _ => out.push(row),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::app::App;
    use crate::app::diff::{DiffRow, SplitRow};
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, code_key, key};

    /// 30 numbered lines with `changed` edited, three hunks for 5, 15, 25.
    fn fixture(changed: &[usize]) -> Fixture {
        let content = |edited: &[usize]| -> String {
            (1..=30)
                .map(|i| {
                    if edited.contains(&i) {
                        format!("LINE {i}\n")
                    } else {
                        format!("line {i}\n")
                    }
                })
                .collect()
        };
        let fixture = Fixture::new();
        fixture.write("a.txt", &content(&[]));
        fixture.commit_all("base");
        fixture.write("a.txt", &content(changed));
        fixture
    }

    fn open(fixture: &Fixture) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_file("a.txt");
        let diff = app.diff.as_mut().expect("diff");
        diff.focus = crate::app::diff::Pane::Diff;
        diff.ensure_rows(&app.review);
        app
    }

    fn rows(app: &App) -> Vec<DiffRow> {
        app.diff.as_ref().expect("diff").rows().to_vec()
    }

    fn cursor_row(app: &App) -> Option<DiffRow> {
        let diff = app.diff.as_ref().expect("diff");
        diff.rows().get(diff.cursor).copied()
    }

    fn headers(app: &App) -> Vec<&'static str> {
        rows(app)
            .iter()
            .filter_map(|row| match row {
                DiffRow::Hunk { .. } => Some("open"),
                DiffRow::Fold { .. } => Some("folded"),
                _ => None,
            })
            .collect()
    }

    fn labels(app: &App) -> Vec<String> {
        let diff = app.diff.as_ref().expect("diff");
        diff.fold_groups.iter().map(|g| g.label.clone()).collect()
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            app.handle(key(c));
        }
    }

    fn message(app: &App) -> String {
        app.message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn nothing_starts_folded() {
        let app = open(&fixture(&[5, 15, 25]));
        assert_eq!(headers(&app), ["open", "open", "open"]);
    }

    #[test]
    fn za_on_any_row_of_a_hunk_folds_it_to_its_header_and_opens_it_again() {
        let mut app = open(&fixture(&[5, 15, 25]));
        press(&mut app, "]j");
        assert!(matches!(cursor_row(&app), Some(DiffRow::Line { .. })));
        press(&mut app, "za");
        assert_eq!(headers(&app), ["open", "folded", "open"]);
        assert!(
            matches!(cursor_row(&app), Some(DiffRow::Fold { .. })),
            "the cursor lands on the folded header"
        );
        assert_eq!(labels(&app), ["@@ -12,7 +12,7 @@ line 11  ⋯ 8 lines +1 -1"]);

        press(&mut app, "za");
        assert_eq!(headers(&app), ["open", "open", "open"]);
        assert!(matches!(cursor_row(&app), Some(DiffRow::Hunk { .. })));
    }

    #[test]
    fn brackets_step_over_folded_and_open_hunks_alike() {
        let mut app = open(&fixture(&[5, 15, 25]));
        press(&mut app, "zM");
        assert_eq!(headers(&app), ["folded", "folded", "folded"]);
        app.diff.as_mut().expect("diff").cursor = 0;
        press(&mut app, "]");
        let diff = app.diff.as_ref().expect("diff");
        assert_eq!(diff.cursor, 1, "{:?}", rows(&app));
        press(&mut app, "]za");
        assert_eq!(headers(&app), ["folded", "folded", "open"]);
    }

    #[test]
    fn z_m_folds_every_hunk_and_z_r_opens_them() {
        let mut app = open(&fixture(&[5, 15, 25]));
        press(&mut app, "zM");
        assert_eq!(headers(&app), ["folded", "folded", "folded"]);
        assert_eq!(message(&app), "folded every hunk");
        press(&mut app, "zR");
        assert_eq!(headers(&app), ["open", "open", "open"]);
        assert_eq!(message(&app), "opened every hunk");
        press(&mut app, "zR");
        assert_eq!(message(&app), "no hunk is folded");
    }

    #[test]
    fn a_folded_hunk_counts_the_comments_it_hides() {
        let fixture = fixture(&[5]);
        let mut app = open(&fixture);
        let anchor = diffler_core::session::Anchor {
            file: "a.txt".to_owned(),
            line: Some(5),
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        app.review.session.add_comment(anchor, "reviewer", "why?");
        let diff = app.diff.as_mut().expect("diff");
        diff.mark_rows_dirty();
        diff.ensure_rows(&app.review);
        diff.cursor = 0;
        press(&mut app, "za");
        assert_eq!(
            labels(&app),
            ["@@ -2,7 +2,7 @@ line 1  ⋯ 8 lines +1 -1 · 1 comment"]
        );
    }

    #[test]
    fn a_search_opens_the_folded_hunk_holding_its_match() {
        let mut app = open(&fixture(&[5, 15, 25]));
        press(&mut app, "zM");
        app.diff.as_mut().expect("diff").cursor = 0;
        press(&mut app, "/LINE 15");
        app.handle(code_key(KeyCode::Enter));
        assert_eq!(headers(&app), ["folded", "open", "folded"]);
    }

    #[test]
    fn a_hunk_edited_after_folding_comes_back_open() {
        let fixture = fixture(&[5, 15]);
        let mut app = open(&fixture);
        press(&mut app, "zM");
        fixture.write(
            "a.txt",
            &(1..=30)
                .map(|i| match i {
                    5 | 15 => format!("LINE {i}\n"),
                    14 => "edited 14\n".to_owned(),
                    _ => format!("line {i}\n"),
                })
                .collect::<String>(),
        );
        app.handle(crate::event::AppEvent::RepoChanged);
        app.settle_refresh();
        app.diff.as_mut().expect("diff").ensure_rows(&app.review);
        assert_eq!(headers(&app), ["folded", "open"]);
    }

    #[test]
    fn side_by_side_folds_the_same_hunks() {
        let mut app = open(&fixture(&[5, 15]));
        press(&mut app, "]za");
        app.handle(key('|'));
        let diff = app.diff.as_ref().expect("diff");
        let kinds: Vec<&str> = diff
            .split_rows
            .iter()
            .filter_map(|row| match row {
                SplitRow::Hunk { .. } => Some("open"),
                SplitRow::Fold { .. } => Some("folded"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["open", "folded"]);
    }
}
