//! The diff pane's in-place comment editor. It occupies the rows its result
//! will occupy, so writing a comment and reading it back look the same.

use crossterm::event::{KeyCode, KeyEvent};
use diffler_core::session::Anchor;
use unicode_width::UnicodeWidthChar;

use super::{App, DiffRow, Flow, text_edit};
use crate::editor::{EditorPurpose, TextBoxTarget};
use crate::keymap::Action;

/// What the composer will do with its buffer once submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerKind {
    New { anchor: Anchor },
    Reply { comment_id: String },
    Edit { comment_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composer {
    pub kind: ComposerKind,
    pub buffer: String,
    /// Char index into `buffer`.
    pub cursor: usize,
}

/// One rendered row of the composer card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerLine {
    Header,
    /// A visual row of the wrapped buffer, plus the column the text cursor
    /// sits at when it is on this row.
    Body {
        text: String,
        cursor: Option<usize>,
    },
    Footer,
}

impl Composer {
    pub fn new(kind: ComposerKind, buffer: String) -> Self {
        Self {
            cursor: buffer.chars().count(),
            buffer,
            kind,
        }
    }

    pub fn apply(&mut self, key: &KeyEvent, row_width: u16) -> text_edit::Edit {
        match key.code {
            KeyCode::Up => self.step_visual_row(row_width, false),
            KeyCode::Down => self.step_visual_row(row_width, true),
            _ => return text_edit::apply(&mut self.buffer, &mut self.cursor, key),
        }
        text_edit::Edit::Consumed
    }

    /// Move the caret one drawn (wrapped) row, holding its column where the
    /// destination is long enough.
    fn step_visual_row(&mut self, row_width: u16, down: bool) {
        let rows = wrap_rows(&self.buffer, card_budget(row_width));
        let caret = self.caret_line(row_width);
        // display index 0 is the header
        let Some(from) = caret.checked_sub(1) else {
            return;
        };
        let Some(to) = (if down {
            from.checked_add(1)
        } else {
            from.checked_sub(1)
        }) else {
            return;
        };
        let (Some(here), Some(there)) = (rows.get(from), rows.get(to)) else {
            return;
        };
        let column = self.cursor.saturating_sub(here.start);
        let end = there.start + there.text.chars().count();
        self.cursor = (there.start + column).min(end);
    }

    /// The rows this composer draws, wrapped to the card's text budget. The
    /// buffer wraps verbatim so every caret position maps back to a character.
    pub fn display(&self, row_width: u16) -> Vec<ComposerLine> {
        let budget = card_budget(row_width);
        let mut lines = vec![ComposerLine::Header];
        for row in wrap_with_cursor(&self.buffer, self.cursor, budget) {
            lines.push(row);
        }
        lines.push(ComposerLine::Footer);
        lines
    }

    /// Index into [`Composer::display`] of the row the caret sits on.
    pub fn caret_line(&self, row_width: u16) -> usize {
        self.display(row_width)
            .iter()
            .position(|line| {
                matches!(
                    line,
                    ComposerLine::Body {
                        cursor: Some(_),
                        ..
                    }
                )
            })
            .unwrap_or(0)
    }

    pub fn comment_id(&self) -> Option<&str> {
        match &self.kind {
            ComposerKind::Reply { comment_id } | ComposerKind::Edit { comment_id } => {
                Some(comment_id)
            }
            ComposerKind::New { .. } => None,
        }
    }

    /// The file the composer writes about, so a pane showing another file can
    /// leave it out of its rows.
    pub fn anchor(&self) -> Option<&Anchor> {
        match &self.kind {
            ComposerKind::New { anchor } => Some(anchor),
            _ => None,
        }
    }
}

impl App {
    pub(crate) fn open_composer(&mut self, kind: ComposerKind, buffer: String) {
        self.message = None;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        // a kept draft is newer than any seed text, an edit's original body
        // included
        let parked = diff
            .parked_drafts
            .iter()
            .position(|draft| draft.kind == kind);
        diff.composer = Some(match parked {
            Some(index) => diff.parked_drafts.remove(index),
            None => Composer::new(kind, buffer),
        });
        diff.visual_anchor = None;
        diff.mark_reflow();
        diff.ensure_rows(&self.review);
    }

