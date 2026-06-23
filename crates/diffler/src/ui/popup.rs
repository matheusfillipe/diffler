//! Transient popup framework, neogit-style: an action popup rendered as a
//! bottom split, plus confirm, input, and pick-one list modals.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::theme::Theme;
use crate::transient::Transient;

/// Neogit-style action popup: a titled bottom panel listing
/// `key → action` entries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Popup {
    pub title: String,
    /// `(key label, description)` pairs.
    pub entries: Vec<(String, String)>,
}

/// Cells between columns when the help popup wraps into multiple columns.
const POPUP_COLUMN_GAP: usize = 2;

impl Popup {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let area = frame.area();
        // rows available under the top border; entries wrap into columns to
        // fit rather than overflowing off the top of the screen
        let body_rows = area.height.saturating_sub(1) as usize;
        let lines = self.lines(theme, body_rows.max(1));
        // +1 for the top border carrying the title
        let height = (lines.len() as u16 + 1).min(area.height);
        let popup_area = Rect {
            x: area.x,
            y: area.y + area.height - height,
            width: area.width,
            height,
        };
        frame.render_widget(Clear, popup_area);
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(theme.border).bg(theme.panel))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::new()
                    .fg(theme.accent)
                    .bg(theme.panel)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(block),
            popup_area,
        );
    }

    fn lines(&self, theme: &Theme, body_rows: usize) -> Vec<Line<'static>> {
        let key_style = Style::new()
            .fg(theme.purple)
            .bg(theme.panel)
            .add_modifier(Modifier::BOLD);
        let dim = Style::new().fg(theme.dim).bg(theme.panel);
        let fg = Style::new().fg(theme.fg).bg(theme.panel);

        let mut lines = vec![Line::styled("Actions", dim)];
        // one row goes to the "Actions" heading
        let rows = body_rows.saturating_sub(1).max(1);
        if self.entries.len() <= rows {
            for (key, description) in &self.entries {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {key}"), key_style),
                    Span::styled(format!("  {description}"), fg),
                ]));
            }
            return lines;
        }
        // too tall for one column: wrap column-major into the fewest columns
        // that fit the height, each padded to its own widest cell
        let columns = self.entries.len().div_ceil(rows);
        let per_column = self.entries.len().div_ceil(columns);
        let cell_width =
            |entry: &(String, String)| 1 + entry.0.chars().count() + 2 + entry.1.chars().count();
        let widths: Vec<usize> = (0..columns)
            .map(|column| {
                (0..per_column)
                    .filter_map(|row| self.entries.get(column * per_column + row))
                    .map(cell_width)
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for row in 0..per_column {
            let mut spans = Vec::new();
            for column in 0..columns {
                let Some(entry) = self.entries.get(column * per_column + row) else {
                    continue;
                };
                let width = widths
                    .get(column)
                    .copied()
                    .unwrap_or_else(|| cell_width(entry));
                let pad = width.saturating_sub(cell_width(entry)) + POPUP_COLUMN_GAP;
                spans.push(Span::styled(format!(" {}", entry.0), key_style));
                spans.push(Span::styled(
                    format!("  {}{}", entry.1, " ".repeat(pad)),
                    fg,
                ));
            }
            lines.push(Line::from(spans));
        }
        lines
    }
}

/// Cells of horizontal space between which-key columns.
const WHICH_KEY_COL_SPACING: usize = 2;
/// Cells between a key and its label within a column.
const WHICH_KEY_KEY_SEP: usize = 2;
/// Most rows the which-key panel uses, borrowed from the bottom of the screen.
const WHICH_KEY_MAX_HEIGHT: u16 = 12;

/// One column of the which-key panel: a group heading and its `(key, label)`
/// entries. Width is computed once so packing stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WhichKeyColumn {
    heading: String,
    entries: Vec<(String, String)>,
    width: usize,
}

impl WhichKeyColumn {
    fn new(heading: String, entries: Vec<(String, String)>) -> Self {
        let body = entries
            .iter()
            .map(|(key, label)| key.chars().count() + WHICH_KEY_KEY_SEP + label.chars().count())
            .max()
            .unwrap_or(0);
        let width = body.max(heading.chars().count());
        Self {
            heading,
            entries,
            width,
        }
    }
}

