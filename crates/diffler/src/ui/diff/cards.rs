//! Comment, summary and composer cards drawn inside the diff pane.

use std::collections::HashMap;

use diffler_core::model::DiffModel;
use diffler_core::session::{Comment, CommentStatus, Session};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::comments::{CardCtx, comments_in_order};
use super::sidebar::tree_lead;
use super::{RenderCtx, RowState, md_span, pad_line};
use crate::app::composer::{Composer, ComposerKind, ComposerLine};
use crate::app::walkthrough::summary_figure_key;
use crate::app::{CommentLine, DiffView, summary_display};
use crate::theme::Theme;
use crate::ui::diff_render::card_frame;

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    // `h` is `hue.rem_euclid(360.0) / 60.0`, always in 0.0..6.0
    #[allow(clippy::cast_sign_loss)]
    let sector = h as u32;
    let (r1, g1, b1) = match sector {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    let channel = |v: f32| {
        // the clamp keeps the value in 0.0..=255.0 before the cast
        #[allow(clippy::cast_sign_loss)]
        let byte = ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        byte
    };
    (channel(r1), channel(g1), channel(b1))
}

/// A distinct, readable colour for the `slot`th thing a screen tells apart.
pub(in crate::ui) fn slot_color(theme: &Theme, slot: usize) -> Color {
    #[allow(clippy::cast_precision_loss)] // a hue only needs to look distinct, not be exact
    let hue = (slot as f32 * HUE_STEP + 25.0).rem_euclid(360.0);
    let (r, g, b) = hsl_to_rgb(hue, 0.7, 0.55);
    let (r, g, b) = diffler_core::language::readable_on((r, g, b), crate::ui::rgb_of(theme.bg));
    Color::Rgb(r, g, b)
}

/// The golden angle, so each new hue lands far from every earlier one.
const HUE_STEP: f32 = 137.507_76;

/// Each author's first-appearance position, skipping the human and the agent
/// since they take fixed colours.
pub(super) fn author_orders<'a>(
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
pub(super) fn pane_author_orders<'a>(
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

/// The human and the agent keep fixed colours because the reader looks for
/// them first; everyone else steps the golden angle from `order`, lifted for
/// contrast against the row's own `bg`.
pub(super) fn author_color(
    theme: &Theme,
    bg: Color,
    human_author: &str,
    author: &str,
    order: usize,
) -> Color {
    if !human_author.is_empty() && author == human_author {
        return theme.accent;
    }
    if author == crate::mcp::AGENT_AUTHOR {
        return theme.purple;
    }
    #[allow(clippy::cast_precision_loss)] // a hue only needs to look distinct, not be exact
    let hue = (order as f32 * HUE_STEP).rem_euclid(360.0);
    let (r, g, b) = hsl_to_rgb(hue, 0.55, 0.6);
    let (r, g, b) = diffler_core::language::readable_on((r, g, b), crate::ui::rgb_of(bg));
    Color::Rgb(r, g, b)
}

