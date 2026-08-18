//! Rendering. `draw` never computes review state; it reads `App` (the diff
//! view additionally fills its lazy highlight cache and follows the cursor
//! with its scroll offset, which is why it takes `&mut App`).

pub mod ci_log;
pub mod diff;
pub mod diff_render;
pub mod file;
pub mod graph;
pub mod log;
pub mod popup;
mod prs;
mod runs;
mod stats;
pub mod status;

use diffler_core::language;
use diffler_core::model::FileStatus;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, BranchAction, Modal, Screen, Severity, fuzzy};
use crate::keymap::{Action, render_chord};
use crate::theme::Theme;
use crate::transient::TransientKind;

/// Split `text` into spans, painting `/`-search match byte ranges with the
/// search background (the active match stronger). Shared by every searchable
/// pane so highlight looks the same everywhere; `ranges` are byte offsets into
/// `text`, paired with whether each is the active match.
pub(super) fn highlight_spans(
    text: &str,
    base: Style,
    ranges: &[(std::ops::Range<usize>, bool)],
    theme: &Theme,
) -> Vec<Span<'static>> {
    highlight_spans_split(text, 0, base, base, ranges, theme)
}

/// [`highlight_spans`] with the leading `split` bytes in `lead` instead of
/// `base`: a path recedes into its parent directories while its basename keeps
/// the foreground, and a search hit stays lit across the boundary.
pub(super) fn highlight_spans_split(
    text: &str,
    split: usize,
    lead: Style,
    base: Style,
    ranges: &[(std::ops::Range<usize>, bool)],
    theme: &Theme,
) -> Vec<Span<'static>> {
    if ranges.is_empty() && split == 0 {
        return vec![Span::styled(text.to_owned(), base)];
    }
    let snap = |i: usize| {
        let mut i = i.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let split = snap(split);
    let mut bounds = vec![0, split, text.len()];
    for (range, _) in ranges {
        bounds.push(snap(range.start));
        bounds.push(snap(range.end));
    }
    bounds.sort_unstable();
    bounds.dedup();
    let bg_at = |at: usize| {
        ranges
            .iter()
            .find(|(range, _)| snap(range.start) <= at && at < snap(range.end))
            .map(|(_, current)| {
                if *current {
                    theme.search_current
                } else {
                    theme.search
                }
            })
    };
    let mut spans = Vec::new();
    for pair in bounds.windows(2) {
        let &[start, end] = pair else { continue };
        let Some(segment) = text.get(start..end) else {
            continue;
        };
        if segment.is_empty() {
            continue;
        }
        let base = if start < split { lead } else { base };
        let style = bg_at(start).map_or(base, |bg| base.bg(bg));
        spans.push(Span::styled(segment.to_owned(), style));
    }
    spans
}

/// Shared chrome for the `[hint, body, bar]` screens: paints the full-area
/// background, splits off the hint row and renders it, and hands back the
/// body and bar rects. The bar's own paragraph (content and style both vary
/// per screen) stays with the caller.
pub(super) fn screen_chrome(frame: &mut Frame<'_>, app: &App, hints: &[Hint]) -> (Rect, Rect) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, body, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(hint_line(app, hints)), hint);
    (body, bar)
}

/// Like [`screen_chrome`], for screens that carve an extra header row (a
/// provenance/summary line) out from under the hint line.
pub(super) fn screen_chrome_with_header(
    frame: &mut Frame<'_>,
    app: &App,
    hints: &[Hint],
) -> (Rect, Rect, Rect) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, header, body, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(hint_line(app, hints)), hint);
    (header, body, bar)
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    match app.screen() {
        Screen::Status => {
            // enrichment (emphasis/highlight) runs on the blocking pool; this
            // only queues work, and expanded inline diffs render plain until
            // the result lands
            app.queue_enrich_status_expanded();
            status::draw(frame, app);
        }
        Screen::Log => log::draw(frame, app),
        Screen::Diff => diff::draw(frame, app),
        Screen::Graph => graph::draw(frame, app),
        Screen::Runs => runs::draw(frame, app),
        Screen::Prs => prs::draw(frame, app),
        Screen::CiLog => ci_log::draw(frame, app),
        Screen::File => file::draw(frame, app),
        Screen::Stats => stats::draw(frame, app),
    }
    app.modal_hits = draw_modal(frame, app);
    // the which-key panel is a transient overlay, not a modal: it draws only
    // once the reveal timer has elapsed and never over a modal
    if app.modal.is_none()
        && let Some(transient) = app.which_key_panel()
    {
        popup::WhichKeyPanel { transient }.render(frame, &app.theme);
    }
}

