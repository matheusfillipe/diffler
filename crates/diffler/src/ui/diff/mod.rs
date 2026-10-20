//! Diff/review screen: file sidebar, diff pane, and the comments or references sidebar.

pub(super) mod avatars;
pub(super) mod cards;
pub(super) mod comments;
mod pane;
mod references;
mod sidebar;
#[cfg(test)]
mod test_support;

use std::collections::HashMap;

use diffler_core::model::DiffModel;
use diffler_core::session::Session;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use self::avatars::AvatarCtx;
use self::cards::CardSearch;
use self::comments::{draw_comments, pane_author_orders};
use self::pane::{FigureRaster, draw_pane, patch_figure_copy_text, rasterize_figures};
use self::references::draw_references;
use self::sidebar::draw_sidebar;
use crate::app::{App, DiffView};
use crate::keymap::Action;
use crate::search::Search;
use crate::theme::Theme;
use crate::ui::Hint;
use crate::ui::diff_render::cursor_band;
use crate::ui::status_bar;

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Comment], "add comment"),
    Hint::Leaf(&[Action::Reply], "reply"),
    Hint::Leaf(&[Action::MarkViewed], "mark viewed"),
    Hint::Leaf(&[Action::CommentsOverview], "see comment list"),
    Hint::Leaf(&[Action::Help], "help"),
];

fn sidebar_width(total: u16) -> u16 {
    (total / 4).clamp(28, 44).min(total)
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body, bar) = super::screen_chrome(frame, app, HINTS);

    let open_figure_hint = app.active_keymap().chord_for(Action::OpenFigureGraph);
    app.queue_enrich_selected();
    // comment bodies are parsed to this width, so we set it before anything reads them
    let right = app.diff.as_ref().map_or(0, |diff| {
        comments_width(body.width, diff.comments_open || diff.refs_visible())
    });
    let pane_width = pane_width(body.width, right);
    if let Some(diff) = app.diff.as_mut() {
        diff.set_wrap_width(pane_width);
    }
    app.ensure_walkthrough_view();

    // we borrow fields disjointly: the diff view mutates while theme and review stay read-only
    let theme = &app.theme;
    let review = &app.review;
    let search = app.search.as_ref();
    let highlighter = app.highlighter.as_ref();
    let human_author = app.author.as_str();
    let algorithm = app.config.diff.algorithm;
    let avatar_states = app.avatars_shown().then_some(&app.avatars);
    let covered = app.modal.is_some() || app.which_key_panel().is_some();
    if let Some(diff) = app.diff.as_mut() {
        diff.ensure_rows(review);
        // we clone the source so the session's borrow stays off the view, which the rasteriser mutates
        let source = diff.source.clone();
        let session = review.session_for(&source);
        // rasterising needs the graph mutably while the pane's loop borrows the model, so we do it up front
        let rasters = rasterize_figures(diff, theme, pane_width, open_figure_hint.as_deref());
        patch_figure_copy_text(diff, &rasters);
        // context files stay off this model because `diff` itself goes into `draw_body`,
        // so each renderer that needs them reads `diff.context_files`
        let review_model = (diff.commit_model.is_none()).then(|| review.model());
        let author_orders = pane_author_orders(
            session,
            diff.commit_model.as_ref().or(review_model),
            human_author,
        );
        let ctx = RenderCtx {
            theme,
            session,
            review_model,
            search,
            highlighter,
            human_author,
            rasters: &rasters,
            algorithm,
            author_orders,
            avatars: AvatarCtx::new(avatar_states, human_author, covered),
        };
        draw_body(frame, body, &ctx, diff);
    }
    // we queue after drawing so the worker builds the picture for the size the pane just drew
    app.queue_image_preview();
    app.queue_avatars();

    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

struct RenderCtx<'a> {
    theme: &'a Theme,
    session: &'a Session,
    review_model: Option<&'a DiffModel>,
    search: Option<&'a Search>,
    highlighter: &'a diffler_core::highlight::Highlighter,
    human_author: &'a str,
    rasters: &'a FigureRaster,
    algorithm: diffler_core::diffalgo::DiffAlgorithm,
    author_orders: HashMap<&'a str, usize>,
    avatars: AvatarCtx<'a>,
}