/// The status glyph and author spans, plus the width left after them.
pub(super) fn comment_header_spans(
    cc: &CardCtx<'_>,
    comment: &diffler_core::session::Comment,
) -> (Vec<Span<'static>>, usize) {
    let &CardCtx {
        theme,
        budget,
        bg,
        depth,
        on_cursor,
        orphan,
        author_color,
        ..
    } = cc;
    // an orphan's file is gone, so we show that over its status
    let (status, colour) = match comment.status {
        _ if orphan => ("⚠", theme.error_fg),
        CommentStatus::Open => ("○", theme.warn_fg),
        CommentStatus::Replied => ("◐", theme.accent),
        CommentStatus::Resolved => ("✓", theme.added),
    };
    let spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(format!("{status} "), Style::new().fg(colour).bg(bg)),
        Span::styled(
            format!("{} ", crate::ui::elide(&comment.author, AUTHOR_MAX)),
            Style::new()
                .fg(author_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    (spans, budget.saturating_sub(used))
}

/// Caps an author's columns so a long handle leaves room for the preview.
const AUTHOR_MAX: usize = 14;

/// Replies take the comments sidebar's author colours, and every reply but the
/// reader's own indents into the other lane.
#[derive(Clone, Copy)]
struct ThreadLook<'a> {
    theme: &'a Theme,
    bg: Color,
    human: &'a str,
    orders: &'a HashMap<&'a str, usize>,
}

impl ThreadLook<'_> {
    fn color(&self, author: &str) -> Color {
        let order = self.orders.get(author).copied().unwrap_or(0);
        author_color(self.theme, self.bg, self.human, author, order)
    }

    fn lane(&self, bar: Span<'static>, author: &str) -> Vec<Span<'static>> {
        let indent = if !self.human.is_empty() && author == self.human {
            0
        } else {
            crate::app::REPLY_LANE
        };
        vec![
            bar,
            Span::styled(" ".repeat(indent), Style::new().bg(self.bg)),
            Span::styled(
                "▌ ".to_owned(),
                Style::new().fg(self.color(author)).bg(self.bg),
            ),
        ]
    }

    fn reply_spans(&self, part: &CommentLine, bar: Span<'static>) -> Vec<Span<'static>> {
        let Self { theme, bg, .. } = *self;
        let dim = Style::new().fg(theme.dim).bg(bg);
        let fg = Style::new().fg(theme.fg).bg(bg);
        match part {
            CommentLine::ReplyHead { author } => {
                let mut spans = self.lane(bar, author);
                spans.push(Span::styled(
                    author.clone(),
                    Style::new()
                        .fg(self.color(author))
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ));
                spans
            }
            CommentLine::Reply {
                author,
                spans: runs,
            } => {
                let mut spans = self.lane(bar, author);
                spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
                spans
            }
            CommentLine::FoldedReplies { count, authors } => {
                let mut spans = vec![
                    bar,
                    Span::styled("  ".to_owned(), Style::new().bg(bg)),
                    Span::styled(
                        crate::app::folded_replies_text(*count),
                        Style::new().fg(theme.accent).bg(bg),
                    ),
                    Span::styled(" · ".to_owned(), dim),
                ];
                for (at, author) in authors.iter().enumerate() {
                    if at > 0 {
                        spans.push(Span::styled(", ".to_owned(), dim));
                    }
                    spans.push(Span::styled(
                        author.clone(),
                        Style::new().fg(self.color(author)).bg(bg),
                    ));
                }
                spans
            }
            _ => vec![bar],
        }
    }
}

pub(super) fn comment_row_line(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    comment: &Comment,
    line: usize,
    outdated: bool,
    width: u16,
    state: RowState,
) -> Line<'static> {
    let theme = ctx.theme;
    let (status_label, accent) = match comment.status {
        CommentStatus::Open => ("open", theme.warn_fg),
        CommentStatus::Replied => ("replied", theme.accent),
        CommentStatus::Resolved => ("resolved", theme.dim),
    };
    let (bg, bar) = card_frame(theme, state.selected, state.focused, accent);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let unresolved = diff.unresolved_anchors.get(&comment.id).copied();
    let lines = diff
        .card_views()
        .lines_with(comment, width, Some(ctx.highlighter));
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let thread = ThreadLook {
        theme,
        bg,
        human: ctx.human_author,
        orders: &ctx.author_orders,
    };
    let spans = match part {
        CommentLine::Header => {
            let mut spans = vec![bar];
            if let Some(title) = comment.title.as_ref() {
                spans.push(Span::styled(
                    format!("{title}  "),
                    Style::new().fg(theme.accent).bg(bg),
                ));
            }
            spans.extend([
                Span::styled(
                    comment.author.clone(),
                    Style::new().fg(thread.color(&comment.author)).bg(bg),
                ),
                Span::styled(" · ".to_owned(), dim),
                Span::styled(status_label.to_owned(), Style::new().fg(accent).bg(bg)),
            ]);
            if outdated {
                spans.push(Span::styled(
                    " · outdated".to_owned(),
                    Style::new().fg(theme.warn_fg).bg(bg),
                ));
            }
            // only the anchor worker can tell a lost anchor from an unread one, so we read its map
            if unresolved.is_some() {
                spans.push(Span::styled(
                    " · stale".to_owned(),
                    Style::new().fg(theme.warn_fg).bg(bg),
                ));
            }
            spans
        }
        CommentLine::Body(runs) => {
            let mut spans = vec![bar];
            spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
            spans
        }
        CommentLine::Note(runs) => {
            let mut spans = vec![bar];
            spans.extend(runs.iter().map(|run| md_span(run, dim, theme)));
            spans
        }
        CommentLine::Figure { block, row } => {
            let Some(drawn) = ctx
                .rasters
                .get(&(comment.id.clone(), *block))
                .and_then(|lines| lines.get(*row))
            else {
                return Line::default();
            };
            return crate::ui::fill_row(drawn.clone(), bg, width);
        }
        CommentLine::ReplyGap
        | CommentLine::ReplyHead { .. }
        | CommentLine::Reply { .. }
        | CommentLine::FoldedReplies { .. } => {
            // each reply draws its own bar, so we leave the card's bar column blank
            let margin = Span::styled("  ".to_owned(), Style::new().bg(bg));
            thread.reply_spans(part, margin)
        }
        CommentLine::Footer => vec![Span::styled(
            "  ▌".to_owned(),
            Style::new().fg(accent).bg(bg),
        )],
    };
    pad_line(spans, bg, width)
}

/// The walkthrough summary's card: a "Summary" header with no status or author,
/// since nothing threads on it.
pub(super) fn summary_row_line(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    walkthrough: &diffler_core::walkthrough::Walkthrough,
    line: usize,
    width: u16,
    state: RowState,
) -> Line<'static> {
    let theme = ctx.theme;
    let (bg, bar) = card_frame(theme, state.selected, state.focused, theme.accent);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let key = summary_figure_key(&walkthrough.id);
    let blocks = crate::app::blocks_of(&diff.figures, &key);
    let summary = walkthrough.summary.as_deref().unwrap_or_default();
    let lines = summary_display(summary, width, Some(ctx.highlighter), blocks);
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let spans = match part {
        CommentLine::Header => vec![
            bar,
            Span::styled("Summary".to_owned(), Style::new().fg(theme.accent).bg(bg)),
        ],
        CommentLine::Body(runs) => {
            let mut spans = vec![bar];
            spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
            spans
        }
        CommentLine::Figure { block, row } => {
            let Some(drawn) = ctx
                .rasters
                .get(&(key.clone(), *block))
                .and_then(|lines| lines.get(*row))
            else {
                return Line::default();
            };
            return crate::ui::fill_row(drawn.clone(), bg, width);
        }
        // the summary has no anchor and no thread
        CommentLine::Note(_)
        | CommentLine::ReplyGap
        | CommentLine::ReplyHead { .. }
        | CommentLine::Reply { .. }
        | CommentLine::FoldedReplies { .. } => return Line::default(),
        CommentLine::Footer => vec![Span::styled(
            "  ▌".to_owned(),
            Style::new().fg(theme.accent).bg(bg),
        )],
    };
    pad_line(spans, bg, width)
}