/// Pack columns into bands (rows of column indices) so each band fits within
/// `available` cells. Greedy left-to-right: a column starts a new band when it
/// no longer fits, matching which-key.nvim's layout. A column wider than
/// `available` still takes its own band. Pure, so the layout is unit-tested.
fn pack_columns(widths: &[usize], available: usize) -> Vec<Vec<usize>> {
    let mut bands: Vec<Vec<usize>> = Vec::new();
    let mut used = 0usize;
    for (index, &width) in widths.iter().enumerate() {
        let needs = if bands.last().is_some_and(|b| !b.is_empty()) {
            WHICH_KEY_COL_SPACING + width
        } else {
            width
        };
        match bands.last_mut() {
            Some(band) if !band.is_empty() && used + needs <= available => {
                band.push(index);
                used += needs;
            }
            _ => {
                bands.push(vec![index]);
                used = width;
            }
        }
    }
    bands
}

/// The which-key bottom panel: a transient's groups laid out as packed columns
/// of `key  label`, revealed after the reveal timer elapses.
#[derive(Debug, Clone)]
pub struct WhichKeyPanel<'a> {
    pub transient: &'a Transient,
}

impl WhichKeyPanel<'_> {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let area = frame.area();
        let columns = self.columns();
        let widths: Vec<usize> = columns.iter().map(|c| c.width).collect();
        let available = (area.width as usize).saturating_sub(2).max(1);
        let bands = pack_columns(&widths, available);
        let lines = render_bands(&columns, &bands, theme);
        // +1 for the top border carrying the title
        let height = (lines.len() as u16 + 1)
            .min(WHICH_KEY_MAX_HEIGHT)
            .min(area.height);
        let panel_area = Rect {
            x: area.x,
            y: area.y + area.height - height,
            width: area.width,
            height,
        };
        frame.render_widget(Clear, panel_area);
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(theme.border).bg(theme.panel))
            .title(Span::styled(
                format!(" {} ", self.transient.kind.title()),
                Style::new()
                    .fg(theme.accent)
                    .bg(theme.panel)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(block),
            panel_area,
        );
    }

    fn columns(&self) -> Vec<WhichKeyColumn> {
        self.transient
            .groups
            .iter()
            .map(|group| {
                let entries = group
                    .entries
                    .iter()
                    .map(|entry| {
                        (
                            crate::keymap::render_chord(std::slice::from_ref(&entry.key)),
                            entry.label.to_owned(),
                        )
                    })
                    .collect();
                WhichKeyColumn::new(group.heading.to_owned(), entries)
            })
            .collect()
    }
}

/// Render packed bands to styled lines: each band shows its columns' headings
/// on one row, then their entries row by row, padded to column width.
fn render_bands(
    columns: &[WhichKeyColumn],
    bands: &[Vec<usize>],
    theme: &Theme,
) -> Vec<Line<'static>> {
    let key_style = Style::new()
        .fg(theme.purple)
        .bg(theme.panel)
        .add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(theme.dim).bg(theme.panel);
    let fg = Style::new().fg(theme.fg).bg(theme.panel);
    let sep = " ".repeat(WHICH_KEY_COL_SPACING);

    let mut lines = Vec::new();
    for band in bands {
        let band_columns: Vec<&WhichKeyColumn> =
            band.iter().filter_map(|&col| columns.get(col)).collect();
        let mut heading = vec![Span::styled(" ".to_owned(), dim)];
        for (slot, column) in band_columns.iter().enumerate() {
            if slot > 0 {
                heading.push(Span::styled(sep.clone(), dim));
            }
            heading.push(Span::styled(pad(&column.heading, column.width), dim));
        }
        lines.push(Line::from(heading));

        let rows = band_columns
            .iter()
            .map(|column| column.entries.len())
            .max()
            .unwrap_or(0);
        for row in 0..rows {
            let mut spans = vec![Span::styled(" ".to_owned(), fg)];
            for (slot, column) in band_columns.iter().enumerate() {
                if slot > 0 {
                    spans.push(Span::styled(sep.clone(), fg));
                }
                match column.entries.get(row) {
                    Some((key, label)) => {
                        let used = key.chars().count() + WHICH_KEY_KEY_SEP + label.chars().count();
                        let pad = column.width.saturating_sub(used);
                        spans.push(Span::styled(key.clone(), key_style));
                        spans.push(Span::styled(" ".repeat(WHICH_KEY_KEY_SEP), fg));
                        spans.push(Span::styled(format!("{label}{}", " ".repeat(pad)), fg));
                    }
                    None => spans.push(Span::styled(" ".repeat(column.width), fg)),
                }
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// Right-pad `text` to `width` cells.
fn pad(text: &str, width: usize) -> String {
    let pad = width.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(pad))
}

/// Yes/no question rendered as a small centered modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmDialog {
    pub message: String,
}

impl ConfirmDialog {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let width = (self.message.len() as u16 + 4).clamp(24, frame.area().width);
        let area = centered(frame.area(), width, 4);
        frame.render_widget(Clear, area);
        let block = bordered_block(theme, " Confirm ");
        let body = vec![
            Line::styled(
                self.message.clone(),
                Style::new().fg(theme.fg).bg(theme.panel),
            ),
            Line::styled(
                "y confirm   n cancel",
                Style::new().fg(theme.dim).bg(theme.panel),
            ),
        ];
        frame.render_widget(
            Paragraph::new(body)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(block),
            area,
        );
    }
}

