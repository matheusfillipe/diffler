//! Rendering. `draw` reads review state from `App` and computes none; it takes
//! `&mut App` for scroll offsets and the diff view's highlight cache.

pub mod ci_log;
pub mod diff;
pub mod diff_render;
pub mod file;
pub mod graph;
mod image_pane;
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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{AgentActivity, App, BranchAction, Modal, Screen, Severity, fuzzy};
use crate::keymap::{Action, render_chord};
use crate::theme::Theme;
use crate::transient::TransientKind;

/// Paint `/`-search matches over `text`. `ranges` are byte offsets, each
/// paired with whether it is the active match.
pub(super) fn highlight_spans(
    text: &str,
    base: Style,
    ranges: &[(std::ops::Range<usize>, bool)],
    theme: &Theme,
) -> Vec<Span<'static>> {
    highlight_spans_split(text, 0, base, base, ranges, theme)
}

/// [`highlight_spans`] with the leading `split` bytes styled `lead`, so a
/// path's parent directories can dim while its basename stays bright.
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

/// The frame below the project tab row, which we draw only while several
/// projects are open.
fn screen_area(frame: &mut Frame<'_>, app: &App) -> Rect {
    let area = frame.area();
    let Some(strip) = app.tab_strip.as_ref() else {
        return area;
    };
    let [row, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(Paragraph::new(tab_row(app, strip, row.width)), row);
    rest
}

fn tab_row(app: &App, strip: &crate::app::tabs::TabStrip, width: u16) -> Line<'static> {
    let theme = &app.theme;
    let bg = theme.panel;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, name) in strip.names.iter().enumerate() {
        let style = if index == strip.active {
            theme.chip.add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.dim).bg(bg)
        };
        spans.push(Span::styled(
            crate::app::tabs::TabStrip::label(index, name),
            style,
        ));
        spans.push(Span::styled(" ".to_owned(), Style::new().bg(bg)));
    }
    let tail: Vec<Span<'static>> = app
        .tabs_keymap()
        .chord_for(crate::keymap::Action::AddProject)
        .map(|chord| {
            vec![
                Span::styled(chord, Style::new().fg(theme.fg).bg(bg)),
                Span::styled(
                    crate::app::tabs::ADD_HINT.to_owned(),
                    Style::new().fg(theme.dim).bg(bg),
                ),
            ]
        })
        .unwrap_or_default();
    let used: usize = spans.iter().chain(&tail).map(Span::width).sum();
    spans.push(Span::styled(
        " ".repeat(usize::from(width).saturating_sub(used)),
        Style::new().bg(bg),
    ));
    spans.extend(tail);
    Line::from(spans)
}

