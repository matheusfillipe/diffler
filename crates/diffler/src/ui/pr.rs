//! One pull request's page: a header, the files row that opens the diff, the
//! description, and the conversation as cards.

use std::collections::HashMap;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::fuzzy::FuzzyList;
use crate::app::pr_page::{PrItem, PrLine, PrPage};
use crate::app::{App, REPLY_LANE};
use crate::ci::{PrState, PrVerdict, ReviewerCandidate};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::Hint;
use crate::ui::diff::avatars::AvatarCtx;
use crate::ui::diff::cards::{author_color, md_span};
use crate::ui::diff::comments::author_orders;
use crate::ui::diff_render::{card_frame, card_surface};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "open diff"),
    Hint::Leaf(&[Action::Comment], "add comment"),
    Hint::Leaf(&[Action::Reply], "reply"),
    Hint::Leaf(&[Action::NextSection], "go to next comment"),
    Hint::Leaf(&[Action::Help], "help"),
];

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (header, body, bar) = super::screen_chrome_with_header(frame, app, HINTS);
    let show_avatars = app.avatars_shown();
    let covered = app.modal.is_some() || app.which_key_panel().is_some();
    let comment_key = app.active_keymap().chord_for(Action::Comment);
    let refresh_key = app.active_keymap().chord_for(Action::Refresh);
    let request_key = app.active_keymap().chord_for(Action::RequestReviewer);
    let highlighter = Arc::clone(&app.highlighter);
    let theme = &app.theme;
    let avatars = AvatarCtx::new(
        show_avatars.then_some(&app.avatars),
        app.author.as_str(),
        covered,
    );
    let Some(page) = app.pr_pages.last_mut() else {
        return;
    };
    frame.render_widget(Paragraph::new(header_line(theme, page)), header);
    let height = usize::from(body.height);
    let total = page.lines(body.width, &highlighter).len();
    page.viewport = body.height;
    page.scroll = super::scroll_to_cursor(page.cursor, page.scroll, height, total);
    let page: &PrPage = page;
    let human = app.author.as_str();
    let orders = page.conversation.as_ref().map_or_else(HashMap::new, |c| {
        let authors = std::iter::once(c.pr.author.as_str())
            .chain(c.notes.iter().map(|note| note.author.as_str()));
        author_orders(authors, human)
    });
    let ctx = LineCtx {
        theme,
        page,
        avatars: &avatars,
        human,
        orders,
        now: app.now_unix,
        width: body.width,
        selected: page.item_at_cursor(),
        comment_key: comment_key.as_deref(),
        refresh_key: refresh_key.as_deref(),
        request_key: request_key.as_deref(),
    };
    let rows: Vec<Line<'static>> = page
        .cached_lines()
        .iter()
        .enumerate()
        .skip(page.scroll)
        .take(height)
        .map(|(at, (item, line))| {
            let ranges = app
                .search
                .as_ref()
                .map(|search| search.ranges_for(at))
                .unwrap_or_default();
            let drawn = ctx.line(*item, line, &ranges);
            avatars.claim(at, true);
            if at == page.cursor {
                super::cursor_line(drawn, theme, body.width)
            } else {
                drawn
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), body);
    avatars.draw(frame, body, page.scroll);
    frame.render_widget(Paragraph::new(super::status_bar(app, bar.width)), bar);
}

fn header_line(theme: &Theme, page: &PrPage) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" #{} ", page.number()),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            page.pr.title.clone(),
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
    ])
}

struct LineCtx<'a> {
    theme: &'a Theme,
    page: &'a PrPage,
    avatars: &'a AvatarCtx<'a>,
    human: &'a str,
    orders: HashMap<&'a str, usize>,
    now: i64,
    width: u16,
    selected: Option<PrItem>,
    comment_key: Option<&'a str>,
    refresh_key: Option<&'a str>,
    request_key: Option<&'a str>,
}