/// Multi-line text input modal with a visible cursor cell. The buffer may
/// hold newlines; the modal grows with it up to a cap, then shows the tail
/// (the cursor lives near the end while typing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputModal {
    pub title: String,
    pub buffer: String,
    /// Cursor position as a character index into `buffer`.
    pub cursor: usize,
}

/// Buffer lines visible at once before the modal stops growing.
const INPUT_MAX_LINES: usize = 8;

/// Width of the input box (clamped to the terminal), comfortable for prose.
const INPUT_WIDTH: u16 = 72;

impl InputModal {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let box_w = INPUT_WIDTH.min(frame.area().width.max(8));
        let inner = (box_w as usize).saturating_sub(2).max(1);
        let mut lines = self.wrapped_lines(theme, inner);
        // keep the tail visible (the cursor sits where you're typing)
        let overflow = lines.len().saturating_sub(INPUT_MAX_LINES);
        lines.drain(..overflow);
        lines.push(Line::styled(
            "enter submit  ·  a-enter newline  ·  esc cancel",
            Style::new().fg(theme.dim).bg(theme.panel),
        ));
        // +2 for the borders
        let height = lines.len() as u16 + 2;
        let area = centered(frame.area(), box_w, height);
        frame.render_widget(Clear, area);
        let block = bordered_block(theme, &format!(" {} ", self.title));
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(block),
            area,
        );
    }

    /// The buffer as display lines, each logical line word-wrapped to `width`,
    /// with the cursor drawn as a highlighted cell at its char position. A
    /// cursor at a line's end (on the newline) renders as a trailing cell.
    fn wrapped_lines(&self, theme: &Theme, width: usize) -> Vec<Line<'static>> {
        let fg = Style::new().fg(theme.fg).bg(theme.panel);
        let cursor_cell = Style::new().fg(theme.bg).bg(theme.accent);
        let mut lines = Vec::new();
        let mut offset = 0usize;
        for logical in self.buffer.split('\n') {
            let chars: Vec<char> = logical.chars().collect();
            let len = chars.len();
            let cursor_here = (offset..=offset + len).contains(&self.cursor);
            let cursor_col = self.cursor.saturating_sub(offset);
            for (start, end) in wrap_ranges(&chars, width) {
                let last = end >= len;
                let owns = cursor_here
                    && cursor_col >= start
                    && (cursor_col < end || (last && cursor_col == len));
                if owns {
                    let col = cursor_col - start;
                    let before: String = chars
                        .get(start..start + col)
                        .unwrap_or(&[])
                        .iter()
                        .collect();
                    let at = chars
                        .get(start + col)
                        .map_or_else(|| " ".to_owned(), char::to_string);
                    let rest = (start + col + 1).min(end);
                    let after: String = chars.get(rest..end).unwrap_or(&[]).iter().collect();
                    lines.push(Line::from(vec![
                        Span::styled(before, fg),
                        Span::styled(at, cursor_cell),
                        Span::styled(after, fg),
                    ]));
                } else {
                    let seg: String = chars.get(start..end).unwrap_or(&[]).iter().collect();
                    lines.push(Line::styled(seg, fg));
                }
            }
            offset += len + 1;
        }
        lines
    }
}