/// The open composer, drawn as the card it will become, with the caret as a reversed cell.
pub(super) fn composer_row_line(
    theme: &Theme,
    composer: &Composer,
    line: usize,
    width: u16,
    selected: bool,
    focused: bool,
) -> Line<'static> {
    let accent = theme.accent;
    let (bg, bar) = card_frame(theme, selected, focused, accent);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let lines = composer.display(width);
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let spans = match part {
        ComposerLine::Header => vec![
            bar,
            Span::styled(composer_title(composer), Style::new().fg(accent).bg(bg)),
        ],
        ComposerLine::Body { text, cursor } => {
            let mut spans = vec![bar];
            let Some(column) = *cursor else {
                spans.push(Span::styled(text.clone(), fg));
                return pad_line(spans, bg, width);
            };
            let head: String = text.chars().take(column).collect();
            let under = text.chars().nth(column).unwrap_or(' ');
            let tail: String = text.chars().skip(column + 1).collect();
            spans.push(Span::styled(head, fg));
            spans.push(Span::styled(
                under.to_string(),
                fg.add_modifier(Modifier::REVERSED),
            ));
            spans.push(Span::styled(tail, fg));
            spans
        }
        ComposerLine::Footer => vec![
            Span::styled("  ▌ ".to_owned(), Style::new().fg(accent).bg(bg)),
            Span::styled("enter".to_owned(), fg),
            Span::styled(" submit · ".to_owned(), dim),
            Span::styled("a-enter".to_owned(), fg),
            Span::styled(" newline · ".to_owned(), dim),
            Span::styled("esc".to_owned(), fg),
            Span::styled(" cancel".to_owned(), dim),
        ],
    };
    pad_line(spans, bg, width)
}

fn composer_title(composer: &Composer) -> String {
    match &composer.kind {
        ComposerKind::New { anchor } => match (anchor.line, anchor.line_end) {
            (Some(line), Some(end)) => format!("comment on {}:{line}-{end}", anchor.file),
            (Some(line), None) => format!("comment on {}:{line}", anchor.file),
            _ => format!("comment on {}", anchor.file),
        },
        ComposerKind::Reply { .. } => "reply".to_owned(),
        ComposerKind::Edit { .. } => "edit comment".to_owned(),
    }
}