    /// A left click outside the open composer parks its text, closes it, and
    /// then acts as an ordinary click.
    pub(super) fn composer_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let on_composer = diff
            .row_at_point(mouse.column, mouse.row)
            .is_some_and(|row| matches!(diff.rows().get(row), Some(DiffRow::Composer { .. })));
        if on_composer {
            return;
        }
        if let Some(draft) = diff.composer.take() {
            if !draft.buffer.trim().is_empty() {
                diff.parked_drafts.retain(|kept| kept.kind != draft.kind);
                diff.parked_drafts.push(draft);
                self.info("reopen the comment to continue your draft");
            }
            if let Some(diff) = self.diff.as_mut() {
                diff.mark_reflow();
                diff.ensure_rows(&self.review);
            }
        }
        self.handle_mouse(mouse);
    }

    pub(crate) fn composer_open(&self) -> bool {
        self.diff.as_ref().is_some_and(|d| d.composer.is_some())
    }

    /// A composer or a dialog holds what the human is typing.
    pub(crate) fn busy_typing(&self) -> bool {
        self.composer_open() || self.modal.is_some()
    }

    /// `ctrl+g`: hand the composer's buffer to `$EDITOR` on a scratch file,
    /// left in place until the terminal is back.
    fn edit_composer_externally(&mut self) {
        let Some(buffer) = self
            .diff
            .as_ref()
            .and_then(|d| d.composer.as_ref())
            .map(|c| c.buffer.clone())
        else {
            return;
        };
        self.queue_scratch_editor(&buffer, |path| EditorPurpose::TextBox {
            path,
            target: TextBoxTarget::Composer,
        });
    }

    pub(super) fn handle_composer_key(&mut self, key: &KeyEvent) -> Flow {
        if self.matches_action(key, Action::EditExternally) {
            self.edit_composer_externally();
            return Flow::Continue;
        }
        let Some(diff) = self.diff.as_mut() else {
            return Flow::Continue;
        };
        let width = diff.wrap_width;
        let Some(composer) = diff.composer.as_mut() else {
            return Flow::Continue;
        };
        let before = shape(composer, width);
        match composer.apply(key, width) {
            text_edit::Edit::Consumed => {
                // the rows only move when the card's height or the caret's row
                // does; typing within a row leaves every other row where it was
                if diff.composer.as_ref().map(|c| shape(c, width)) != Some(before) {
                    diff.mark_reflow();
                    diff.ensure_rows(&self.review);
                }
            }
            text_edit::Edit::Submit => self.submit_composer(),
            text_edit::Edit::Cancel => self.close_composer(),
        }
        Flow::Continue
    }

    /// Drop the draft and rebuild at once, since stale composer rows would
    /// misroute the next key.
    fn close_composer(&mut self) {
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            diff.composer = None;
            diff.mark_reflow();
            diff.ensure_rows(review);
        }
    }

    /// Persist the draft. An empty buffer acts as a cancel.
    fn submit_composer(&mut self) {
        let Some(composer) = self.diff.as_ref().and_then(|d| d.composer.clone()) else {
            return;
        };
        self.close_composer();
        let body = composer.buffer.trim().to_owned();
        if body.is_empty() {
            return;
        }
        let source = self.active_review_source();
        match composer.kind {
            ComposerKind::New { anchor } => {
                self.review
                    .session_for_mut(&source)
                    .add_comment(anchor, &self.author, &body);
                self.after_session_change();
            }
            ComposerKind::Reply { comment_id } => {
                if self
                    .review
                    .session_for_mut(&source)
                    .reply(&comment_id, &self.author, &body)
                {
                    self.after_session_change();
                } else {
                    self.error("comment is gone; reply dropped");
                }
            }
            ComposerKind::Edit { comment_id } => {
                if self
                    .review
                    .session_for_mut(&source)
                    .edit_comment(&comment_id, &body)
                {
                    self.queue_pr_comment_edit(&source, &comment_id, &body);
                    self.after_session_change();
                } else {
                    self.error("comment is gone; edit dropped");
                }
            }
        }
    }
}