/// Char ranges to break a logical line into display segments of at most `width`,
/// preferring a break after the last space so words stay intact (long words hard
/// break). Every char lands in exactly one range, so cursor offsets stay exact.
/// An empty line yields a single empty range so it still renders (and can hold
/// the cursor).
fn wrap_ranges(chars: &[char], width: usize) -> Vec<(usize, usize)> {
    let len = chars.len();
    if len == 0 {
        return vec![(0, 0)];
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < len {
        let mut end = (start + width).min(len);
        if end < len
            && let Some(space) = (start..end).rev().find(|&i| chars.get(i) == Some(&' '))
            && space > start
        {
            end = space + 1; // keep the space on this line, wrap the next word
        }
        ranges.push((start, end));
        start = end;
    }
    ranges
}

/// Centered pick-one list (branches) with a j/k cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListModal {
    pub title: String,
    pub items: Vec<String>,
    pub cursor: usize,
}

impl ListModal {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let hint = "j/k move  enter select  esc back";
        let width = self
            .items
            .iter()
            .map(|item| item.chars().count())
            .chain([hint.len(), self.title.chars().count() + 2])
            .max()
            .unwrap_or(0) as u16
            + 4;
        // +3: borders and the hint line
        let height = self.items.len() as u16 + 3;
        let area = centered(frame.area(), width.min(frame.area().width), height);
        frame.render_widget(Clear, area);
        let mut lines: Vec<Line<'static>> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let style = if index == self.cursor {
                    Style::new().fg(theme.fg).bg(theme.cursor_line)
                } else {
                    Style::new().fg(theme.fg).bg(theme.panel)
                };
                Line::styled(format!(" {item} "), style)
            })
            .collect();
        lines.push(Line::styled(
            hint,
            Style::new().fg(theme.dim).bg(theme.panel),
        ));
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(bordered_block(theme, &format!(" {} ", self.title))),
            area,
        );
    }
}