/// Draw the open modal, returning where it put its rows so the pointer can
/// find them. `None` for a dialog with nothing to point at.
fn draw_modal(frame: &mut Frame<'_>, app: &App) -> Option<popup::ListHits> {
    match &app.modal {
        Some(Modal::Confirm { message, .. }) => {
            popup::ConfirmDialog {
                message: message.clone(),
            }
            .render(frame, &app.theme);
            None
        }
        Some(Modal::Input {
            title,
            buffer,
            cursor,
            ..
        }) => {
            popup::InputModal {
                title: title.clone(),
                buffer: buffer.clone(),
                cursor: *cursor,
            }
            .render(frame, &app.theme);
            None
        }
        Some(Modal::Help) => {
            let screen = match app.screen() {
                Screen::Status => "status",
                Screen::Diff => "diff",
                Screen::Log => "log",
                Screen::Graph => "graph",
                Screen::Runs => "runs",
                Screen::Prs => "prs",
                Screen::CiLog => "logs",
                Screen::File => "file",
                Screen::Stats => "stats",
            };
            popup::Popup {
                title: format!("Help: {screen} keys"),
                entries: help_entries(app),
                summary: Vec::new(),
            }
            .render(frame, &app.theme);
            None
        }
        Some(
            Modal::BranchList { .. }
            | Modal::PrBase { .. }
            | Modal::RevList { .. }
            | Modal::Palette { .. }
            | Modal::Themes { .. }
            | Modal::FilePicker { .. }
            | Modal::RemoteList { .. },
        ) => fuzzy_modal(app).map(|modal| modal.render(frame, &app.theme)),
        Some(Modal::PullDiverged { upstream }) => {
            popup::Popup {
                title: format!("Diverged from {upstream}"),
                entries: vec![
                    ("r".to_owned(), "rebase your commits on top".to_owned()),
                    ("m".to_owned(), "merge".to_owned()),
                    ("f".to_owned(), "force (discard local commits)".to_owned()),
                    ("esc".to_owned(), "cancel".to_owned()),
                ],
                summary: Vec::new(),
            }
            .render(frame, &app.theme);
            None
        }
        Some(Modal::CreatePr { draft }) => {
            Some(popup::CreatePrForm { draft }.render(frame, &app.theme))
        }
        Some(Modal::ReviewVerdict { number, summary }) => {
            popup::Popup {
                title: format!("Submit review: PR #{number}"),
                entries: vec![
                    ("a".to_owned(), "approve".to_owned()),
                    ("x".to_owned(), "request changes".to_owned()),
                    ("c".to_owned(), "comment only".to_owned()),
                    ("esc".to_owned(), "cancel".to_owned()),
                ],
                summary: summary.clone(),
            }
            .render(frame, &app.theme);
            None
        }
        None => None,
    }
}

/// Dialog footer for the active focus: classic keys in list focus, the
/// filter hints while typing.
fn footer_for(list: &fuzzy::FuzzyList, list_keys: &str, verb: &str) -> String {
    match list.focus {
        fuzzy::FuzzyFocus::List => {
            format!(" enter{verb} · j/k move{list_keys} · tab filter · q close ")
        }
        fuzzy::FuzzyFocus::Input => {
            format!(" type to filter · enter{verb} · tab list · esc close ")
        }
    }
}

/// A dialog whose rows are plain labels, ranked through the list's matches.
fn plain_list(
    title: String,
    list: &fuzzy::FuzzyList,
    labels: &[String],
    verb: &str,
) -> popup::FuzzyModal {
    popup::FuzzyModal {
        title,
        query: list.query.clone(),
        cursor: list.cursor,
        typing: matches!(list.focus, fuzzy::FuzzyFocus::Input),
        items: list
            .matches
            .iter()
            .filter_map(|index| labels.get(*index))
            .map(|label| (label.clone(), String::new()))
            .collect(),
        selected: list.selected,
        footer: footer_for(list, "", verb),
    }
}