impl LineCtx<'_> {
    fn line(
        &self,
        item: Option<PrItem>,
        line: &PrLine,
        matches: &[(std::ops::Range<usize>, bool)],
    ) -> Line<'static> {
        let theme = self.theme;
        let dim = theme.dim_style();
        match line {
            PrLine::Meta => self.meta(),
            PrLine::Files => self.files(),
            PrLine::Reviewers => self.reviewers(),
            PrLine::Blank => Line::default(),
            PrLine::Section { title, count } => {
                let mut spans = vec![Span::styled(
                    format!(" {title}"),
                    Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
                )];
                if let Some(count) = count {
                    spans.push(Span::styled(format!("  {count}"), dim));
                }
                Line::from(spans)
            }
            PrLine::Head(note) => self.head(item, *note),
            PrLine::Body(runs) => {
                let (bg, mut spans) = self.card_lead(item);
                let fg = Style::new().fg(theme.fg).bg(bg);
                // search ranges index the row's plain text, so a matching row drops its markdown styling
                if matches.is_empty() {
                    spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
                } else {
                    let text: String = runs.iter().map(|run| run.text.as_str()).collect();
                    spans.extend(super::highlight_spans(&text, fg, matches, theme));
                }
                super::fill_row(Line::from(spans), bg, self.width)
            }
            PrLine::NoDescription => {
                let (bg, mut spans) = self.card_lead(item);
                spans.push(Span::styled("no description", dim.bg(bg)));
                super::fill_row(Line::from(spans), bg, self.width)
            }
            PrLine::NoNotes => Line::styled(
                match self.comment_key {
                    Some(key) => format!("  no comments yet, {key} to add one"),
                    None => "  no comments yet".to_owned(),
                },
                dim,
            ),
            PrLine::Loading => Line::styled("  loading the conversation…", dim),
            PrLine::Failed(err) => {
                let retry = self
                    .refresh_key
                    .map(|key| format!(", {key} to retry"))
                    .unwrap_or_default();
                Line::styled(
                    format!("  cannot load the conversation: {err}{retry}"),
                    Style::new().fg(theme.error_fg),
                )
            }
        }
    }

    fn meta(&self) -> Line<'static> {
        let theme = self.theme;
        let dim = theme.dim_style();
        let pr = &self.page.pr;
        let state = self.page.conversation.as_ref().map(|c| c.state);
        let mut spans = vec![Span::raw(" ")];
        if let Some(state) = state {
            spans.push(Span::styled(
                state.label(),
                Style::new().fg(state_color(theme, state)),
            ));
            spans.push(Span::styled(" · ", dim));
        }
        spans.extend([
            Span::styled(pr.author.clone(), Style::new().fg(theme.accent)),
            Span::styled(" · ", dim),
            Span::styled(pr.head_ref.clone(), Style::new().fg(theme.purple)),
            Span::styled(" → ", dim),
            Span::styled(pr.base_ref.clone(), Style::new().fg(theme.purple)),
        ]);
        let labels = self
            .page
            .conversation
            .as_ref()
            .map(|c| c.labels.as_slice())
            .unwrap_or_default();
        for label in labels {
            spans.extend(super::label_chip_spans(theme, label));
        }
        Line::from(spans)
    }

    fn files(&self) -> Line<'static> {
        let theme = self.theme;
        let mut spans = vec![Span::styled(
            " Files changed",
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        )];
        let Some(conversation) = self.page.conversation.as_ref() else {
            return Line::from(spans);
        };
        if let Some(files) = conversation.changed_files {
            spans.push(Span::styled(format!("  {files}"), theme.dim_style()));
        }
        if let (Some(added), Some(deleted)) = (conversation.additions, conversation.deletions) {
            let count = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
            spans.push(Span::raw(" "));
            spans.extend(super::diffstat_spans(
                theme,
                count(added),
                count(deleted),
                theme.bg,
            ));
        }
        Line::from(spans)
    }

    fn reviewers(&self) -> Line<'static> {
        let theme = self.theme;
        let dim = theme.dim_style();
        let mut spans = vec![Span::styled(
            " Reviewers",
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        )];
        let reviewers = self
            .page
            .conversation
            .as_ref()
            .map(|c| c.reviewers.as_slice())
            .unwrap_or_default();
        if reviewers.is_empty() {
            spans.push(Span::styled(
                match self.request_key {
                    Some(key) => format!("  no reviewers yet, {key} to request one"),
                    None => "  no reviewers yet".to_owned(),
                },
                dim,
            ));
            return Line::from(spans);
        }
        for reviewer in reviewers {
            spans.push(Span::raw("  "));
            self.avatars
                .gap(&mut spans, theme.bg, &reviewer.login, true);
            let (glyph, color) = match reviewer.verdict {
                None => ("●", theme.warn_fg),
                Some(verdict) => (verdict_glyph(verdict), verdict_color(theme, verdict)),
            };
            spans.push(Span::styled(format!("{glyph} "), Style::new().fg(color)));
            spans.push(Span::styled(
                reviewer.login.clone(),
                Style::new().fg(self.author_color(theme.bg, &reviewer.login)),
            ));
        }
        if let Some(key) = self.request_key {
            spans.push(Span::styled(format!("  · {key} to request another"), dim));
        }
        Line::from(spans)
    }

    fn head(&self, item: Option<PrItem>, note: Option<usize>) -> Line<'static> {
        let theme = self.theme;
        let (bg, mut spans) = self.card_lead(item);
        let dim = theme.dim_style().bg(bg);
        let note = note.and_then(|index| self.page.conversation.as_ref()?.notes.get(index));
        let author = note.map_or(self.page.pr.author.as_str(), |note| note.author.as_str());
        self.avatars.gap(&mut spans, bg, author, true);
        spans.push(Span::styled(
            author.to_owned(),
            Style::new()
                .fg(self.author_color(bg, author))
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        match note {
            None => spans.push(Span::styled(" · opened the pull request", dim)),
            Some(note) => {
                if let Some(verdict) = note.verdict {
                    spans.push(Span::styled(" · ", dim));
                    spans.push(Span::styled(
                        verdict.label(),
                        Style::new().fg(verdict_color(theme, verdict)).bg(bg),
                    ));
                }
                if note.at > 0 {
                    let at = i64::try_from(note.at).unwrap_or(i64::MAX);
                    spans.push(Span::styled(
                        format!(" · {}", super::relative_time(self.now, at)),
                        dim,
                    ));
                }
            }
        }
        super::fill_row(Line::from(spans), bg, self.width)
    }

    /// The card's background and its bar, indented when the row's note
    /// answers another inside a forge thread.
    fn card_lead(&self, item: Option<PrItem>) -> (Color, Vec<Span<'static>>) {
        let author = match item {
            Some(PrItem::Note(index)) => self
                .page
                .conversation
                .as_ref()
                .and_then(|c| c.notes.get(index))
                .map(|note| (note.author.as_str(), note.reply_to.is_some())),
            _ => Some((self.page.pr.author.as_str(), false)),
        };
        let (author, reply) = author.unwrap_or_default();
        let selected = item.is_some() && item == self.selected;
        let surface = card_surface(self.theme);
        let accent = self.author_color(surface, author);
        let (bg, bar) = card_frame(self.theme, selected, true, accent);
        if !reply {
            return (bg, vec![bar]);
        }
        let indent = Span::styled(" ".repeat(REPLY_LANE), Style::new().bg(bg));
        (bg, vec![indent, bar])
    }

    fn author_color(&self, bg: Color, author: &str) -> Color {
        let order = self.orders.get(author).copied().unwrap_or(0);
        author_color(self.theme, bg, self.human, author, order)
    }
}