/// Chrome for the `[hint, body, bar]` screens. Returns the body and bar
/// rects; the caller draws its own bar.
pub(super) fn screen_chrome(frame: &mut Frame<'_>, app: &App, hints: &[Hint]) -> (Rect, Rect) {
    let area = screen_area(frame, app);
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

/// [`screen_chrome`] with a header row under the hint line.
pub(super) fn screen_chrome_with_header(
    frame: &mut Frame<'_>,
    app: &App,
    hints: &[Hint],
) -> (Rect, Rect, Rect) {
    let area = screen_area(frame, app);
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
    app.frame_width = frame.area().width;
    match app.screen() {
        Screen::Status => {
            // expanded inline diffs render plain until their enrichment lands
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
    if app.modal.is_none()
        && let Some(which_key) = app.which_key_panel()
    {
        popup::WhichKeyPanel::new(&which_key).render(frame, &app.theme);
    }
}

/// Returns where the modal drew its rows, for mouse hit-testing.
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
            | Modal::Choice { .. }
            | Modal::FilePicker { .. }
            | Modal::AddProject { .. }
            | Modal::Menu { .. }
            | Modal::LanguageScope { .. }
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

fn command_modal(
    title: &str,
    commands: &[crate::app::Command],
    list: &fuzzy::FuzzyList,
    footer: String,
) -> popup::FuzzyModal {
    popup::FuzzyModal {
        title: title.to_owned(),
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
        footer,
    }
}

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

fn scope_modal(
    language: &str,
    scopes: &[crate::app::language::LanguageScope],
    list: &fuzzy::FuzzyList,
) -> popup::FuzzyModal {
    let labels: Vec<String> = scopes
        .iter()
        .map(crate::app::language::LanguageScope::label)
        .collect();
    plain_list(format!("Use {language}"), list, &labels, " choose")
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
        Some(Modal::Palette { list }) => Some(command_modal(
            "Commands",
            &app.command_index(),
            list,
            footer_for(list, "", " run"),
        )),
        Some(Modal::Choice { kind, list }) => {
            let current = kind.current(app);
            let labels: Vec<String> = kind
                .names()
                .into_iter()
                .map(|name| format!("{} {name}", if name == current { "*" } else { " " }))
                .collect();
            Some(plain_list(kind.title().to_owned(), list, &labels, " apply"))
        }
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
        Some(Modal::Menu { commands, list }) => Some(command_modal(
            "Actions",
            commands,
            list,
            " j/k move · enter run · esc close ".to_owned(),
        )),
        Some(Modal::LanguageScope {
            language,
            scopes,
            list,
            ..
        }) => Some(scope_modal(language, scopes, list)),
        Some(Modal::AddProject { entries, list, .. }) => {
            let mut modal = plain_list("Add project".to_owned(), list, entries, " open");
            " type a name or a path · tab complete · enter open · esc close "
                .clone_into(&mut modal.footer);
            Some(modal)
        }
        _ => None,
    }
}

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
    entries.extend(tab_help_entries(app.tabs_keymap()));
    entries
}

/// The nine go-to-tab keys fold into one row.
fn tab_help_entries(keymap: &crate::keymap::Keymap) -> Vec<(String, String)> {
    use crate::keymap::Action;
    let mut entries: Vec<(String, String)> = keymap
        .bindings()
        .iter()
        .filter(|(_, action)| crate::app::tabs::tab_op(*action).is_some())
        .filter(|(_, action)| {
            !matches!(
                crate::app::tabs::tab_op(*action),
                Some(crate::app::tabs::TabOp::Go(_))
            )
        })
        .map(|(chord, action)| (render_chord(chord), action.label().to_owned()))
        .collect();
    if let (Some(first), Some(last)) = (
        keymap.chord_for(Action::GoTab1),
        keymap.chord_for(Action::GoTab9),
    ) {
        entries.push((
            format!("{first}-{last}"),
            "switch to project tab N".to_owned(),
        ));
    }
    entries
}

pub(super) fn status_color(theme: &Theme, status: FileStatus) -> Color {
    match status {
        FileStatus::Added | FileStatus::Untracked => theme.added,
        FileStatus::Deleted => theme.error_fg,
        FileStatus::Modified | FileStatus::Renamed => theme.warn_fg,
        FileStatus::Unchanged => theme.dim,
    }
}

pub(super) fn ci_status_color(theme: &Theme, status: crate::ci::JobStatus) -> Color {
    use crate::ci::JobStatus;
    match status {
        JobStatus::Ok => theme.added,
        JobStatus::Failed => theme.error_fg,
        JobStatus::Running => theme.warn_fg,
        JobStatus::Queued | JobStatus::Skipped | JobStatus::Neutral => theme.dim,
    }
}

/// ` +A -B` over `bg`, with a zero side dimmed.
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
    let (r, g, b) = color;
    readable_on(Color::Rgb(r, g, b), theme.bg)
}

/// `fg` lifted until it clears 3:1 contrast on `bg`.
pub(super) fn readable_on(fg: Color, bg: Color) -> Color {
    let Color::Rgb(r, g, b) = fg else {
        return fg;
    };
    let (r, g, b) = language::readable_on((r, g, b), rgb_of(bg));
    Color::Rgb(r, g, b)
}

pub(super) fn rgb_of(color: Color) -> language::Rgb {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        // every bundled theme is truecolor, so we treat a palette colour as dark
        _ => (0, 0, 0),
    }
}

