//! Rendering. `draw` never computes review state; it reads `App` (the diff
//! view additionally fills its lazy highlight cache and follows the cursor
//! with its scroll offset, which is why it takes `&mut App`).

pub mod diff;
pub mod diff_render;
pub mod log;
pub mod popup;
pub mod status;

use diffler_core::model::FileStatus;
use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, BranchAction, Modal, Screen, Severity};
use crate::keymap::{Action, render_chord};
use crate::theme::Theme;
use crate::transient::TransientKind;

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
        FileStatus::Added | FileStatus::Untracked => theme.accent,
        FileStatus::Deleted => theme.error_fg,
        FileStatus::Modified | FileStatus::Renamed => theme.warn_fg,
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
    let chip = match app.screen() {
        Screen::Status => " STATUS ",
        Screen::Diff => " DIFF ",
        Screen::Log => " LOG ",
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
    if let Some(message) = &app.message {
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