#[derive(Clone, Copy)]
struct RowState {
    selected: bool,
    focused: bool,
}

/// Empty columns between panes; the gap is the divider.
const PANE_GAP: u16 = 1;

fn draw_body(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let width = (sidebar_width(area.width) + 1).min(area.width);
    let comments = comments_width(area.width, diff.comments_open || diff.refs_visible());
    let [list_area, _gap, pane_area, _right_gap, comments_area] = Layout::horizontal([
        Constraint::Length(width),
        Constraint::Length(PANE_GAP),
        Constraint::Min(0),
        Constraint::Length(if comments == 0 { 0 } else { PANE_GAP }),
        Constraint::Length(comments),
    ])
    .areas(area);
    draw_sidebar(frame, list_area, ctx, diff);
    draw_pane(frame, pane_area, ctx, diff);
    if comments > 0 && diff.refs_visible() {
        draw_references(frame, comments_area, ctx, diff);
    } else if comments > 0 {
        draw_comments(frame, comments_area, ctx, diff);
    }
}

/// The diff pane's width once the file sidebar and a `right` pane of that
/// width take their columns, matching `draw_body`'s layout.
fn pane_width(total: u16, right: u16) -> u16 {
    let right = if right == 0 { 0 } else { right + PANE_GAP };
    total.saturating_sub(sidebar_width(total) + 1 + PANE_GAP + right)
}

fn comments_width(total: u16, open: bool) -> u16 {
    if !open {
        return 0;
    }
    sidebar_width(total).min(total / 3)
}

#[derive(Clone, Copy)]
struct HeaderCtx<'a> {
    theme: &'a Theme,
    bg: Color,
    width: u16,
    on_cursor: bool,
}

/// A group header row with an optional right-aligned tail (the file sidebar
/// passes a diffstat).
fn group_header_line(
    hc: HeaderCtx<'_>,
    label: &str,
    count: usize,
    folded: bool,
    tail: Vec<Span<'static>>,
) -> Line<'static> {
    let HeaderCtx {
        theme,
        bg,
        width,
        on_cursor,
    } = hc;
    let arrow = if folded { "▸ " } else { "▾ " };
    let label_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let mut spans = vec![
        tree_lead(theme, 0, bg, on_cursor),
        Span::styled(arrow.to_owned(), dim),
        Span::styled(label.to_owned(), label_style),
        Span::styled(format!(" ({count})"), dim),
    ];
    push_right(&mut spans, tail, width, bg);
    pad_line(spans, bg, width)
}

/// The sidebar differs from the diff pane's surface, which is what separates them with no border.
fn sidebar_bg(theme: &Theme) -> Color {
    theme.bg
}

fn pane_heading(theme: &Theme, title: &str, focused: bool, bg: Color) -> Line<'static> {
    Line::styled(
        format!(" {title}"),
        Style::new()
            .fg(if focused { theme.accent } else { theme.dim })
            .bg(bg),
    )
}

/// Right-align `tail` when it fits; a row too narrow drops it and keeps its name.
fn push_right(spans: &mut Vec<Span<'static>>, tail: Vec<Span<'static>>, width: u16, bg: Color) {
    if tail.is_empty() {
        return;
    }
    let tail_width: usize = tail.iter().map(Span::width).sum();
    let used: usize = spans.iter().map(Span::width).sum();
    let Some(pad) = super::right_align_pad(used, tail_width, width as usize) else {
        return;
    };
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::new().bg(bg)));
    }
    spans.extend(tail);
}

