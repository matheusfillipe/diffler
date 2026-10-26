//! Comments sidebar of the diff screen.

use std::collections::HashMap;

use diffler_core::model::DiffModel;
use diffler_core::session::Session;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use super::cards::{CardCtx, CardSearch, author_color, comment_header_spans};
use super::{
    HeaderCtx, RenderCtx, group_header_line, pad_line, pane_heading, search_ranges, sidebar_bg,
    sidebar_row_bg,
};
use crate::app::{
    CommentFacts, CommentGrouping, CommentPaneRow, DiffView, Pane, group_comment_rows,
};
use crate::theme::Theme;

/// Each author's first-appearance position, skipping the human and the agent
/// since they take fixed colours.
pub(in crate::ui) fn author_orders<'a>(
    authors: impl Iterator<Item = &'a str>,
    human_author: &str,
) -> HashMap<&'a str, usize> {
    let mut orders = HashMap::new();
    for author in authors {
        if author == human_author || author == crate::mcp::AGENT_AUTHOR {
            continue;
        }
        let next = orders.len();
        orders.entry(author).or_insert(next);
    }
    orders
}

/// Comment authors in sidebar order first, then anyone who only replied.
pub(in crate::ui) fn pane_author_orders<'a>(
    session: &'a Session,
    model: Option<&DiffModel>,
    human_author: &str,
) -> HashMap<&'a str, usize> {
    let roots = comments_in_order(session, model);
    let repliers = session
        .comments
        .iter()
        .flat_map(|comment| comment.replies.iter().map(|reply| reply.author.as_str()));
    author_orders(
        roots
            .iter()
            .map(|(comment, _)| comment.author.as_str())
            .chain(repliers),
        human_author,
    )
}

/// Built from the same ordering `App::comment_rows` groups, so a click or a key
/// and the drawn rows agree on which row is which.
fn comment_pane_rows(
    ordered: &[(&diffler_core::session::Comment, bool)],
    diff: &DiffView,
) -> Vec<CommentPaneRow> {
    let facts: Vec<CommentFacts> = ordered
        .iter()
        .map(|(comment, orphan)| CommentFacts {
            id: comment.id.clone(),
            file: comment.anchor.file.clone(),
            author: comment.author.clone(),
            status: comment.status,
            orphan: *orphan,
        })
        .collect();
    group_comment_rows(&facts, diff.comment_grouping, &diff.comment_folds)
}

fn comment_group_header_line(
    theme: &Theme,
    bg: Color,
    width: u16,
    on_cursor: bool,
    label: &str,
    count: usize,
    folded: bool,
) -> Line<'static> {
    group_header_line(
        HeaderCtx {
            theme,
            bg,
            width,
            on_cursor,
        },
        label,
        count,
        folded,
        Vec::new(),
    )
}

/// Right pane: the review's comments under the pane's own grouping. The
/// selection drives the diff cursor, so the pane's verbs act on the highlighted card.
pub(super) fn draw_comments(
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
    diff: &mut DiffView,
) {
    let theme = ctx.theme;
    let focused = diff.focus == Pane::Comments;
    let surface = sidebar_bg(theme);
    frame.render_widget(Block::new().style(Style::new().bg(surface)), area);
    let [heading, inner] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let ordered = ordered_comments(ctx, diff);
    let rows = comment_pane_rows(&ordered, diff);
    frame.render_widget(
        Paragraph::new(pane_heading(
            theme,
            &format!(
                "Comments ({}) · {}",
                ordered.len(),
                diff.comment_grouping.label()
            ),
            focused,
            surface,
        )),
        heading,
    );
    diff.comments_rect = inner;

    let (mut lines, owners, cursor_line) = comment_pane_lines(ctx, diff, &rows, inner, focused);
    diff.comment_lines = owners;
    if lines.is_empty() {
        let dim = Style::new().fg(theme.dim).bg(surface);
        lines.push(Line::styled(" no comments yet", dim));
        lines.push(Line::styled(" c to add one", dim));
    }

    let height = inner.height.max(1) as usize;
    diff.comments_scroll =
        crate::ui::scroll_to_cursor(cursor_line, diff.comments_scroll, height, lines.len());
    let shown: Vec<Line<'static>> = lines
        .into_iter()
        .skip(diff.comments_scroll)
        .take(height)
        .collect();
    frame.render_widget(Paragraph::new(shown), inner);
    ctx.avatars.draw(frame, inner, diff.comments_scroll);
}