fn fuzzy_modal(app: &App) -> Option<popup::FuzzyModal> {
    match &app.modal {
        Some(Modal::BranchList {
            branches,
            list,
            action,
        }) => {
            let title = match action {
                BranchAction::Checkout => "Checkout branch",
                BranchAction::Delete => "Delete branch",
            };
            Some(popup::FuzzyModal {
                title: title.to_owned(),
                query: list.query.clone(),
                cursor: list.cursor,
                typing: matches!(list.focus, fuzzy::FuzzyFocus::Input),
                items: list
                    .matches
                    .iter()
                    .filter_map(|index| branches.get(*index))
                    .map(|b| {
                        (
                            format!("{} {}", if b.is_head { "*" } else { " " }, b.name),
                            String::new(),
                        )
                    })
                    .collect(),
                selected: list.selected,
                footer: footer_for(list, "", " select"),
            })
        }
        Some(Modal::RevList {
            title,
            entries,
            list,
        }) => {
            let labels: Vec<String> = entries.iter().map(|c| c.label.clone()).collect();
            Some(plain_list((*title).to_owned(), list, &labels, " review"))
        }
        Some(Modal::PrBase { names, list, .. }) => Some(plain_list(
            "Base branch".to_owned(),
            list,
            names,
            " set base",
        )),
        Some(Modal::Palette { list }) => {
            let commands = app.command_index();
            Some(popup::FuzzyModal {
                title: "Commands".to_owned(),
                query: list.query.clone(),
                cursor: list.cursor,
                typing: matches!(list.focus, fuzzy::FuzzyFocus::Input),
                items: list
                    .matches
                    .iter()
                    .filter_map(|index| commands.get(*index))
                    .map(|c| (c.label.to_owned(), c.chord.clone()))
                    .collect(),
                selected: list.selected,
                footer: footer_for(list, "", " run"),
            })
        }
        Some(Modal::Themes { list }) => Some(plain_list(
            "Theme".to_owned(),
            list,
            &crate::theme::names(),
            " apply",
        )),
        Some(Modal::RemoteList { remotes, list, .. }) => {
            Some(plain_list("Remote".to_owned(), list, remotes, " select"))
        }
        Some(Modal::FilePicker { paths, list }) => {
            let mut modal = plain_list(
                format!("File · {} tracked", paths.len()),
                list,
                paths,
                " open",
            );
            modal.footer = footer_for(list, " · b blame · e editor", " open");
            Some(modal)
        }
        _ => None,
    }
}

/// Help popup entries: the active keymap's leaves, then, on the status
/// screen, each transient's prefix and its grouped sub-keys, so the popup
/// documents the full two-level map.
fn help_entries(app: &App) -> Vec<(String, String)> {
    let keymap = app.active_keymap();
    let mut entries: Vec<(String, String)> = keymap
        .bindings()
        .iter()
        .map(|(chord, action)| (render_chord(chord), action.label().to_owned()))
        .collect();
    if app.screen() == Screen::Status {
        for kind in TransientKind::ALL {
            let Some(prefix) = keymap.prefix_chord(kind) else {
                continue;
            };
            entries.push((prefix, format!("{} …", kind.title())));
            for (key, entry) in app.transient(kind).flat_entries() {
                entries.push((format!("  {key}"), entry.label.to_owned()));
            }
        }
    }
    entries
}

/// Status accent shared by the diff sidebar and the status screen.
pub(super) fn status_color(theme: &Theme, status: FileStatus) -> Color {
    match status {
        FileStatus::Added | FileStatus::Untracked => theme.added,
        FileStatus::Deleted => theme.error_fg,
        FileStatus::Modified | FileStatus::Renamed => theme.warn_fg,
    }
}

/// Theme color for a CI job/run status, shared by the runs list and the inline
/// status section so the palette stays in one place.
pub(super) fn ci_status_color(theme: &Theme, status: crate::ci::JobStatus) -> Color {
    use crate::ci::JobStatus;
    match status {
        JobStatus::Ok => theme.added,
        JobStatus::Failed => theme.error_fg,
        JobStatus::Running => theme.warn_fg,
        JobStatus::Queued | JobStatus::Skipped | JobStatus::Neutral => theme.dim,
    }
}