/// Split `cells` between `shares` by largest remainder, so the pieces sum to
/// exactly `cells` and no non-zero share rounds away to nothing.
pub(super) fn allocate(shares: &[usize], cells: usize) -> Vec<usize> {
    let total: usize = shares.iter().sum();
    let counted = shares.iter().filter(|share| **share > 0).count();
    if total == 0 || cells == 0 || counted == 0 {
        return vec![0; shares.len()];
    }
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

/// A 5-cell added:deleted bar, giving each non-zero side at least one cell.
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

/// A prefix hint shows only its own key; the which-key panel lists the rest.
pub(super) enum Hint {
    Leaf(&'static [Action], &'static str),
    Prefix(TransientKind, &'static str),
}

/// Built from the active keymap so remaps show; an entry with an unbound key
/// is dropped.
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

/// Compact "time ago": `49s`, `6m`, `21h`, `3d`, `2w`, `5mo`, `1y`. A future
/// time (clock skew) clamps to `0s`.
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

/// `None` when there is no room, so the left content stays on screen.
fn right_align_pad(used: usize, content_width: usize, width: usize) -> Option<usize> {
    (used + content_width < width).then(|| width - used - content_width)
}

/// Right-aligned author and age for a commit row, empty when there is no room.
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

/// Vim's `scrolloff`.
pub(super) const SCROLLOFF: usize = 3;

/// Scroll offset that keeps `cursor` [`SCROLLOFF`] rows from the viewport's
/// edges, except at the content's first and last rows.
pub(super) fn scroll_to_cursor(cursor: usize, scroll: usize, height: usize, total: usize) -> usize {
    scroll_to_span(cursor, 1, scroll, height, total)
}

/// [`scroll_to_cursor`] for a cursor spanning `span` lines from `start`, as a
/// wrapped diff row does.
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

pub(super) use crate::text::elide;

/// A list row under the cursor: banded to full width, its lead cell taken by
/// the accent bar. The diff sidebar draws its own, since its band also shows
/// pane focus.
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

/// A row banded to its full width in `bg`, keeping every span's foreground.
pub(super) fn fill_row(line: Line<'static>, bg: Color, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.bg(bg);
            Span::styled(span.content, style)
        })
        .collect();
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::new().bg(bg)));
    }
    Line::from(spans)
}

/// How far the reference band blends from the background toward the accent.
const REFERENCE_BAND: u16 = 25;

/// Band `rendered` when `index` falls inside the `referenced` span. We pick
/// the colour here so the diff pane and the file view always agree on it.
pub(super) fn band_referenced(
    rendered: Vec<Line<'static>>,
    referenced: Option<(usize, usize)>,
    index: usize,
    theme: &Theme,
    width: u16,
) -> Vec<Line<'static>> {
    if !referenced.is_some_and(|(start, end)| index >= start && index <= end) {
        return rendered;
    }
    let bg = crate::theme::blend(theme.bg, theme.accent, REFERENCE_BAND);
    rendered
        .into_iter()
        .map(|line| fill_row(line, bg, width))
        .collect()
}

/// The bar replaces a one-cell lead so the row's columns hold still; any
/// other row shifts right to make room.
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

/// `agent · <focus>[ · <file>]` in at most `room` cells, dropping the file
/// first, then eliding the focus.
fn agent_activity_spans(
    activity: &AgentActivity,
    theme: &Theme,
    on_panel: impl Fn(Color) -> Style,
    room: usize,
) -> Vec<Span<'static>> {
    const MIN_FOCUS: usize = 4;
    let lead = " · agent".width();
    if room < lead {
        return Vec::new();
    }
    let mut spans = vec![
        Span::styled(" · ", on_panel(theme.dim)),
        Span::styled("agent", on_panel(theme.purple)),
    ];
    let focus = format!(" · {}", activity.focus);
    let left = room - lead;
    if left < " · ".width() + MIN_FOCUS {
        return spans;
    }
    spans.push(Span::styled(elide(&focus, left), on_panel(theme.fg)));
    if let Some(file) = &activity.file {
        let file = format!(" · {file}");
        if focus.width() + file.width() <= left {
            spans.push(Span::styled(file, on_panel(theme.dim)));
        }
    }
    spans
}

fn mode_chip(app: &App) -> String {
    match app.screen() {
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
    }
}

