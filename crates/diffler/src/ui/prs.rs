//! The list of the forge's open pull requests.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::ci::{PrSummary, PullRequest};
use crate::keymap::Action;
use crate::text::elide;
use crate::theme::Theme;
use crate::ui::Hint;

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "open"),
    Hint::Leaf(&[Action::OpenPrDiff], "review diff"),
    Hint::Leaf(&[Action::BranchCheckout], "checkout"),
    Hint::Leaf(&[Action::PrWorktree], "open in a worktree"),
    Hint::Leaf(&[Action::Help], "help"),
];

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body, bar) = super::screen_chrome(frame, app, HINTS);
    draw_list(frame, app, body);
    frame.render_widget(Paragraph::new(super::status_bar(app, bar.width)), bar);
}

fn draw_list(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if app.prs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "  no open pull requests…",
                app.theme.dim_style(),
            )),
            area,
        );
        return;
    }
    let height = area.height.max(1) as usize;
    let scroll = super::scroll_to_cursor(app.prs_cursor, app.prs_scroll, height, app.prs.len());
    app.prs_scroll = scroll;
    let author_width =
        super::column_width(app.prs.iter().map(|pr| pr.author.as_str()), AUTHOR_WIDTH);
    let rows: Vec<Line<'static>> = app
        .prs
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(i, pr)| {
            let line = row(app, i, pr, area.width as usize, author_width);
            if i == app.prs_cursor {
                super::cursor_line(line, &app.theme, area.width)
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
}

/// We size the age column for `12mo`, the widest age we render.
const AGE_WIDTH: usize = 4;
/// We cut longer logins so the author column stays narrow.
const AUTHOR_WIDTH: usize = 16;
/// We never cut a title shorter than this to make room for the details.
const MIN_TITLE: usize = 24;

fn row(
    app: &App,
    index: usize,
    pr: &PullRequest,
    width: usize,
    author_width: usize,
) -> Line<'static> {
    let theme = &app.theme;
    let summary = app.pr_summaries.get(&pr.number);
    let ranges = app
        .search
        .as_ref()
        .map(|s| s.ranges_for(index))
        .unwrap_or_default();
    let headline = pr.headline();
    let right = right_columns(app, pr, summary, &ranges, headline.len() + 1, author_width);
    let right_width: usize = right.iter().map(Span::width).sum();
    let mut details = detail_groups(theme, summary);
    let groups_width =
        |groups: &[Vec<Span<'_>>]| -> usize { groups.iter().flatten().map(Span::width).sum() };
    let min_title = headline.width().min(MIN_TITLE);
    while !details.is_empty() && 4 + min_title + groups_width(&details) + right_width > width {
        details.pop();
    }
    let details_width = groups_width(&details);
    let budget = width
        .saturating_sub(4 + details_width + right_width)
        .max(MIN_TITLE);
    let title_style = if summary.is_some_and(|s| s.draft) {
        theme.dim_style()
    } else {
        Style::new().fg(theme.fg)
    };
    let mut spans = vec![Span::raw(" ")];
    spans.extend(super::ci_glyph_spans(summary.and_then(|s| s.checks), theme));
    spans.extend(super::highlight_spans(
        &elide(&headline, budget),
        title_style,
        &ranges,
        theme,
    ));
    spans.extend(details.into_iter().flatten());
    let branches = format!("  {} → {}", pr.head_ref, pr.base_ref);
    let used: usize = spans.iter().map(Span::width).sum();
    if used + branches.width() + right_width < width {
        spans.push(Span::styled(branches, Style::new().fg(theme.purple)));
    }
    let used: usize = spans.iter().map(Span::width).sum();
    if used + right_width <= width {
        spans.push(Span::raw(" ".repeat(width - used - right_width)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// The search matches `#number title author`, so the author's matches start
/// `author_at` bytes in.
fn right_columns(
    app: &App,
    pr: &PullRequest,
    summary: Option<&PrSummary>,
    ranges: &[(std::ops::Range<usize>, bool)],
    author_at: usize,
    author_width: usize,
) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let author_ranges: Vec<_> = ranges
        .iter()
        .filter(|(range, _)| range.end > author_at)
        .map(|(range, current)| {
            (
                range.start.saturating_sub(author_at)..range.end - author_at,
                *current,
            )
        })
        .collect();
    let mut spans = vec![Span::raw("  ")];
    spans.extend(super::highlight_spans(
        &super::fit(&pr.author, author_width),
        Style::new().fg(theme.accent),
        &author_ranges,
        theme,
    ));
    let age = summary
        .and_then(|s| i64::try_from(s.updated).ok())
        .filter(|&at| at > 0)
        .map(|at| super::relative_time(app.now_unix, at))
        .unwrap_or_default();
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{age:>AGE_WIDTH$} "),
        theme.dim_style(),
    ));
    spans
}

/// Draft, review decision, size, comment count and labels, each where the
/// forge reports it, most telling first so a narrow row drops from the end.
fn detail_groups(theme: &Theme, summary: Option<&PrSummary>) -> Vec<Vec<Span<'static>>> {
    let Some(summary) = summary else {
        return Vec::new();
    };
    let dim = theme.dim_style();
    let mut groups = Vec::new();
    if summary.draft {
        groups.push(vec![Span::styled("  draft", dim)]);
    }
    if let Some(review) = summary.review {
        groups.push(vec![Span::styled(
            format!("  {}", review.label()),
            Style::new().fg(super::pr::verdict_color(theme, review)),
        )]);
    }
    if let (Some(added), Some(deleted)) = (summary.additions, summary.deletions) {
        let mut size = vec![Span::raw(" ")];
        size.extend(super::diffstat_spans(
            theme,
            usize::try_from(added).unwrap_or(usize::MAX),
            usize::try_from(deleted).unwrap_or(usize::MAX),
            theme.bg,
        ));
        groups.push(size);
    }
    match summary.comments {
        Some(0) | None => {}
        Some(1) => groups.push(vec![Span::styled("  1 comment", dim)]),
        Some(count) => groups.push(vec![Span::styled(format!("  {count} comments"), dim)]),
    }
    for label in &summary.labels {
        groups.push(super::label_chip_spans(theme, label).to_vec());
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::PullRequest;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn pr(number: u64, title: &str, author: &str, head_ref: &str, base_ref: &str) -> PullRequest {
        PullRequest {
            number,
            title: title.to_owned(),
            url: None,
            base_ref: base_ref.to_owned(),
            head_ref: head_ref.to_owned(),
            head_oid: "0".repeat(40),
            author: author.to_owned(),
        }
    }

    /// The hint line reads the active screen's keymap, so a test that draws
    /// this pane while the app still sits on the status screen renders the
    /// wrong keys.
    fn prs_app(fixture: &crate::test_support::Fixture) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.push_screen(crate::app::Screen::Prs);
        app
    }

    #[test]
    fn renders_the_pr_list_with_a_selection() {
        let fixture = standard_fixture();
        let mut app = prs_app(&fixture);
        app.prs = vec![
            pr(12, "Add widgets", "alice", "feat/widgets", "main"),
            pr(9, "Fix flaky test", "bob", "fix/flaky", "main"),
        ];
        app.prs_cursor = 1;
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn rows_show_checks_review_size_labels_comments_and_age() {
        let fixture = standard_fixture();
        let mut app = prs_app(&fixture);
        app.now_unix = 1_000_000;
        app.prs = vec![
            pr(12, "Add widgets", "alice", "feat/widgets", "main"),
            pr(9, "Fix flaky test", "bob", "fix/flaky", "main"),
            pr(
                5,
                "Rework the whole rendering pipeline so long titles get cut",
                "carol",
                "feat/render",
                "main",
            ),
        ];
        app.pr_summaries = [
            (
                12,
                PrSummary {
                    checks: Some(crate::ci::JobStatus::Ok),
                    review: Some(crate::ci::PrVerdict::Approved),
                    additions: Some(120),
                    deletions: Some(4),
                    comments: Some(3),
                    labels: vec!["feature".to_owned()],
                    updated: 1_000_000 - 7200,
                    ..PrSummary::default()
                },
            ),
            (
                9,
                PrSummary {
                    draft: true,
                    checks: Some(crate::ci::JobStatus::Failed),
                    comments: Some(1),
                    updated: 1_000_000 - 3 * 86_400,
                    ..PrSummary::default()
                },
            ),
            (
                5,
                PrSummary {
                    checks: Some(crate::ci::JobStatus::Running),
                    review: Some(crate::ci::PrVerdict::ChangesRequested),
                    additions: Some(2400),
                    deletions: Some(900),
                    updated: 1_000_000 - 60,
                    ..PrSummary::default()
                },
            ),
        ]
        .into_iter()
        .collect();
        let backend = TestBackend::new(110, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn a_narrow_row_drops_labels_before_the_author_and_age() {
        let fixture = standard_fixture();
        let mut app = prs_app(&fixture);
        app.now_unix = 1_000_000;
        app.prs = vec![pr(12, "Add widgets", "alice", "feat/widgets", "main")];
        app.pr_summaries.insert(
            12,
            PrSummary {
                draft: true,
                additions: Some(120),
                deletions: Some(4),
                labels: vec!["do not merge".to_owned(), "preview".to_owned()],
                updated: 1_000_000 - 7200,
                ..PrSummary::default()
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 4)).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        let content = terminal.backend().to_string();
        assert!(
            content.contains("alice") && content.contains("2h") && content.contains("+120"),
            "{content}"
        );
        assert!(!content.contains("preview"), "{content}");
    }

    #[test]
    fn renders_no_open_pull_requests() {
        let fixture = standard_fixture();
        let mut app = prs_app(&fixture);
        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