/// GitHub-style ` +A -B` diffstat spans over `bg`. A zero side is dimmed so it
/// reads as inactive; both-zero yields no spans.
pub(super) fn diffstat_spans(
    theme: &Theme,
    added: usize,
    deleted: usize,
    bg: Color,
) -> Vec<Span<'static>> {
    if added == 0 && deleted == 0 {
        return Vec::new();
    }
    let side = |count: usize, color: Color| {
        let fg = if count == 0 { theme.dim } else { color };
        Style::new().fg(fg).bg(bg)
    };
    vec![
        Span::styled(format!(" +{added}"), side(added, theme.added)),
        Span::styled(format!(" -{deleted}"), side(deleted, theme.error_fg)),
    ]
}

/// Linguist's hue, lifted until it reads on this theme's background.
pub(super) fn language_color(theme: &Theme, color: language::Rgb) -> Color {
    let (r, g, b) = language::readable_on(color, rgb_of(theme.bg));
    Color::Rgb(r, g, b)
}

pub(super) fn rgb_of(color: Color) -> language::Rgb {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        // every bundled theme is truecolor; a terminal-palette colour has no
        // channels to compare, so treat it as the dark end
        _ => (0, 0, 0),
    }
}

/// Split `cells` between `shares` in proportion, by largest remainder, so the
/// pieces add up to exactly `cells` and no non-zero share rounds away to
/// nothing. A stacked bar is only honest if it fills its own width.
pub(super) fn allocate(shares: &[usize], cells: usize) -> Vec<usize> {
    let total: usize = shares.iter().sum();
    let counted = shares.iter().filter(|share| **share > 0).count();
    if total == 0 || cells == 0 || counted == 0 {
        return vec![0; shares.len()];
    }
    // every language that changed anything keeps a cell, so the rest of the
    // bar is what is left to share out
    if counted >= cells {
        return shares
            .iter()
            .scan(cells, |left, share| {
                let take = usize::from(*share > 0 && *left > 0);
                *left -= take;
                Some(take)
            })
            .collect();
    }
    let spare = cells - counted;
    let mut out: Vec<usize> = shares
        .iter()
        .map(|share| usize::from(*share > 0) + share * spare / total)
        .collect();
    let mut remainders: Vec<(usize, usize)> = shares
        .iter()
        .enumerate()
        .filter(|(_, share)| **share > 0)
        .map(|(index, share)| (index, share * spare % total))
        .collect();
    remainders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut short = cells - out.iter().sum::<usize>();
    for (index, _) in remainders {
        if short == 0 {
            break;
        }
        if let Some(cell) = out.get_mut(index) {
            *cell += 1;
            short -= 1;
        }
    }
    out
}

/// A ~5-cell bar split green:red by the added:deleted ratio over `bg`; at least
/// one cell goes to each non-zero side so neither vanishes. Empty with no
/// changes. Shared by the status total and the diff pane header.
pub(super) fn proportion_bar(
    theme: &Theme,
    added: usize,
    deleted: usize,
    bg: Color,
) -> Vec<Span<'static>> {
    const CELLS: usize = 5;
    let total = added + deleted;
    if total == 0 {
        return Vec::new();
    }
    let mut add_cells = (added * CELLS).div_ceil(total).min(CELLS);
    if added > 0 && add_cells == 0 {
        add_cells = 1;
    }
    if deleted > 0 && add_cells == CELLS {
        add_cells = CELLS - 1;
    }
    let del_cells = CELLS - add_cells;
    let mut spans = Vec::new();
    if add_cells > 0 {
        spans.push(Span::styled(
            "█".repeat(add_cells),
            Style::new().fg(theme.added).bg(bg),
        ));
    }
    if del_cells > 0 {
        spans.push(Span::styled(
            "█".repeat(del_cells),
            Style::new().fg(theme.error_fg).bg(bg),
        ));
    }
    spans
}