fn state_color(theme: &Theme, state: PrState) -> Color {
    match state {
        PrState::Open => theme.added,
        PrState::Draft => theme.dim,
        PrState::Merged => theme.purple,
        PrState::Closed => theme.error_fg,
    }
}

fn verdict_glyph(verdict: PrVerdict) -> &'static str {
    match verdict {
        PrVerdict::Approved => "✓",
        PrVerdict::ChangesRequested => "✗",
        PrVerdict::Commented => "◆",
    }
}

pub(super) fn reviewer_picker(
    number: u64,
    candidates: &[ReviewerCandidate],
    list: &FuzzyList,
) -> super::popup::FuzzyModal {
    let rows: Vec<(String, String)> = candidates
        .iter()
        .map(|c| {
            let note = if c.suggested { "suggested" } else { "" };
            (c.login.clone(), note.to_owned())
        })
        .collect();
    super::noted_list(
        format!("Request a reviewer for #{number}"),
        list,
        &rows,
        " request",
    )
}

pub(super) fn verdict_color(theme: &Theme, verdict: PrVerdict) -> Color {
    match verdict {
        PrVerdict::Approved => theme.added,
        PrVerdict::ChangesRequested => theme.error_fg,
        PrVerdict::Commented => theme.accent,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::pr_page::tests::{conversation, pr};
    use crate::app::{App, Screen};
    use crate::config::LoadedConfig;
    use crate::keymap::Action;
    use crate::test_support::standard_fixture;

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(frame, app))
            .expect("draw");
        terminal.backend().to_string()
    }

    fn page_app() -> (crate::test_support::Fixture, App) {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_pr_page(pr(7));
        (fixture, app)
    }

    #[test]
    fn a_loading_page_shows_the_pr_and_waits_for_its_conversation() {
        let (_fixture, mut app) = page_app();
        assert_eq!(app.screen(), Screen::Pr);
        insta::assert_snapshot!(screen(&mut app, 80, 10));
    }

    #[test]
    fn the_page_shows_the_description_and_every_comment_as_a_card() {
        let (_fixture, mut app) = page_app();
        app.on_pr_conversation(7, &Ok(Box::new(conversation(7))));
        insta::assert_snapshot!(screen(&mut app, 80, 22));
    }

    #[test]
    fn a_page_with_no_comments_names_the_key_that_adds_one() {
        let (_fixture, mut app) = page_app();
        let mut empty = conversation(7);
        empty.notes.clear();
        empty.body.clear();
        app.on_pr_conversation(7, &Ok(Box::new(empty)));
        insta::assert_snapshot!(screen(&mut app, 80, 14));
    }

    #[test]
    fn a_failed_load_names_the_key_that_retries() {
        let (_fixture, mut app) = page_app();
        app.on_pr_conversation(7, &Err("gh: not logged in".to_owned()));
        let shown = screen(&mut app, 80, 10);
        assert!(
            shown.contains("cannot load the conversation: gh: not logged in, <c-r> to retry"),
            "{shown}"
        );
    }

    #[test]
    fn a_page_with_no_reviewers_names_the_key_that_requests_one() {
        let (_fixture, mut app) = page_app();
        let mut lonely = conversation(7);
        lonely.reviewers.clear();
        app.on_pr_conversation(7, &Ok(Box::new(lonely)));
        let shown = screen(&mut app, 80, 10);
        assert!(
            shown.contains("Reviewers  no reviewers yet, R to request one"),
            "{shown}"
        );
    }

    #[test]
    fn the_reviewer_picker_marks_the_suggested() {
        let (_fixture, mut app) = page_app();
        app.on_pr_conversation(7, &Ok(Box::new(conversation(7))));
        app.on_reviewer_candidates(
            7,
            Ok(vec![
                crate::ci::ReviewerCandidate {
                    login: "lead".to_owned(),
                    suggested: true,
                },
                crate::ci::ReviewerCandidate {
                    login: "new".to_owned(),
                    suggested: false,
                },
            ]),
        );
        insta::assert_snapshot!(screen(&mut app, 80, 22));
    }

    #[test]
    fn the_selected_card_follows_the_brackets() {
        let (_fixture, mut app) = page_app();
        app.on_pr_conversation(7, &Ok(Box::new(conversation(7))));
        screen(&mut app, 80, 22);
        app.dispatch(Action::NextSection);
        app.dispatch(Action::NextSection);
        insta::assert_snapshot!(screen(&mut app, 80, 22));
    }
}