/// Clip styled `spans` to `room` chars, keeping each span's style. `front`
/// elides from the left, otherwise from the right.
fn clip_spans(
    spans: Vec<Span<'static>>,
    room: usize,
    front: bool,
    ellipsis_style: Style,
) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if room == 0 {
        return Vec::new();
    }
    if total <= room {
        return spans;
    }
    let keep = room - 1;
    if front {
        let skip = total - keep;
        let mut out = vec![Span::styled("…".to_owned(), ellipsis_style)];
        let mut seen = 0;
        for span in spans {
            let len = span.content.chars().count();
            let start = seen;
            seen += len;
            if seen <= skip {
                continue;
            }
            let drop_here = skip.saturating_sub(start);
            let kept: String = span.content.chars().skip(drop_here).collect();
            out.push(Span::styled(kept, span.style));
        }
        out
    } else {
        let mut out = Vec::new();
        let mut taken = 0;
        for span in spans {
            if taken >= keep {
                break;
            }
            let len = span.content.chars().count();
            let take = (keep - taken).min(len);
            let kept: String = span.content.chars().take(take).collect();
            out.push(Span::styled(kept, span.style));
            taken += len;
        }
        out.push(Span::styled("…".to_owned(), ellipsis_style));
        out
    }
}

fn sidebar_row_bg(theme: &Theme, on_cursor: bool, focused: bool) -> Color {
    if on_cursor {
        cursor_band(theme, sidebar_bg(theme), focused)
    } else {
        sidebar_bg(theme)
    }
}

/// The cursor `▌` marker plus the tree indent for `depth`.
fn tree_lead(theme: &Theme, depth: usize, bg: Color, on_cursor: bool) -> Span<'static> {
    // some themes tint the cursor band faintly, so we add a bar
    let marker = if on_cursor { "▌" } else { " " };
    Span::styled(
        format!("{marker}{}", " ".repeat(depth * 2)),
        Style::new().fg(theme.accent).bg(bg),
    )
}

fn search_ranges(
    search: Option<CardSearch<'_>>,
    text: &str,
) -> Vec<(std::ops::Range<usize>, bool)> {
    search
        .map(|search| {
            crate::search::find_matches(&[(0, text.to_owned())], search.query)
                .into_iter()
                .map(|found| (found.range, search.current))
                .collect()
        })
        .unwrap_or_default()
}