/// One hint entry: either leaf actions sharing a label, or a transient prefix.
/// Prefix entries render only the top-level key, keeping the hint line at the
/// prefix altitude (sub-commands live in the which-key panel and help popup).
pub(super) enum Hint {
    Leaf(&'static [Action], &'static str),
    Prefix(TransientKind, &'static str),
}

/// Hint line built from the active keymap so config remaps show. Leaf items
/// whose action lost its key to a remap are dropped; a prefix without a bound
/// key (a dropped conflict) is dropped too.
pub(super) fn hint_line(app: &App, items: &[Hint]) -> Line<'static> {
    let keymap = app.active_keymap();
    let mut parts: Vec<(String, &str)> = Vec::new();
    for item in items {
        match item {
            Hint::Leaf(actions, label) => {
                let chords: Vec<String> = actions
                    .iter()
                    .filter_map(|action| keymap.chord_for(*action))
                    .collect();
                if chords.len() == actions.len() {
                    parts.push((chords.join("/"), label));
                }
            }
            Hint::Prefix(kind, label) => {
                if let Some(chord) = keymap.prefix_chord(*kind) {
                    parts.push((chord, label));
                }
            }
        }
    }
    let dim = app.theme.dim_style();
    let key_style = Style::new().fg(app.theme.fg).bg(app.theme.bg);
    let mut spans = Vec::new();
    for (index, (chord, label)) in parts.into_iter().enumerate() {
        spans.push(Span::styled(if index == 0 { " " } else { " · " }, dim));
        spans.push(Span::styled(chord, key_style));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    Line::from(spans)
}

/// Repaint a row with the cursor-line background, padded to the full width
/// so the highlight spans the whole row.
/// Compact "time ago" for a commit time, neogit-style: `49s`, `6m`, `21h`,
/// `3d`, `2w`, `5mo`, `1y`. Future times (clock skew) clamp to `0s`.
pub(super) fn relative_time(now: i64, then: i64) -> String {
    let secs = (now - then).max(0);
    let (n, unit) = match secs {
        s if s < 60 => (s, "s"),
        s if s < 3600 => (s / 60, "m"),
        s if s < 86_400 => (s / 3600, "h"),
        s if s < 86_400 * 7 => (s / 86_400, "d"),
        s if s < 86_400 * 30 => (s / (86_400 * 7), "w"),
        s if s < 86_400 * 365 => (s / (86_400 * 30), "mo"),
        s => (s / (86_400 * 365), "y"),
    };
    format!("{n}{unit}")
}

/// Padding to right-align `content_width` cells within `width`, given `used`
/// cells already consumed. `None` when there is no room, so the left content
/// is never pushed off-screen. Shared by every right-aligned row column.
fn right_align_pad(used: usize, content_width: usize, width: usize) -> Option<usize> {
    (used + content_width < width).then(|| width - used - content_width)
}

/// Right-aligned author + commit age for a commit row, given the width already
/// used by the row's left content. Empty when there is no room, so the left
/// content (oid, subject) is never pushed off-screen.
pub(super) fn commit_meta_spans(
    theme: &Theme,
    author: &str,
    time_unix: i64,
    now: i64,
    used: usize,
    width: usize,
) -> Vec<Span<'static>> {
    let age = relative_time(now, time_unix);
    let content_width = author.chars().count() + 2 + age.chars().count() + 1;
    let Some(pad) = right_align_pad(used, content_width, width) else {
        return Vec::new();
    };
    vec![
        Span::styled(" ".repeat(pad), Style::new().bg(theme.bg)),
        Span::styled(
            author.to_owned(),
            Style::new().fg(theme.accent).bg(theme.bg),
        ),
        Span::styled("  ", Style::new().bg(theme.bg)),
        Span::styled(age, theme.dim_style()),
        Span::styled(" ", Style::new().bg(theme.bg)),
    ]
}

/// Right-aligned age for a branch row, given the width already used by the
/// row's left content: same padding arithmetic as [`commit_meta_spans`],
/// without the author column a branch row has no use for.
pub(super) fn age_spans(
    theme: &Theme,
    time_unix: i64,
    now: i64,
    used: usize,
    width: usize,
) -> Vec<Span<'static>> {
    let age = relative_time(now, time_unix);
    let content_width = age.chars().count() + 1;
    let Some(pad) = right_align_pad(used, content_width, width) else {
        return Vec::new();
    };
    vec![
        Span::styled(" ".repeat(pad), Style::new().bg(theme.bg)),
        Span::styled(age, theme.dim_style()),
        Span::styled(" ", Style::new().bg(theme.bg)),
    ]
}

/// Rows kept between the cursor and the edge of the viewport, vim's
/// `scrolloff`: the view starts moving before the cursor reaches the last row,
/// so there is always something to read ahead of it.
pub(super) const SCROLLOFF: usize = 3;