fn bordered_block(theme: &Theme, title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border).bg(theme.panel))
        .title(Span::styled(
            title.to_owned(),
            Style::new()
                .fg(theme.accent)
                .bg(theme.panel)
                .add_modifier(Modifier::BOLD),
        ))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Block;

    use super::*;

    /// Render a widget over a themed background so the split/overlay
    /// boundaries are visible in the snapshot.
    fn render(draw: impl Fn(&mut Frame<'_>, &Theme)) -> Terminal<TestBackend> {
        let theme = Theme::github_dark();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(Block::new().style(theme.base()), frame.area());
                frame.render_widget(
                    ratatui::text::Text::from("status screen content behind the popup"),
                    frame.area(),
                );
                draw(frame, &theme);
            })
            .expect("draw");
        terminal
    }

    #[test]
    fn pack_columns_keeps_fitting_columns_on_one_band() {
        // three 10-wide columns with 2-cell spacing need 10+2+10+2+10 = 34
        let bands = pack_columns(&[10, 10, 10], 34);
        assert_eq!(bands, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn pack_columns_wraps_when_the_next_column_overflows() {
        // 34 wide fits all three; 33 pushes the third to a second band
        assert_eq!(pack_columns(&[10, 10, 10], 33), vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn pack_columns_gives_an_oversized_column_its_own_band() {
        assert_eq!(pack_columns(&[40, 5], 20), vec![vec![0], vec![1]]);
    }

    #[test]
    fn which_key_column_width_covers_key_label_and_heading() {
        let column = WhichKeyColumn::new(
            "Create and switch branches".to_owned(),
            vec![("c".to_owned(), "Create".to_owned())],
        );
        // the heading is wider than `c  Create`, so it sets the width
        assert_eq!(column.width, "Create and switch branches".chars().count());

        let column = WhichKeyColumn::new(
            "X".to_owned(),
            vec![("D".to_owned(), "Delete branch".to_owned())],
        );
        // body width: key(1) + sep(2) + label("Delete branch")
        assert_eq!(
            column.width,
            1 + WHICH_KEY_KEY_SEP + "Delete branch".chars().count()
        );
    }

    #[test]
    fn which_key_panel_renders_the_commit_transient() {
        let (transient, warnings) = crate::transient::Transient::build(
            crate::transient::TransientKind::Commit,
            &crate::config::KeysConfig::default(),
        );
        assert!(warnings.is_empty());
        let terminal = render(|frame, theme| {
            WhichKeyPanel {
                transient: &transient,
            }
            .render(frame, theme);
        });
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn which_key_panel_renders_the_stash_transient() {
        let (transient, warnings) = crate::transient::Transient::build(
            crate::transient::TransientKind::Stash,
            &crate::config::KeysConfig::default(),
        );
        assert!(warnings.is_empty());
        let terminal = render(|frame, theme| {
            WhichKeyPanel {
                transient: &transient,
            }
            .render(frame, theme);
        });
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn popup_renders_as_bottom_split() {
        let popup = Popup {
            title: "Branch".to_owned(),
            entries: vec![
                ("c".to_owned(), "create and checkout".to_owned()),
                ("n".to_owned(), "create".to_owned()),
                ("D".to_owned(), "delete".to_owned()),
            ],
        };
        let terminal = render(|frame, theme| popup.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn popup_wraps_into_columns_when_too_tall_to_fit() {
        // more entries than the 40-row test screen can stack in one column
        let entries: Vec<(String, String)> = (0..60)
            .map(|i| (format!("k{i}"), format!("action_{i}")))
            .collect();
        let popup = Popup {
            title: "Many keys".to_owned(),
            entries,
        };
        let terminal = render(|frame, theme| popup.render(frame, theme));
        let content = terminal.backend().to_string();
        // the last entry would overflow a single column but a second column
        // keeps every binding on screen
        assert!(content.contains("action_0"), "{content}");
        assert!(content.contains("action_59"), "{content}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn confirm_dialog_renders_centered() {
        let dialog = ConfirmDialog {
            message: "Discard changes to src/lib.rs?".to_owned(),
        };
        let terminal = render(|frame, theme| dialog.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn input_modal_renders_with_cursor() {
        let modal = InputModal {
            title: "New branch".to_owned(),
            buffer: "feat/m1".to_owned(),
            cursor: 7,
        };
        let terminal = render(|frame, theme| modal.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn input_modal_renders_a_two_line_buffer() {
        let modal = InputModal {
            title: "Comment".to_owned(),
            buffer: "first line\nsecond".to_owned(),
            cursor: 17,
        };
        let terminal = render(|frame, theme| modal.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn input_modal_wraps_a_long_line_onto_multiple_rows() {
        // a single logical line longer than the box wraps instead of overflowing
        let long = "the quick brown fox jumps over the lazy dog and then keeps on \
                    running well past the right edge of the comment box";
        let modal = InputModal {
            title: "Comment".to_owned(),
            buffer: long.to_owned(),
            cursor: long.chars().count(),
        };
        let terminal = render(|frame, theme| modal.render(frame, theme));
        let content = terminal.backend().to_string();
        // the long line wrapped across rows with words kept intact
        assert!(content.contains("keeps on running"), "{content}");
        assert!(content.contains("well past the right edge"), "{content}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn input_modal_overflow_shows_the_last_lines() {
        let buffer = (1..=12)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let modal = InputModal {
            title: "Comment".to_owned(),
            cursor: buffer.chars().count(),
            buffer,
        };
        let terminal = render(|frame, theme| modal.render(frame, theme));
        let content = terminal.backend().to_string();
        assert!(content.contains("line 12"), "tail stays visible: {content}");
        assert!(content.contains("line 5 "), "8 lines fit: {content}");
        assert!(
            !content.contains("line 4 "),
            "older lines scroll away: {content}"
        );
    }
}