#[cfg(test)]
mod tests {

    use crate::app::{App, Pane};

    use crate::test_support::{key, mouse_click, render, settle_submit, standard_fixture};
    use crate::theme::Theme;
    use crate::ui::diff::test_support::{
        cursor_to_added_line, diff_app, first_two_pane_lines, open_lib_diff, walkthrough_app,
    };

    const STOP_BODY: &str = "\
The layer that wins is the last one to **set** the key.

| Layer | Wins on |
| --- | --- |
| built-in | nothing |
| cli | every key |

```mermaid
flowchart LR
  load[load defaults] --> merge[merge]
  click merge \"src/lib.rs#answer\"
```
";

    #[test]
    fn reply_authors_step_after_comment_authors() {
        let mut session = diffler_core::session::Session::default();
        let anchor = diffler_core::session::Anchor {
            file: "a.rs".to_owned(),
            line: Some(1),
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        let id = session
            .add_comment(anchor.clone(), "alice", "one")
            .id
            .clone();
        session.add_comment(anchor, "bob", "two");
        session.reply(&id, "carol", "three");
        session.reply(&id, "agent", "four");
        let orders = super::pane_author_orders(&session, None, "reviewer");
        assert_eq!(orders.get("alice"), Some(&0));
        assert_eq!(orders.get("bob"), Some(&1));
        assert_eq!(orders.get("carol"), Some(&2));
        assert_eq!(
            orders.get("agent"),
            None,
            "the agent keeps its fixed colour"
        );
    }

    #[test]
    fn author_color_is_fixed_for_human_and_agent_regardless_of_order() {
        let theme = Theme::github_dark();
        let bg = theme.bg;
        assert_eq!(
            super::author_color(&theme, bg, "reviewer", "reviewer", 3),
            theme.accent,
            "the reviewer's own comments take the fixed accent colour"
        );
        assert_eq!(
            super::author_color(&theme, bg, "reviewer", crate::mcp::AGENT_AUTHOR, 7),
            theme.purple,
            "the agent's comments take the fixed purple colour"
        );
        assert_eq!(
            super::author_color(&theme, bg, "reviewer", "alice", 0),
            super::author_color(&theme, bg, "reviewer", "alice", 0),
            "the same order always reads the same colour"
        );
    }

    #[test]
    fn several_authors_get_distinct_hues_and_never_the_fixed_two() {
        let theme = Theme::github_dark();
        let bg = theme.bg;
        let authors = ["alice", "bob", "carol", "dave", "erin", "frank"];
        let orders = super::author_orders(authors.into_iter(), "reviewer");
        let colours: Vec<ratatui::style::Color> = authors
            .iter()
            .map(|author| {
                let order = *orders.get(author).expect("every author was seeded");
                super::author_color(&theme, bg, "reviewer", author, order)
            })
            .collect();
        for (i, colour) in colours.iter().enumerate() {
            assert_ne!(
                *colour, theme.accent,
                "{}: never the human's colour",
                authors[i]
            );
            assert_ne!(
                *colour, theme.purple,
                "{}: never the agent's colour",
                authors[i]
            );
            for (j, other) in colours.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    colour, other,
                    "{} and {} collide: {colours:?}",
                    authors[i], authors[j]
                );
            }
        }
    }

    #[test]
    fn a_stop_card_renders_its_body_table_and_figure_under_the_span() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[
                ("What changed", None, "the overview, pointing at nothing"),
                ("The answer", Some("src/lib.rs#answer"), STOP_BODY),
            ],
        );
        app.handle(key('j'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn the_current_anchorless_stop_shows_its_card_alone() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[
                ("What changed", None, "the overview, pointing at nothing"),
                ("The answer", Some("src/lib.rs#answer"), "why 42"),
            ],
        );
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_stop_in_a_large_file_windows_to_its_span() {
        let fixture = crate::test_support::big_file_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The change", Some("big.txt:100"), "why line 100 changed")],
        );
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// A slide shows only its region, so banding it would paint every code row.
    #[test]
    fn a_slide_never_bands_its_whole_region_while_a_file_layout_bands_a_span() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let band = crate::theme::blend(app.theme.bg, app.theme.accent, 25);
        let banded = |app: &mut App| {
            let terminal = render(app);
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .filter(|&y| {
                    let cell = &buffer[(buffer.area.width / 2, y)];
                    cell.style().bg == Some(band)
                })
                .count()
        };
        assert!(
            app.diff.as_ref().expect("diff view").referenced.is_none(),
            "a slide bands nothing: it already shows the stop's region"
        );
        assert_eq!(banded(&mut app), 0, "so no row is banded");

        let source = app.active_review_source();
        let note = app
            .review
            .session_for_mut(&source)
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: Some(3),
                    on_old_side: false,
                    line_text: None,
                },
                "agent",
                "two lines inside the region",
            )
            .id
            .clone();
        {
            let diff = app.diff.as_mut().expect("diff view");
            diff.layout = crate::config::FileLayout::Tree;
            diff.slide = None;
            diff.invalidate();
        }
        app.focus_comment(&note);
        assert!(
            banded(&mut app) >= 1,
            "outside the slide the file is all on screen, so the jumped-to span is banded"
        );
    }

    /// The card's rows shift the code, so a band keyed on row numbers would colour the whole slide.
    #[test]
    fn a_comment_added_inside_a_slide_leaves_it_unbanded() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let band = crate::theme::blend(app.theme.bg, app.theme.accent, 25);

        let source = app.active_review_source();
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "why not 41?",
        );
        app.diff.as_mut().expect("diff view").invalidate();
        app.seat_stop(0);

        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let banded = (0..buffer.area.height)
            .filter(|&y| buffer[(buffer.area.width / 2, y)].style().bg == Some(band))
            .count();
        assert_eq!(banded, 0, "the slide stays plain with a comment in it");
    }

    #[test]
    fn a_slide_with_two_comments_renders_both_cards() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let source = app.active_review_source();
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "why 42 and not 41?",
        );
        app.diff.as_mut().expect("diff view").invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_mermaid_fence_in_a_human_comment_draws_as_a_figure() {
        let (_fixture, mut app) = diff_app();
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                "does it go this way?\n\n```mermaid\nflowchart LR\n  a[read] --> b[merge]\n```\n",
            )
            .id
            .clone();
        app.diff.as_mut().expect("diff view").invalidate();
        app.focus_comment(&id);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// `GraphView::set_model` selects a node by default, and a card figure is a static picture.
    #[test]
    fn a_figures_selection_is_cleared_so_no_node_reverses_in_the_card() {
        let (_fixture, mut app) = diff_app();
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                "does it go this way?\n\n```mermaid\nflowchart LR\n  a[read] --> b[merge]\n```\n",
            )
            .id
            .clone();
        app.diff.as_mut().expect("diff view").invalidate();
        app.focus_comment(&id);
        let figure_rows = {
            let diff = app.diff.as_ref().expect("diff view");
            let cached = diff.figures.get(&id).expect("the figure is cached");
            cached
                .blocks
                .iter()
                .find_map(|block| match block {
                    crate::app::walkthrough::Block::Figure(figure) => Some(figure.rows()),
                    crate::app::walkthrough::Block::Prose(_) => None,
                })
                .expect("a figure block")
        };
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let header_row = (area.y..area.y + area.height)
            .find(|&y| {
                let line: String = (area.x..area.x + area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                line.contains("figure 1")
            })
            .expect("the figure's header row is on screen");
        let last_row =
            (header_row + u16::try_from(figure_rows).unwrap_or(0)).min(area.y + area.height);
        let reversed = (header_row..last_row)
            .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                buffer[(x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
            .count();
        assert_eq!(reversed, 0, "no node in the figure should render reversed");
    }

    #[test]
    fn the_summary_slide_renders_one_card_and_no_diff_rows() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        crate::test_support::set_walkthrough_summary(&mut app, "w1", "the shape of the change");
        app.seat_summary();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn fenced_code_block_in_a_comment_is_syntax_highlighted() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "try this:\n```rust\nfn answer() -> u32 { 42 }\n```",
        );
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn markdown_in_a_comment_body_renders_styled() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "call `foo()` then make it **bold**",
        );
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        let buffer = format!("{:?}", terminal.backend().buffer());
        assert!(
            buffer.contains("BOLD"),
            "the **bold** run renders with the bold modifier"
        );
        assert!(
            buffer.contains(&format!("{:?}", app.theme.accent)),
            "the `code` run renders in the accent color"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn a_click_outside_the_comment_box_keeps_the_draft_for_reopening() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let (x, y0, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(x, y0));
        for c in "half a thought".chars() {
            app.handle(key(c));
        }
        let kind = app
            .diff
            .as_ref()
            .unwrap()
            .composer
            .as_ref()
            .unwrap()
            .kind
            .clone();
        render(&mut app);
        let sidebar = app.diff.as_ref().unwrap().sidebar;
        app.handle(mouse_click(sidebar.x + 1, sidebar.y));
        assert!(!app.composer_open(), "the click closed the box");
        app.open_composer(kind, String::new());
        let reopened = app.diff.as_ref().unwrap().composer.as_ref().unwrap();
        assert_eq!(reopened.buffer, "half a thought", "the draft came back");
    }

    #[test]
    fn a_kept_edit_draft_comes_back_over_the_original_text() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let kind = crate::app::composer::ComposerKind::Edit {
            comment_id: "c1".to_owned(),
        };
        let edited = crate::app::composer::Composer::new(kind.clone(), "edited".to_owned());
        let other = crate::app::composer::Composer::new(
            crate::app::composer::ComposerKind::Reply {
                comment_id: "c2".to_owned(),
            },
            "a reply".to_owned(),
        );
        app.diff.as_mut().unwrap().parked_drafts = vec![edited, other];
        app.open_composer(kind, "original".to_owned());
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.composer.as_ref().unwrap().buffer, "edited");
        assert_eq!(diff.parked_drafts.len(), 1, "the other draft is still kept");
    }

    #[test]
    fn a_long_thread_folds_and_splits_into_lanes() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let human = app.author.clone();
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("pub fn answer() -> u32 {".to_owned()),
                },
                &human,
                "why full jitter here?",
            )
            .id
            .clone();
        let session = &mut app.review.session;
        session.reply(&id, "agent", "it spreads the restarts after a deploy");
        session.reply(&id, &human, "fair, but cap it");
        session.reply(&id, &human, "and the docs?");
        session.reply(&id, "agent", "capped at two seconds\n\nadded a test for attempt zero\n\nupdated the docs\n\nand the changelog\n\nand the bench");
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comment_blocks_render_open_and_replied_threads() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why 42?".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        let answered = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("pub fn answer() -> u32 {".to_owned()),
                },
                "reviewer",
                "rename this?",
            )
            .id
            .clone();
        app.review
            .session
            .reply(&answered, "agent", "kept for api compat");
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_markdown_table_in_a_comment_renders_as_columns() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: Some("    42".to_owned()),
            },
            "reviewer",
            "| Between | Ordering | What a failure costs |\n\
             |---|---|---|\n\
             | Two customers | Parallel | Nothing, one lane throwing leaves the others |\n\
             | Postgres, then the sinks | Strictly ordered | No sink hears anything this run |",
        );
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn long_comment_text_wraps_to_the_pane_width() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("    42".to_owned()),
                },
                "reviewer",
                "a negative or far too large answer breaks the callers downstream, \
                 so this needs a clamp before it ships to anyone",
            )
            .id
            .clone();
        app.review.session.reply(
            &id,
            "agent",
            "agreed, clamping to the documented range and adding a regression \
             test so the next refactor cannot silently drop it",
        );
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn a_card_keeps_its_last_line_beside_the_comments_pane() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: Some("    42".to_owned()),
            },
            "reviewer",
            &format!("{} END-MARKER", "word ".repeat(40)),
        );
        app.diff.as_mut().unwrap().invalidate();
        app.handle(key('C'));
        let text = render(&mut app).backend().to_string();
        assert!(text.contains("END-MARKER"), "{text}");
    }

    #[test]
    fn stray_clicks_never_lose_a_draft() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "half written".chars() {
            app.handle(key(c));
        }
        render(&mut app);
        // a double-click that opens a composer on another line, then a click
        // on the file sidebar
        let (x, y0, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(1, 3));
        let diff = app.diff.as_ref().unwrap();
        let kept = diff
            .composer
            .iter()
            .chain(&diff.parked_drafts)
            .any(|draft| draft.buffer == "half written");
        assert!(kept, "the draft is open or kept aside for reopening");
    }

    #[test]
    fn a_file_level_composer_renders_at_the_top_of_the_pane() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.diff.as_mut().unwrap().focus = Pane::List;
        app.handle(key('c'));
        for c in "applies to the whole file".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_reply_composer_renders_under_the_thread_it_answers() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why?".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        settle_submit(&mut app);
        let comment_row = app
            .diff
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .position(|row| matches!(row, crate::app::DiffRow::Comment { line: 0, .. }))
            .expect("the comment header");
        app.diff.as_mut().unwrap().cursor = comment_row;
        app.handle(key('r'));
        for c in "because".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn the_composer_renders_under_the_line_it_comments_on() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }
}