/// The pane's rendered lines, each line's owning row (so a click on a wrapped
/// line selects its comment), and the cursor's line index.
fn comment_pane_lines(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    rows: &[CommentPaneRow],
    inner: Rect,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<Option<usize>>, usize) {
    let theme = ctx.theme;
    let search = ctx.search;
    let surface = sidebar_bg(theme);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut owners: Vec<Option<usize>> = Vec::new();
    let budget = (inner.width as usize).saturating_sub(2).max(1);
    let item_depth = usize::from(diff.comment_grouping != CommentGrouping::Flat);
    let orders = &ctx.author_orders;
    let mut cursor_line = 0usize;
    for (row_index, row) in rows.iter().enumerate() {
        let on_cursor = row_index == diff.comments_cursor;
        if on_cursor {
            cursor_line = lines.len();
        }
        let bg = sidebar_row_bg(theme, on_cursor, focused);
        match row {
            CommentPaneRow::Header {
                label,
                count,
                folded,
                ..
            } => {
                lines.push(comment_group_header_line(
                    theme,
                    bg,
                    inner.width,
                    on_cursor,
                    label,
                    *count,
                    *folded,
                ));
                owners.push(Some(row_index));
            }
            CommentPaneRow::Item { id, orphan } => {
                let Some(comment) = ctx.session.comment(id) else {
                    continue;
                };
                let order = orders.get(comment.author.as_str()).copied().unwrap_or(0);
                let card = CardCtx {
                    theme,
                    budget,
                    bg,
                    width: inner.width,
                    depth: item_depth,
                    on_cursor,
                    orphan: *orphan,
                    author_color: author_color(theme, bg, ctx.human_author, &comment.author, order),
                    search: search.filter(|_| focused).map(|search| CardSearch {
                        query: search.query(),
                        current: search.current_row() == Some(row_index),
                    }),
                    avatars: &ctx.avatars,
                };
                // the cursor's comment stays one line too: the diff pane shows it in
                // full, and a row that grew under the cursor would shift every row below
                lines.push(comment_summary_line(&card, comment));
                ctx.avatars.claim(lines.len() - 1, true);
                owners.push(Some(row_index));
                let last_in_group =
                    !matches!(rows.get(row_index + 1), Some(CommentPaneRow::Item { .. }));
                if last_in_group {
                    lines.push(Line::styled(
                        " ".repeat(inner.width as usize),
                        Style::new().bg(surface),
                    ));
                    owners.push(None);
                }
            }
        }
    }
    (lines, owners, cursor_line)
}

/// Comments by diff file order then line, matching `App::comment_order`, each
/// paired with whether its file left the diff (orphaned, ranked last).
fn ordered_comments<'a>(
    ctx: &'a RenderCtx<'_>,
    diff: &DiffView,
) -> Vec<(&'a diffler_core::session::Comment, bool)> {
    comments_in_order(ctx.session, diff.commit_model.as_ref().or(ctx.review_model))
}

pub(super) fn comments_in_order<'a>(
    session: &'a Session,
    model: Option<&DiffModel>,
) -> Vec<(&'a diffler_core::session::Comment, bool)> {
    let rank = |path: &str| {
        model
            .and_then(|model| model.files.iter().position(|file| file.path == path))
            .unwrap_or(usize::MAX)
    };
    let mut ordered: Vec<(&diffler_core::session::Comment, usize)> = session
        .comments
        .iter()
        .map(|comment| (comment, rank(&comment.anchor.file)))
        .collect();
    ordered.sort_by_key(|(comment, rank)| (*rank, comment.anchor.line.unwrap_or(0)));
    ordered
        .into_iter()
        .map(|(comment, rank)| (comment, rank == usize::MAX))
        .collect()
}

