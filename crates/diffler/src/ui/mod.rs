//! Rendering. `draw` never computes review state; it reads `App` (the diff
//! view additionally fills its lazy highlight cache and follows the cursor
//! with its scroll offset, which is why it takes `&mut App`).

pub mod diff;
pub mod diff_render;
pub mod graph;
pub mod log;
pub mod logs;
pub mod popup;
mod prs;
mod runs;
pub mod status;

use diffler_core::model::FileStatus;
use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, BranchAction, Modal, Screen, Severity};
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
    if ranges.is_empty() {
        return vec![Span::styled(text.to_owned(), base)];
    }
    let snap = |i: usize| {
        let mut i = i.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let mut bounds = vec![0, text.len()];
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
        let style = bg_at(start).map_or(base, |bg| base.bg(bg));
        spans.push(Span::styled(segment.to_owned(), style));
    }
    spans
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    match app.screen() {
        Screen::Status => {
            // attach intra-line emphasis and syntax to expanded inline diffs
            // before the read-only status render
            app.enrich_status_expanded();
            app.ensure_status_highlights(diff::ensure_file_highlights);
            status::draw(frame, app);
        }
        Screen::Log => log::draw(frame, app),
        Screen::Diff => diff::draw(frame, app),
        Screen::Graph => graph::draw(frame, app),
        Screen::Runs => runs::draw(frame, app),
        Screen::Prs => prs::draw(frame, app),
        Screen::Logs => logs::draw(frame, app),
    }
    match &app.modal {
        Some(Modal::Confirm { message, .. }) => {
            popup::ConfirmDialog {
                message: message.clone(),
            }
            .render(frame, &app.theme);
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
        }
        Some(Modal::Help) => {
            let screen = match app.screen() {
                Screen::Status => "status",
                Screen::Diff => "diff",
                Screen::Log => "log",
                Screen::Graph => "graph",
                Screen::Runs => "runs",
                Screen::Prs => "prs",
                Screen::Logs => "logs",
            };
            popup::Popup {
                title: format!("Help — {screen} keys"),
                entries: help_entries(app),
            }
            .render(frame, &app.theme);
        }
        Some(Modal::BranchList {
            branches,
            cursor,
            action,
        }) => {
            let title = match action {
                BranchAction::Checkout => "Checkout branch",
                BranchAction::Delete => "Delete branch",
            };
            popup::ListModal {
                title: title.to_owned(),
                items: branches
                    .iter()
                    .map(|b| format!("{} {}", if b.is_head { "*" } else { " " }, b.name))
                    .collect(),
                cursor: *cursor,
            }
            .render(frame, &app.theme);
        }
        Some(Modal::Comments { entries, cursor }) => {
            popup::ListModal {
                title: format!("Comments — {}", app.active_review_source().label()),
                items: entries.iter().map(|e| e.label.clone()).collect(),
                cursor: *cursor,
            }
            .render(frame, &app.theme);
        }
        Some(Modal::ReviewVerdict { number }) => {
            popup::Popup {
                title: format!("Submit review — PR #{number}"),
                entries: vec![
                    ("a".to_owned(), "approve".to_owned()),
                    ("x".to_owned(), "request changes".to_owned()),
                    ("c".to_owned(), "comment only".to_owned()),
                    ("esc".to_owned(), "cancel".to_owned()),
                ],
            }
            .render(frame, &app.theme);
        }
        None => {}
    }
    // the which-key panel is a transient overlay, not a modal: it draws only
    // once the reveal timer has elapsed and never over a modal
    if app.modal.is_none()
        && let Some(transient) = app.which_key_panel()
    {
        popup::WhichKeyPanel { transient }.render(frame, &app.theme);
    }
}