pub(super) fn status_bar(app: &App, width: u16) -> Line<'static> {
    let theme = &app.theme;
    let on_panel = |fg| Style::new().fg(fg).bg(theme.panel);
    let chip = mode_chip(app);
    let repo = app.project_name();
    let branch = app.head.branch.clone().unwrap_or_else(|| "?".to_owned());
    let mut spans = vec![
        Span::styled(chip, theme.chip),
        Span::styled(format!(" {repo}@{branch}"), on_panel(theme.fg)),
    ];
    if let Some(port) = app.mcp_port {
        spans.push(Span::styled(format!(" · mcp :{port}"), on_panel(theme.dim)));
    } else if app.config.mcp.enabled {
        spans.push(Span::styled(" · mcp off", on_panel(theme.dim)));
    }
    if app.refresh_flash > 0 {
        spans.push(Span::styled(" · ↻", on_panel(theme.dim)));
    }
    let (files, viewed) = app.viewed_counts();
    if files > 0 {
        let text = if app.screen() == Screen::Diff {
            format!(" · viewed {viewed}/{files} files")
        } else {
            let noun = if files == 1 { "file" } else { "files" };
            format!(" · {files} {noun}, {viewed} viewed")
        };
        spans.push(Span::styled(text, on_panel(theme.dim)));
    }
    let mut tail = Vec::new();
    if let Some(search) = &app.search {
        let (i, n) = search.count();
        let count = if n == 0 {
            " [no match]".to_owned()
        } else {
            format!(" [{i}/{n}]")
        };
        tail.push(Span::styled(
            format!(" · /{}", search.query()),
            on_panel(theme.accent),
        ));
        tail.push(Span::styled(count, on_panel(theme.dim)));
    }
    let message = app
        .message
        .as_ref()
        .filter(|_| app.search.is_none())
        .map(|message| {
            let fg = match message.severity {
                Severity::Info => theme.dim,
                Severity::Warning => theme.warn_fg,
                Severity::Error => theme.error_fg,
            };
            Span::styled(format!("{} ", message.text), on_panel(fg))
        });
    if app.config.ui.show_agent_activity
        && let Some(activity) = &app.agent_activity.current
    {
        let used: usize = spans.iter().chain(&tail).map(Span::width).sum();
        let reserved = message
            .as_ref()
            .map_or(0, |message| message.content.width() + 2);
        let room = (width as usize).saturating_sub(used + reserved);
        spans.extend(agent_activity_spans(activity, theme, on_panel, room));
    }
    spans.extend(tail);
    if let Some(message) = message {
        let used: usize = spans.iter().map(Span::width).sum();
        let pad = (width as usize).saturating_sub(used + message.content.width());
        let pad = if pad > 0 { pad } else { 2 };
        spans.push(Span::styled(" ".repeat(pad), on_panel(theme.fg)));
        spans.push(message);
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

    /// Snapshots carry text only, so we assert a path row's styles here.
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
    fn the_status_bar_shows_the_agents_live_activity() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::Paragraph;

        use crate::app::App;
        use crate::config::LoadedConfig;
        use crate::test_support::standard_fixture;

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.set_agent_activity("writing the walkthrough", Some("src/lib.rs"));

        let mut terminal = Terminal::new(TestBackend::new(100, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                let bar = super::status_bar(&app, 100);
                frame.render_widget(Paragraph::new(bar), frame.area());
            })
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn agent_activity_gives_way_to_the_message_on_a_narrow_bar() {
        use crate::app::App;
        use crate::config::LoadedConfig;
        use crate::test_support::standard_fixture;

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.set_agent_activity(&"reading ".repeat(20), Some("src/app/refresh.rs"));
        app.error("push failed");

        let bar = super::status_bar(&app, 80);
        let text: String = bar.spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(bar.width(), 80, "{text}");
        assert!(text.ends_with("push failed "), "{text}");
        assert!(text.contains("agent · rea…"), "{text}");
        assert!(!text.contains("refresh.rs"), "{text}");
    }

    #[test]
    fn a_bar_too_narrow_for_the_focus_keeps_just_the_agent() {
        use crate::app::App;
        use crate::config::LoadedConfig;
        use crate::test_support::standard_fixture;

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        app.set_agent_activity("writing the walkthrough", Some("src/lib.rs"));

        let bar = super::status_bar(&app, 60);
        let text: String = bar.spans.iter().map(|s| s.content.clone()).collect();
        assert!(text.ends_with("· agent"), "{text}");
    }

    #[test]
    fn a_wide_glyph_focus_still_keeps_the_bar_at_its_own_width() {
        use crate::app::App;
        use crate::config::LoadedConfig;
        use crate::test_support::standard_fixture;

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.set_agent_activity(&"读写".repeat(20), Some("src/app/refresh.rs"));
        app.error("push failed");

        let bar = super::status_bar(&app, 80);
        assert_eq!(bar.width(), 80);
    }

    #[test]
    fn hiding_agent_activity_drops_it_from_the_status_bar() {
        use crate::app::App;
        use crate::config::LoadedConfig;
        use crate::test_support::standard_fixture;

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.config.ui.show_agent_activity = false;
        app.set_agent_activity("writing the walkthrough", None);

        let bar = super::status_bar(&app, 80);
        let text: String = bar.spans.iter().map(|s| s.content.clone()).collect();
        assert!(!text.contains("agent"), "{text}");
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