/// How many rows the card draws and which one holds the caret. The pane
/// rebuilds only when this pair changes.
fn shape(composer: &Composer, width: u16) -> (usize, usize) {
    let lines = composer.display(width);
    let caret = lines
        .iter()
        .position(|line| {
            matches!(
                line,
                ComposerLine::Body {
                    cursor: Some(_),
                    ..
                }
            )
        })
        .unwrap_or(0);
    (lines.len(), caret)
}

/// Text cells a comment card has after its `"  ▌ "` bar, matching
/// [`crate::app::diff::comment_display`] so a draft and its result wrap alike.
pub fn card_budget(row_width: u16) -> usize {
    (row_width.saturating_sub(4) as usize).max(8)
}

/// [`wrap_rows`] with the cursor placed on its row and column.
fn wrap_with_cursor(buffer: &str, cursor: usize, budget: usize) -> Vec<ComposerLine> {
    let wrapped = wrap_rows(buffer, budget);
    let mut placed = false;
    let mut rows: Vec<ComposerLine> = wrapped
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let end = row.start + row.text.chars().count();
            // a cursor at the end of a full row belongs to the next one, where
            // the next typed character appears
            let last = index + 1 == wrapped.len();
            let holds = cursor >= row.start && (cursor < end || (last && cursor == end));
            placed |= holds;
            ComposerLine::Body {
                text: row.text.clone(),
                cursor: holds.then(|| cursor - row.start),
            }
        })
        .collect();
    // an out-of-range cursor would leave the caret invisible; park it at the end
    if !placed && let Some(ComposerLine::Body { text, cursor: at }) = rows.last_mut() {
        *at = Some(text.chars().count());
    }
    rows
}

struct WrappedRow {
    text: String,
    start: usize,
}