fn pad_line(mut spans: Vec<Span<'static>>, bg: Color, width: u16) -> Line<'static> {
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::new().bg(bg)));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, key, render, standard_fixture};
    use crate::theme::Theme;
    use crate::ui::diff::test_support::{diff_app, plain};

    use ratatui::style::Style;

    fn joined(spans: &[ratatui::text::Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn clip_spans_keeps_a_short_name_whole() {
        let style = ratatui::style::Style::new();
        assert_eq!(
            joined(&clip_spans(plain("lib.rs"), 32, false, style)),
            "lib.rs"
        );
    }

    #[test]
    fn clip_spans_end_elides_a_long_basename() {
        let style = ratatui::style::Style::new();
        let clipped = clip_spans(plain("a_very_long_filename_indeed.rs"), 12, false, style);
        let text = joined(&clipped);
        assert_eq!(text.chars().count(), 12);
        assert!(text.ends_with('…'), "clipped at the end: {text:?}");
        assert!(text.starts_with("a_very"), "kept from the start: {text:?}");
    }

    #[test]
    fn clip_spans_front_elides_a_path_to_keep_the_basename() {
        let style = ratatui::style::Style::new();
        assert_eq!(
            joined(&clip_spans(plain("src/lib.rs"), 32, true, style)),
            "src/lib.rs"
        );
        let clipped = clip_spans(plain("deep/nested/dir/module.rs"), 12, true, style);
        let text = joined(&clipped);
        assert_eq!(text.chars().count(), 12);
        assert!(text.starts_with('…'), "front-elided: {text:?}");
        assert!(text.ends_with("module.rs"), "basename kept: {text:?}");
    }

    #[test]
    fn clip_spans_zero_room_yields_nothing() {
        let style = ratatui::style::Style::new();
        assert!(clip_spans(plain("lib.rs"), 0, false, style).is_empty());
    }

    #[test]
    fn clip_spans_keeps_a_highlight_lit_on_the_visible_part() {
        let theme = Theme::github_dark();
        // "status" starts at byte 13 of this name and survives an end-clip
        let name = "diffler__ui__status__tests__foo";
        let spans = super::super::highlight_spans(name, Style::new(), &[(13..19, true)], &theme);
        let clipped = clip_spans(spans, 20, false, Style::new());
        let lit: String = clipped
            .iter()
            .filter(|s| s.style.bg == Some(theme.search_current))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            lit, "status",
            "the match stays lit after clipping: {clipped:?}"
        );
    }

    #[test]
    fn diff_algorithm_picker_switches_the_algorithm_live() {
        let (_fixture, mut app) = diff_app();
        app.handle(crate::test_support::ctrl_key('a'));
        insta::assert_snapshot!(render(&mut app).backend());

        // myers, minimal, patience, histogram: three steps down
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(crate::test_support::code_key(
            crossterm::event::KeyCode::Enter,
        ));
        assert_eq!(
            app.config.diff.algorithm,
            diffler_core::diffalgo::DiffAlgorithm::Histogram
        );
        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains("histogram"),
            "pane heading names the active algorithm: {screen}"
        );
    }

    /// The content hash is the same under both algorithms, so an enrichment
    /// queued before the switch could put the old hunks back.
    #[test]
    fn a_switch_rediffs_everything_and_outlives_an_enrichment_in_flight() {
        use diffler_core::diffalgo::{DiffAlgorithm, histogram_hunks};
        let old = "begin\nrepeat\nrepeat\nunique_anchor\nrepeat\nrepeat\nend\n";
        let new = "begin\nunique_anchor\nrepeat\nrepeat\nrepeat\nrepeat\nend\n";
        let fixture = crate::test_support::Fixture::new();
        fixture.write("a.txt", old);
        fixture.commit_all("base");
        fixture.write("a.txt", new);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        app.queue_enrich_selected();

        app.apply_diff_algorithm("histogram");
        app.settle_rediff();
        let expected = histogram_hunks(old, new, "a.txt", 3, true);
        let ids = |hunks: &[diffler_core::model::Hunk]| -> Vec<_> {
            hunks.iter().map(|h| h.id.clone()).collect()
        };
        app.enrich_now();
        assert_eq!(
            ids(&app.review.model().files[0].hunks),
            ids(&expected),
            "the stale job left the histogram hunks alone"
        );
        app.queue_enrich_selected();
        app.enrich_now();
        assert_eq!(ids(&app.review.model().files[0].hunks), ids(&expected));
        assert_eq!(
            ids(&app.review.status.unstaged.files[0].hunks),
            ids(&expected),
            "the status sections re-diffed too"
        );
        assert_eq!(app.config.diff.algorithm, DiffAlgorithm::Histogram);
    }

    #[test]
    fn a_switch_says_whether_the_hunks_changed() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write(
            "a.txt",
            "begin\nrepeat\nrepeat\nunique_anchor\nrepeat\nrepeat\nend\n",
        );
        fixture.write("b.txt", "one\ntwo\nthree\n");
        fixture.commit_all("base");
        fixture.write(
            "a.txt",
            "begin\nunique_anchor\nrepeat\nrepeat\nrepeat\nrepeat\nend\n",
        );
        fixture.write("b.txt", "one\nTWO\nthree\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let said = |app: &App| app.message.as_ref().map(|m| m.text.clone());

        app.apply_diff_algorithm("patience");
        app.settle_rediff();
        assert_eq!(
            said(&app).as_deref(),
            Some("patience changed the hunks of 1 file")
        );

        app.apply_diff_algorithm("patience");
        app.settle_rediff();
        assert_eq!(
            said(&app).as_deref(),
            Some("patience gives the same hunks here")
        );
    }

    #[test]
    fn a_rediff_keeps_the_cursor_moved_while_it_ran() {
        use crate::app::rowsel::RowText as _;
        use std::fmt::Write as _;
        let fixture = Fixture::new();
        let mut base = String::new();
        for i in 1..=40 {
            let _ = writeln!(base, "line {i}");
        }
        fixture.write("a.txt", &base);
        fixture.commit_all("base");
        let edited = base
            .replace("line 5\n", "LINE FIVE\n")
            .replace("line 35\n", "LINE THIRTY-FIVE\n");
        fixture.write("a.txt", &edited);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("a.txt");
        let _ = render(&mut app);

        app.apply_diff_algorithm("histogram");
        let diff = app.diff.as_mut().unwrap();
        let target = (0..diff.rows().len())
            .find(|&row| diff.row_text(row) == " line 36")
            .unwrap();
        diff.cursor = target;
        app.settle_rediff();

        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.row_text(diff.cursor), " line 36");
    }

    #[test]
    fn a_view_opened_during_a_rediff_rebuilds_when_it_lands() {
        let old = "begin\nrepeat\nrepeat\nunique_anchor\nrepeat\nrepeat\nend\n";
        let new = "begin\nunique_anchor\nrepeat\nrepeat\nrepeat\nrepeat\nend\n";
        let fixture = Fixture::new();
        fixture.write("a.txt", old);
        fixture.commit_all("base");
        fixture.write("a.txt", new);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let rows = |app: &mut App| {
            use crate::app::rowsel::RowText as _;
            let _ = render(app);
            let diff = app.diff.as_ref().unwrap();
            (0..diff.rows().len())
                .map(|row| diff.row_text(row))
                .collect::<Vec<_>>()
        };

        let _ = app.review.model();
        app.apply_diff_algorithm("histogram");
        app.open_working_tree_diff(None);
        let stale = rows(&mut app);
        app.settle_rediff();
        let landed = rows(&mut app);
        app.diff = None;
        app.open_working_tree_diff(None);
        let fresh = rows(&mut app);
        assert_ne!(stale, fresh, "the two algorithms draw different rows");
        assert_eq!(landed, fresh);
    }

    #[test]
    fn review_verdict_picker_renders_its_choices() {
        let (_fixture, mut app) = diff_app();
        let comment = crate::ci::NewPrComment {
            number: 7,
            head_oid: "abc".to_owned(),
            path: "src/lib.rs".to_owned(),
            line: Some(2),
            start_line: None,
            new_side: true,
            counterpart: None,
            body: "looks off".to_owned(),
        };
        let pending = crate::app::pr::PrPending {
            review_comments: vec![comment.clone(), comment],
            replies: vec![crate::app::pr::PrPost::Reply {
                number: 7,
                comment_id: "c1".to_owned(),
                reply_index: 0,
                parent_remote_id: "r1".to_owned(),
                body: "and this".to_owned(),
            }],
            agent_withheld: 2,
            file_level: 1,
            file_level_forge: Some(crate::ci::ProviderKind::Forgejo),
            ..Default::default()
        };
        app.modal = Some(crate::app::Modal::ReviewVerdict {
            number: 7,
            summary: pending.summary(),
        });
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn md_span_applies_the_syntax_color() {
        let theme = crate::theme::Theme::github_dark();
        let run = crate::app::markdown::MdSpan {
            text: "fn".to_owned(),
            code: true,
            fg: Some((1, 2, 3)),
            ..Default::default()
        };
        let span = super::cards::md_span(&run, theme.base(), &theme);
        assert_eq!(
            span.style.fg,
            Some(ratatui::style::Color::Rgb(1, 2, 3)),
            "fg overrides the code accent"
        );
    }

    #[test]
    fn commit_diff_opened_from_the_log_renders() {
        let (_fixture, mut app) = diff_app();
        // back to status, into the log, open the only commit
        app.handle(key('q'));
        app.handle(key('l'));
        app.handle(key('l'));
        app.handle(key('\n'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn range_diff_from_the_log_renders_with_a_range_header() {
        let fixture = standard_fixture();
        fixture.write("notes.txt", "alpha\nbeta\n");
        fixture.commit_all("add beta note");
        fixture.write(
            "src/util.rs",
            "pub fn twice(x: u32) -> u32 {\n    x * 2\n}\n",
        );
        fixture.commit_all("add util module");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        // into the log, select the two newest commits, open the combined diff
        app.handle(key('l'));
        app.handle(key('l'));
        app.handle(key('V'));
        app.handle(key('j'));
        app.handle(key('\n'));
        let content = render(&mut app).backend().to_string();
        assert!(
            content.contains(".."),
            "range header shows a span: {content}"
        );
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn status_bar_shows_viewed_progress() {
        let (_fixture, mut app) = diff_app();
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("viewed 0/3 files"), "{content}");
        app.handle(key('m'));
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("viewed 1/3 files"), "{content}");
    }
}
