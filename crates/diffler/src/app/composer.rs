//! The diff pane's in-place comment editor. It occupies the rows its result
//! will occupy, so writing a comment and reading it back look the same.

use crossterm::event::KeyEvent;
use diffler_core::session::Anchor;
use unicode_width::UnicodeWidthChar;

use super::{App, Flow, text_edit};

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

    pub fn apply(&mut self, key: &crossterm::event::KeyEvent) -> text_edit::Edit {
        text_edit::apply(&mut self.buffer, &mut self.cursor, key)
    }

    /// The rows this composer draws, wrapped to the card's text budget. The
    /// buffer wraps verbatim: a live editor has to keep every character where
    /// the writer put it, so the cursor lands where they expect.
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
    /// Open the composer over the diff pane. It takes the keyboard until it
    /// submits or cancels.
    pub(crate) fn open_composer(&mut self, kind: ComposerKind, buffer: String) {
        self.message = None;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.composer = Some(Composer::new(kind, buffer));
        diff.visual_anchor = None;
        diff.mark_reflow();
        diff.ensure_rows(&self.review);
    }

    pub(crate) fn composer_open(&self) -> bool {
        self.diff.as_ref().is_some_and(|d| d.composer.is_some())
    }

    pub(super) fn handle_composer_key(&mut self, key: &KeyEvent) -> Flow {
        let Some(diff) = self.diff.as_mut() else {
            return Flow::Continue;
        };
        let width = diff.wrap_width;
        let Some(composer) = diff.composer.as_mut() else {
            return Flow::Continue;
        };
        let before = shape(composer, width);
        match composer.apply(key) {
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

    /// Drop the draft and rebuild at once: rows holding a composer that is no
    /// longer open would misroute the next key that reads them.
    fn close_composer(&mut self) {
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            diff.composer = None;
            diff.mark_reflow();
            diff.ensure_rows(review);
        }
    }

    /// Persist the draft. An empty buffer leaves as a cancel: a comment has to
    /// say something to be worth keeping.
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

/// How many rows the card draws and which one holds the caret. The pane only
/// has to rebuild when this pair moves.
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

/// Break `buffer` into visual rows of at most `budget` columns, splitting on
/// its newlines first and then on width. The cursor rides along to the row and
/// column it lands on; sitting at the end of a full row it moves to the next,
/// which is where the next character will appear.
fn wrap_with_cursor(buffer: &str, cursor: usize, budget: usize) -> Vec<ComposerLine> {
    let mut rows = Vec::new();
    let mut placed = false;
    let mut index = 0usize;
    for (paragraph_no, paragraph) in buffer.split('\n').enumerate() {
        if paragraph_no > 0 {
            index += 1;
        }
        let mut row = String::new();
        let mut width = 0usize;
        let mut row_start = index;
        for character in paragraph.chars() {
            let cell = character.width().unwrap_or(0);
            if width + cell > budget && !row.is_empty() {
                let at = (cursor >= row_start && cursor < index).then(|| cursor - row_start);
                placed |= at.is_some();
                rows.push(ComposerLine::Body {
                    text: std::mem::take(&mut row),
                    cursor: at,
                });
                width = 0;
                row_start = index;
            }
            row.push(character);
            width += cell;
            index += 1;
        }
        let at = (cursor >= row_start && cursor <= index).then(|| cursor - row_start);
        placed |= at.is_some();
        rows.push(ComposerLine::Body {
            text: row,
            cursor: at,
        });
    }
    // an out-of-range cursor would leave the caret invisible; park it at the end
    if !placed && let Some(ComposerLine::Body { text, cursor: at }) = rows.last_mut() {
        *at = Some(text.chars().count());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

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