/// Help popup entries: the active keymap's leaves, then — on the status
/// screen — each transient's prefix and its grouped sub-keys, so the popup
/// documents the full two-level map.
fn help_entries(app: &App) -> Vec<(String, String)> {
    let keymap = app.active_keymap();
    let mut entries: Vec<(String, String)> = keymap
        .bindings()
        .iter()
        .map(|(chord, action)| (render_chord(chord), action.name().to_owned()))
        .collect();
    if app.screen() == Screen::Status {
        for kind in TransientKind::ALL {
            let Some(prefix) = keymap.prefix_chord(kind) else {
                continue;
            };
            entries.push((prefix, format!("{} …", kind.title())));
            for (key, label) in app.transient(kind).flat_entries() {
                entries.push((format!("  {key}"), label.to_owned()));
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
    let mut parts = Vec::new();
    for item in items {
        match item {
            Hint::Leaf(actions, label) => {
                let chords: Vec<String> = actions
                    .iter()
                    .filter_map(|action| keymap.chord_for(*action))
                    .collect();
                if chords.len() == actions.len() {
                    parts.push(format!("{} {label}", chords.join("/")));
                }
            }
            Hint::Prefix(kind, label) => {
                if let Some(chord) = keymap.prefix_chord(*kind) {
                    parts.push(format!("{chord} {label}"));
                }
            }
        }
    }
    Line::styled(
        format!(" Hint: {}", parts.join("  ")),
        app.theme.dim_style(),
    )
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
    let meta_width = author.chars().count() + 2 + age.chars().count() + 1;
    if used + meta_width >= width {
        return Vec::new();
    }
    let pad = width - used - meta_width;
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

/// Scroll offset that keeps `cursor` inside a `height`-row viewport: pull the
/// top down to the cursor when it scrolls above, push it up when below. Shared
/// by every row-list pane so scrolling behaves identically.
pub(super) fn scroll_to_cursor(cursor: usize, scroll: usize, height: usize) -> usize {
    if cursor < scroll {
        cursor
    } else if cursor >= scroll + height {
        cursor + 1 - height
    } else {
        scroll
    }
}

pub(super) fn cursor_line(line: Line<'static>, theme: &Theme, width: u16) -> Line<'static> {
    let pad = (width as usize).saturating_sub(line.width());
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.bg(theme.cursor_line);
            Span::styled(span.content, style)
        })
        .collect();
    if pad > 0 {
        spans.push(Span::styled(
            " ".repeat(pad),
            Style::new().bg(theme.cursor_line),
        ));
    }
    Line::from(spans)
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
            Some(source @ crate::app::DiffSource::Pr { number }) => {
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
            _ => " DIFF ".to_owned(),
        },
        Screen::Log => " LOG ".to_owned(),
        Screen::Graph => " GRAPH ".to_owned(),
        Screen::Runs => " RUNS ".to_owned(),
        Screen::Prs => " PRS ".to_owned(),
        Screen::Logs => " LOGS ".to_owned(),
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
        spans.push(Span::styled(format!("  mcp :{port}"), on_panel(theme.dim)));
    } else if app.config.mcp.enabled {
        // server is configured but not yet bound (or failed)
        spans.push(Span::styled("  mcp off", on_panel(theme.dim)));
    }
    if app.refresh_flash > 0 {
        spans.push(Span::styled("  ↻", on_panel(theme.dim)));
    }
    let (files, viewed) = app.viewed_counts();
    if files > 0 {
        // the diff view is the review walk, so its counter reads as progress
        let text = if app.screen() == Screen::Diff {
            format!("  viewed {viewed}/{files} files")
        } else {
            let noun = if files == 1 { "file" } else { "files" };
            format!("  {files} {noun}, {viewed} viewed")
        };
        spans.push(Span::styled(text, on_panel(theme.dim)));
    }
    if let Some(search) = &app.search {
        let (i, n) = search.count();
        let count = if n == 0 {
            "  [no match]".to_owned()
        } else {
            format!("  [{i}/{n}]")
        };
        spans.push(Span::styled(
            format!("  /{}", search.query()),
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
    use super::relative_time;

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
}