/// Scroll offset that keeps `cursor` inside a `height`-row viewport with
/// [`SCROLLOFF`] rows to spare: the top follows the cursor down before it hits
/// the bottom row and up before it hits the top one. The first and last rows of
/// the content have no room for a margin, so the cursor reaches them. `total`
/// is the row count; shared by every row-list pane so scrolling behaves
/// identically.
pub(super) fn scroll_to_cursor(cursor: usize, scroll: usize, height: usize, total: usize) -> usize {
    scroll_to_span(cursor, 1, scroll, height, total)
}

/// [`scroll_to_cursor`] for a cursor that spans several lines, which a wrapped
/// diff row does: `start` is its first line and `span` how many it occupies.
pub(super) fn scroll_to_span(
    start: usize,
    span: usize,
    scroll: usize,
    height: usize,
    total: usize,
) -> usize {
    if height == 0 {
        return 0;
    }
    // a viewport too short for two margins keeps the cursor centred instead
    let gap = SCROLLOFF.min(height.saturating_sub(1) / 2);
    let last = total.saturating_sub(height);
    let highest = start.saturating_sub(gap);
    let lowest = (start + span.max(1) + gap).saturating_sub(height);
    scroll.min(highest).max(lowest).min(last)
}

/// Truncate to `max` graphemes with an ellipsis. Shared by the runs list and
/// the status screen's inline CI section, which both fit run metadata into
/// fixed-width columns.
pub(super) fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// A list row under the cursor: the band across its full width, and its
/// leading cell given over to the accent bar. The flat lists share this so the
/// bar means one thing and a row holds its columns as the cursor arrives. The
/// diff sidebar builds its own equivalent, because its band also carries which
/// pane has focus.
pub(super) fn cursor_line(line: Line<'static>, theme: &Theme, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.bg(theme.cursor_line);
            Span::styled(span.content, style)
        })
        .collect();
    claim_lead_cell(&mut spans, theme);
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(
            " ".repeat(pad),
            Style::new().bg(theme.cursor_line),
        ));
    }
    Line::from(spans)
}

/// Rows lead with an indent cell for the bar to claim, so claiming it holds
/// every following column in place. A row leading with something wider, or
/// with nothing at all, takes the bar in front and shifts.
fn claim_lead_cell(spans: &mut Vec<Span<'static>>, theme: &Theme) {
    let bar = |bg| Span::styled("▌", Style::new().fg(theme.accent).bg(bg));
    let Some(first) = spans.first_mut() else {
        spans.push(bar(theme.cursor_line));
        return;
    };
    let mut rest = first.content.chars();
    match rest.next() {
        Some(lead) if lead.width().unwrap_or(0) == 1 => {
            let bg = first.style.bg.unwrap_or(theme.cursor_line);
            first.content = rest.collect::<String>().into();
            spans.insert(0, bar(bg));
        }
        _ => spans.insert(0, bar(theme.cursor_line)),
    }
}