/// The title, else the body's first line.
fn comment_preview(comment: &diffler_core::session::Comment) -> String {
    comment.title.clone().unwrap_or_else(|| {
        comment
            .body
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}

fn comment_summary_line(
    cc: &CardCtx<'_>,
    comment: &diffler_core::session::Comment,
) -> Line<'static> {
    let (mut spans, rest_budget) = comment_header_spans(cc, comment);
    let preview = crate::ui::elide(&comment_preview(comment), rest_budget);
    spans.extend(crate::ui::highlight_spans(
        &preview,
        Style::new().fg(cc.theme.dim).bg(cc.bg),
        &search_ranges(cc.search, &preview),
        cc.theme,
    ));
    pad_line(spans, cc.bg, cc.width)
}

#[cfg(test)]
mod tests {

    use crate::app::{App, Pane};

    use crate::test_support::{key, render};

    use crate::ui::diff::test_support::{diff_app, open_lib_diff};

    fn app_with_grouped_comments() -> (crate::test_support::Fixture, App) {
        let (fixture, mut app) = diff_app();
        let source = app.active_review_source();
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
                on_file: false,
            },
            "reviewer",
            "why 42?",
        );
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "todo.md".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
                on_file: false,
            },
            "alice",
            "needs a date",
        );
        let resolved = app
            .review
            .session_for_mut(&source)
            .add_comment(
                diffler_core::session::Anchor {
                    file: "todo.md".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                    on_file: false,
                },
                "alice",
                "already fixed",
            )
            .id
            .clone();
        app.review.session_for_mut(&source).resolve(&resolved);
        (fixture, app)
    }

    fn busy_anchor(file: &str, line: u32) -> diffler_core::session::Anchor {
        diffler_core::session::Anchor {
            file: file.to_owned(),
            line: Some(line),
            line_end: None,
            on_old_side: false,
            line_text: None,
            on_file: false,
        }
    }

    #[test]
    fn renders_the_comments_sidebar() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('l'));
        app.handle(key('j'));
        app.handle(key('c'));
        for c in "this line reads oddly and should wrap across the column".chars() {
            app.handle(key(c));
        }
        app.handle(crate::test_support::code_key(
            crossterm::event::KeyCode::Enter,
        ));
        app.handle(key('C'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// A stop's body stays in its card; ten stops of four bullets would bury the list.
    #[test]
    fn the_comments_sidebar_lists_a_titled_comment_by_its_title_alone() {
        let (_fixture, mut app) = diff_app();
        let source = app.active_review_source();
        let file = app
            .diff
            .as_ref()
            .and_then(|diff| {
                diff.model(&app.review)
                    .files
                    .first()
                    .map(|f| f.path.clone())
            })
            .expect("a file in the diff");
        let anchor = diffler_core::session::Anchor {
            file,
            line: None,
            line_end: None,
            on_old_side: false,
            line_text: None,
            on_file: false,
        };
        let id = app
            .review
            .session_for_mut(&source)
            .add_comment(
                anchor,
                "agent",
                "We refuse a default here because it hides a missing list.",
            )
            .id
            .clone();
        if let Some(comment) = app
            .review
            .session_for_mut(&source)
            .comments
            .iter_mut()
            .find(|comment| comment.id == id)
        {
            comment.title = Some("Missing staff list stops the run".to_owned());
        }
        app.handle(key('C'));
        let screen = render(&mut app).backend().to_string();
        // we slice by column since box-drawing glyphs make a byte slice land mid-character
        let pane_start = screen
            .lines()
            .find_map(|row| row.find("Comments (").map(|at| row[..at].chars().count()))
            .expect("the comments pane heading");
        let pane: String = screen
            .lines()
            .map(|row| row.chars().skip(pane_start).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(pane.contains("Missing staff list"), "{pane}");
        assert!(
            !pane.contains("We refuse a default here"),
            "the body belongs in the card, not the list: {pane}"
        );
    }

    #[test]
    fn the_comments_sidebar_opens_empty_on_a_review_without_comments() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('C'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comments_pane_flat_lists_every_comment_with_no_headers() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comments_pane_grouped_by_file_renders_a_header_per_file() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        app.handle(key('t')); // flat -> file
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comments_pane_grouped_by_author_renders_a_header_per_author() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        app.handle(key('t')); // file
        app.handle(key('t')); // author
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comments_pane_grouped_by_status_starts_resolved_folded() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        app.handle(key('t')); // file
        app.handle(key('t')); // author
        app.handle(key('t')); // status
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_busy_comments_pane_stays_dense_with_a_long_name_an_elided_body_and_two_folds() {
        let (_fixture, mut app) = diff_app();
        let source = app.active_review_source();
        let reviewer_id = app
            .review
            .session_for_mut(&source)
            .add_comment(busy_anchor("src/lib.rs", 1), "reviewer", "why 42?")
            .id
            .clone();
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("src/lib.rs", 2),
            "alexandra-the-longform-reviewer",
            "looks fine",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("src/lib.rs", 3),
            "dave",
            "This preview has to run long enough that the collapsed row has \
             no choice but to elide it with an ellipsis at the end.",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("ci.yml", 1),
            "bob",
            "looks fine",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("ci.yml", 1),
            "carol",
            "ship it",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("todo.md", 1),
            "agent",
            "flagged for follow-up",
        );
        app.handle(key('C'));
        app.handle(key('t')); // flat -> file
        let diff = app.diff.as_mut().expect("diff");
        diff.comment_folds.insert("file:ci.yml".to_owned());
        diff.comment_folds.insert("file:todo.md".to_owned());
        let open_row = app
            .comment_rows()
            .iter()
            .position(
                |row| matches!(row, super::CommentPaneRow::Item { id, .. } if *id == reviewer_id),
            )
            .expect("the reviewer's comment has a row under src/lib.rs");
        app.comments_to(open_row);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn the_comments_cursor_band_follows_focus_like_the_file_rows() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('l'));
        app.handle(key('j'));
        app.handle(key('c'));
        for c in "a note".chars() {
            app.handle(key(c));
        }
        app.handle(crate::test_support::code_key(
            crossterm::event::KeyCode::Enter,
        ));
        app.handle(key('C'));

        let band_of = |app: &mut App| {
            let terminal = render(app);
            let buffer = terminal.backend().buffer();
            let rect = app.diff.as_ref().expect("diff").comments_rect;
            buffer[(rect.x, rect.y)].style().bg
        };
        let focused = band_of(&mut app);
        // h leaves the comments pane for the diff, keeping the same selection
        app.handle(key('h'));
        let unfocused = band_of(&mut app);

        let theme = &app.theme;
        assert_eq!(
            focused,
            Some(crate::ui::diff::sidebar_row_bg(theme, true, true))
        );
        assert_eq!(
            unfocused,
            Some(crate::ui::diff::sidebar_row_bg(theme, true, false))
        );
        assert_ne!(focused, unfocused, "the band weakens with focus lost");
    }

    #[test]
    fn searching_the_comments_sidebar_lights_the_matches() {
        let (_fixture, mut app) = diff_app();
        for body in ["the widget leaks", "another widget here"] {
            app.review.session.add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                    on_file: false,
                },
                "reviewer",
                body,
            );
        }
        app.handle(key('C'));
        app.handle(key('/'));
        for c in "widget".chars() {
            app.handle(key(c));
        }

        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let rect = app.diff.as_ref().expect("diff").comments_rect;
        let painted = |bg| {
            (rect.y..rect.y + rect.height)
                .flat_map(|y| (rect.x..rect.x + rect.width).map(move |x| (x, y)))
                .filter(|at| buffer[*at].style().bg == Some(bg))
                .count()
        };
        assert_eq!(
            painted(app.theme.search_current),
            "widget".len(),
            "the active match is lit stronger"
        );
        assert_eq!(
            painted(app.theme.search),
            "widget".len(),
            "and the other match is lit too"
        );
    }

    #[test]
    fn t_regroups_only_the_list_that_has_the_keyboard() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let before = app.diff.as_ref().unwrap().layout;
        app.diff.as_mut().unwrap().focus = Pane::Diff;
        app.handle(key('t'));
        assert_eq!(
            app.diff.as_ref().unwrap().layout,
            before,
            "the diff pane leaves it"
        );
        app.diff.as_mut().unwrap().focus = Pane::List;
        app.handle(key('t'));
        assert_ne!(
            app.diff.as_ref().unwrap().layout,
            before,
            "the file list regroups"
        );
    }
}