/// Break `buffer` into drawn rows of at most `budget` columns, on its newlines
/// first and then on width.
fn wrap_rows(buffer: &str, budget: usize) -> Vec<WrappedRow> {
    let mut rows = Vec::new();
    let mut index = 0usize;
    for (paragraph_no, paragraph) in buffer.split('\n').enumerate() {
        if paragraph_no > 0 {
            index += 1;
        }
        let mut text = String::new();
        let mut width = 0usize;
        let mut start = index;
        for character in paragraph.chars() {
            let cell = character.width().unwrap_or(0);
            if width + cell > budget && !text.is_empty() {
                // we break after the row's last space, the way a finished
                // card wraps, and keep every character so the caret maps back
                let kept = text.rfind(' ').map_or(text.len(), |at| at + 1);
                let carry = text.split_off(kept);
                let row_start = start;
                start += text.chars().count();
                rows.push(WrappedRow {
                    text: std::mem::replace(&mut text, carry),
                    start: row_start,
                });
                width = text.chars().map(|c| c.width().unwrap_or(0)).sum();
            }
            text.push(character);
            width += cell;
            index += 1;
        }
        rows.push(WrappedRow { text, start });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_wraps_between_words_and_keeps_every_character() {
        let rows = wrap_rows("one two three four", 9);
        let texts: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(texts, ["one two ", "three ", "four"]);
        let starts: Vec<usize> = rows.iter().map(|row| row.start).collect();
        assert_eq!(starts, [0, 8, 14], "each row starts where the last ended");
        let long = wrap_rows("abcdefghijkl", 5);
        assert_eq!(long.len(), 3, "a word longer than the row still breaks");
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn anchor() -> Anchor {
        Anchor {
            file: "src/lib.rs".to_owned(),
            line: Some(2),
            line_end: None,
            on_old_side: false,
            line_text: None,
        }
    }

    fn body(lines: &[ComposerLine]) -> Vec<String> {
        lines
            .iter()
            .filter_map(|line| match line {
                ComposerLine::Body { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn caret(lines: &[ComposerLine]) -> (usize, usize) {
        lines
            .iter()
            .filter(|line| matches!(line, ComposerLine::Body { .. }))
            .enumerate()
            .find_map(|(row, line)| match line {
                ComposerLine::Body {
                    cursor: Some(column),
                    ..
                } => Some((row, *column)),
                _ => None,
            })
            .expect("the caret is always on some row")
    }

    #[test]
    fn an_empty_composer_still_draws_a_card_with_its_caret() {
        let composer = Composer::new(ComposerKind::New { anchor: anchor() }, String::new());
        let lines = composer.display(40);
        assert_eq!(lines.first(), Some(&ComposerLine::Header));
        assert_eq!(lines.last(), Some(&ComposerLine::Footer));
        assert_eq!(body(&lines), vec![String::new()]);
        assert_eq!(caret(&lines), (0, 0));
    }

    #[test]
    fn the_card_grows_a_row_as_the_text_passes_the_wrap_budget() {
        let width = 20;
        let budget = card_budget(width);
        let short = Composer::new(ComposerKind::New { anchor: anchor() }, "a".repeat(budget));
        let long = Composer::new(
            ComposerKind::New { anchor: anchor() },
            "a".repeat(budget + 1),
        );
        assert_eq!(short.display(width).len(), 3, "header, one row, footer");
        assert_eq!(long.display(width).len(), 4);
    }

    #[test]
    fn a_newline_starts_a_row_even_when_the_one_above_has_room() {
        let composer = Composer::new(ComposerKind::New { anchor: anchor() }, "a\nb".to_owned());
        assert_eq!(body(&composer.display(40)), vec!["a", "b"]);
    }

    #[test]
    fn the_caret_follows_the_cursor_onto_the_row_it_wrapped_to() {
        let budget = card_budget(20);
        let text = "a".repeat(budget) + "bc";
        let mut composer = Composer::new(ComposerKind::New { anchor: anchor() }, text);
        assert_eq!(
            caret(&composer.display(20)),
            (1, 2),
            "end of the second row"
        );
        composer.cursor = 0;
        assert_eq!(caret(&composer.display(20)), (0, 0));
        composer.cursor = budget;
        // the row above is full, so the next character appears on the next one
        assert_eq!(caret(&composer.display(20)), (1, 0));
    }

    #[test]
    fn the_arrows_walk_the_wrapped_rows_a_writer_actually_sees() {
        let width = 40;
        let budget = card_budget(width);
        // one paragraph, no newline in it, three drawn rows
        let mut composer = Composer::new(
            ComposerKind::New { anchor: anchor() },
            "z".repeat(budget * 3),
        );
        composer.cursor = 5;
        composer.apply(&press(KeyCode::Down), width);
        assert_eq!(composer.cursor, budget + 5, "one row down, same column");
        composer.apply(&press(KeyCode::Down), width);
        assert_eq!(composer.cursor, budget * 2 + 5);
        composer.apply(&press(KeyCode::Up), width);
        assert_eq!(composer.cursor, budget + 5);
    }

    #[test]
    fn the_arrows_hold_still_at_the_first_and_last_drawn_row() {
        let width = 40;
        let mut composer = Composer::new(ComposerKind::New { anchor: anchor() }, "one".to_owned());
        composer.cursor = 1;
        composer.apply(&press(KeyCode::Up), width);
        assert_eq!(composer.cursor, 1);
        composer.apply(&press(KeyCode::Down), width);
        assert_eq!(composer.cursor, 1);
    }

    #[test]
    fn a_short_row_clamps_the_column_the_arrow_carries() {
        let width = 40;
        let mut composer = Composer::new(
            ComposerKind::New { anchor: anchor() },
            "a long first line\nab".to_owned(),
        );
        composer.cursor = 10;
        composer.apply(&press(KeyCode::Down), width);
        assert_eq!(composer.cursor, 20, "the end of the short row");
    }

    #[test]
    fn the_caret_stays_visible_when_the_cursor_runs_past_the_buffer() {
        let mut composer = Composer::new(ComposerKind::New { anchor: anchor() }, "ab".to_owned());
        composer.cursor = 99;
        assert_eq!(caret(&composer.display(40)), (0, 2));
    }

    #[test]
    fn a_wide_glyph_wraps_on_the_columns_it_occupies() {
        let budget = card_budget(20);
        let composer = Composer::new(
            ComposerKind::New { anchor: anchor() },
            "世".repeat(budget / 2 + 1),
        );
        assert_eq!(body(&composer.display(20)).len(), 2);
    }
}