/// Bottom bar shared by every screen: mode chip, repo@branch, MCP state,
/// viewed counts, and the transient message.
pub(super) fn status_bar(app: &App, width: u16) -> Line<'static> {
    let theme = &app.theme;
    let on_panel = |fg| Style::new().fg(fg).bg(theme.panel);
    // the chip is the mode indicator: a forge-backed review must read
    // differently from a local one, so the PR source names itself
    let chip = match app.screen() {
        Screen::Status => " STATUS ".to_owned(),
        Screen::Diff => match app.diff.as_ref().map(|d| &d.source) {
            Some(source @ diffler_core::source::ReviewSource::Pr { number }) => {
                let pending = app
                    .review
                    .session_for(source)
                    .comments
                    .iter()
                    .filter(|c| c.remote_id.is_none())
                    .count();
                if pending == 0 {
                    format!(" PR #{number} ")
                } else {
                    format!(" PR #{number} · {pending} pending ")
                }
            }
            Some(source @ diffler_core::source::ReviewSource::Against { .. }) => {
                format!(" DIFF {} ", source.label())
            }
            _ => " DIFF ".to_owned(),
        },
        Screen::Log => " LOG ".to_owned(),
        Screen::Graph => " GRAPH ".to_owned(),
        Screen::Runs => " RUNS ".to_owned(),
        Screen::Prs => " PRS ".to_owned(),
        Screen::CiLog => " LOGS ".to_owned(),
        Screen::File => " FILE ".to_owned(),
        Screen::Stats => " STATS ".to_owned(),
    };
    let repo = app
        .review
        .repo_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch = app.head.branch.clone().unwrap_or_else(|| "?".to_owned());
    let mut spans = vec![
        Span::styled(chip, theme.chip),
        Span::styled(format!(" {repo}@{branch}"), on_panel(theme.fg)),
    ];
    if let Some(port) = app.mcp_port {
        spans.push(Span::styled(format!(" · mcp :{port}"), on_panel(theme.dim)));
    } else if app.config.mcp.enabled {
        // server is configured but not yet bound (or failed)
        spans.push(Span::styled(" · mcp off", on_panel(theme.dim)));
    }
    if app.refresh_flash > 0 {
        spans.push(Span::styled(" · ↻", on_panel(theme.dim)));
    }
    let (files, viewed) = app.viewed_counts();
    if files > 0 {
        // the diff view is the review walk, so its counter reads as progress
        let text = if app.screen() == Screen::Diff {
            format!(" · viewed {viewed}/{files} files")
        } else {
            let noun = if files == 1 { "file" } else { "files" };
            format!(" · {files} {noun}, {viewed} viewed")
        };
        spans.push(Span::styled(text, on_panel(theme.dim)));
    }
    if let Some(search) = &app.search {
        let (i, n) = search.count();
        let count = if n == 0 {
            " [no match]".to_owned()
        } else {
            format!(" [{i}/{n}]")
        };
        spans.push(Span::styled(
            format!(" · /{}", search.query()),
            on_panel(theme.accent),
        ));
        spans.push(Span::styled(count, on_panel(theme.dim)));
    } else if let Some(message) = &app.message {
        let fg = match message.severity {
            Severity::Info => theme.dim,
            Severity::Warning => theme.warn_fg,
            Severity::Error => theme.error_fg,
        };
        let used: usize = spans.iter().map(Span::width).sum();
        let text = format!("{} ", message.text);
        let pad = (width as usize).saturating_sub(used + text.len());
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), on_panel(theme.fg)));
        } else {
            spans.push(Span::styled("  ", on_panel(theme.fg)));
        }
        spans.push(Span::styled(text, on_panel(fg)));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::{
        Line, SCROLLOFF, Span, Theme, cursor_line, highlight_spans_split, relative_time,
        scroll_to_cursor,
    };
    use ratatui::style::{Color, Style};

    /// Snapshots carry text only, so the styles a sidebar path row is made of
    /// are asserted here or nowhere.
    #[test]
    fn a_paths_parents_recede_behind_its_basename() {
        let theme = Theme::github_dark();
        let (dim, bright) = (
            Style::new().fg(theme.dim),
            Style::new().fg(Color::Rgb(1, 2, 3)),
        );
        let name = "app/diff/mod.rs";
        let split = name.rfind('/').map_or(0, |at| at + 1);

        let spans = highlight_spans_split(name, split, dim, bright, &[], &theme);

        let painted: Vec<(&str, Style)> = spans
            .iter()
            .map(|span| (span.content.as_ref(), span.style))
            .collect();
        assert_eq!(painted, vec![("app/diff/", dim), ("mod.rs", bright)]);
    }

    #[test]
    fn a_search_hit_stays_lit_across_the_parent_boundary() {
        let theme = Theme::github_dark();
        let (dim, bright) = (
            Style::new().fg(theme.dim),
            Style::new().fg(Color::Rgb(1, 2, 3)),
        );
        // "f/m" spans the last slash, so the match covers both styles
        let spans = highlight_spans_split("diff/mod.rs", 5, dim, bright, &[(3..6, true)], &theme);

        let lit: Vec<&str> = spans
            .iter()
            .filter(|span| span.style.bg == Some(theme.search_current))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(lit, vec!["f/", "m"], "both halves of the match are lit");
    }

    #[test]
    fn the_view_holds_still_until_the_cursor_reaches_its_margin() {
        // 20 rows on screen, 100 in the list, parked at the top
        let held = scroll_to_cursor(10, 0, 20, 100);
        assert_eq!(held, 0, "a cursor in the middle moves nothing");

        // walking down, the view starts moving SCROLLOFF rows short of the end
        assert_eq!(scroll_to_cursor(16, 0, 20, 100), 0);
        assert_eq!(scroll_to_cursor(17, 0, 20, 100), 1, "the margin is reached");
        assert_eq!(
            scroll_to_cursor(18, 1, 20, 100),
            2,
            "and keeps up from there"
        );
    }

    #[test]
    fn the_last_rows_are_reachable_without_a_margin() {
        // there is nothing below the final row to keep in view
        let scroll = scroll_to_cursor(99, 80, 20, 100);
        assert_eq!(scroll, 80, "the last screenful is the end of the scroll");
        assert_eq!(99 - scroll, 19, "the cursor still reaches the bottom row");
    }

    #[test]
    fn the_first_rows_are_reachable_without_a_margin() {
        assert_eq!(scroll_to_cursor(1, 5, 20, 100), 0, "the top pulls it home");
        assert_eq!(scroll_to_cursor(0, 0, 20, 100), 0);
    }

    #[test]
    fn a_short_viewport_keeps_the_cursor_centred() {
        // fewer rows than two margins: the gap shrinks rather than fighting
        for height in 1..=(SCROLLOFF * 2) {
            let scroll = scroll_to_cursor(50, 0, height, 100);
            assert!(
                (scroll..scroll + height).contains(&50),
                "the cursor stays on screen at height {height}"
            );
        }
    }

    #[test]
    fn the_chip_names_the_pr_when_reviewing_one() {
        use crate::app::App;
        use crate::config::LoadedConfig;
        use crate::test_support::standard_fixture;

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head");
        app.open_pr_diff(7, &head, &head);
        let bar = super::status_bar(&app, 80);
        let text: String = bar.spans.iter().map(|s| s.content.clone()).collect();
        assert!(text.contains(" PR #7 "), "{text}");
    }

    #[test]
    fn the_status_bar_names_the_revision_an_against_review_diffs_from() {
        use crate::app::App;

        let fixture = crate::test_support::branch_fixture();
        let mut app = App::new(fixture.review(), crate::config::LoadedConfig::default());
        app.open_against_diff("main");
        let bar = super::status_bar(&app, 80);
        let text: String = bar.spans.iter().map(|s| s.content.clone()).collect();
        assert!(text.contains(" DIFF vs main "), "{text}");
    }

    #[test]
    fn relative_time_picks_a_compact_unit() {
        let now = 1_000_000;
        assert_eq!(relative_time(now, now), "0s");
        assert_eq!(relative_time(now, now - 49), "49s");
        assert_eq!(relative_time(now, now - 6 * 60), "6m");
        assert_eq!(relative_time(now, now - 21 * 3600), "21h");
        assert_eq!(relative_time(now, now - 3 * 86_400), "3d");
        assert_eq!(relative_time(now, now - 2 * 7 * 86_400), "2w");
        assert_eq!(relative_time(now, now - 90 * 86_400), "3mo");
        assert_eq!(relative_time(now, now - 800 * 86_400), "2y");
        // future commit times (clock skew) clamp to 0s, never negative
        assert_eq!(relative_time(now, now + 500), "0s");
    }

    #[test]
    fn the_cursor_bar_takes_the_lead_cell_without_moving_the_row() {
        let theme = Theme::github_dark();
        let plain = Line::from(vec![
            Span::raw(" "),
            Span::styled("● src/lib.rs", theme.base()),
        ]);
        let under_cursor = cursor_line(plain.clone(), &theme, 40);
        let text: String = under_cursor
            .spans
            .iter()
            .map(|s| s.content.clone())
            .collect();
        assert!(text.starts_with("▌● src/lib.rs"), "{text}");
        assert_eq!(under_cursor.width(), 40, "the band spans the full width");
        // the glyph sits in the same column either way, so the list holds still
        let column = |line: &Line<'_>| {
            line.spans
                .iter()
                .flat_map(|s| s.content.chars().collect::<Vec<_>>())
                .position(|c| c == '●')
        };
        assert_eq!(column(&under_cursor), column(&plain));
    }

    #[test]
    fn a_row_with_no_lead_cell_to_spare_still_bands_exactly_its_width() {
        let theme = Theme::github_dark();
        for row in [Span::raw(""), Span::raw("世界"), Span::raw("x")] {
            let banded = cursor_line(Line::from(row.clone()), &theme, 6);
            let text: String = banded.spans.iter().map(|s| s.content.clone()).collect();
            assert!(text.starts_with('▌'), "{text}");
            assert_eq!(banded.width(), 6, "{:?} overflows its row", row.content);
        }
    }
}
