//! Visual selection over a list of rows, shared by every screen that has a
//! cursor walking rows: the diff, the log, a CI job's log, and the file view.
//! `V` anchors, a motion extends, `y` yanks, `esc` drops it.

use super::{App, Screen};

/// A screen whose cursor walks rows and can anchor a `V` selection over them.
pub trait RowSelect {
    fn cursor(&self) -> usize;
    fn anchor(&self) -> Option<usize>;
    fn set_anchor(&mut self, anchor: Option<usize>);

    /// Inclusive row span the visual selection covers, when active.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor()?;
        Some((anchor.min(self.cursor()), anchor.max(self.cursor())))
    }

    /// True when the row is the cursor's or inside the selection, which is
    /// what the renderers tint.
    fn row_selected(&self, row: usize) -> bool {
        row == self.cursor()
            || self
                .selection()
                .is_some_and(|(lo, hi)| row >= lo && row <= hi)
    }

    fn toggle_visual(&mut self) {
        let next = match self.anchor() {
            Some(_) => None,
            None => Some(self.cursor()),
        };
        self.set_anchor(next);
    }

    /// Keep an anchor inside the row range after rows disappear under it, which
    /// a fold or a refresh can do.
    fn clamp_anchor(&mut self, last: usize) {
        let clamped = self.anchor().map(|anchor| anchor.min(last));
        self.set_anchor(clamped);
    }
}

/// A [`RowSelect`] screen whose rows are plain text, so a selection yanks as
/// itself. The diff is the exception: its rows only read against the model, so
/// it builds its own yank text.
pub trait RowText: RowSelect {
    fn row_count(&self) -> usize;
    fn row_text(&self, row: usize) -> String;

    /// The selected rows as text, falling back to the cursor's row so `y`
    /// without a selection still copies the line under it.
    fn selection_text(&self) -> String {
        let last = self.row_count().saturating_sub(1);
        let (lo, hi) = self.selection().unwrap_or((self.cursor(), self.cursor()));
        (lo.min(last)..=hi.min(last))
            .map(|row| self.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl App {
    /// The row list the keyboard is on, so `esc` and the selection check reach
    /// it without every screen spelling out its own arm.
    pub(crate) fn row_select_mut(&mut self) -> Option<&mut dyn RowSelect> {
        match self.screen() {
            Screen::Diff => self.diff.as_mut().map(|view| view as &mut dyn RowSelect),
            Screen::Log => self.log.as_mut().map(|view| view as &mut dyn RowSelect),
            Screen::CiLog => self.ci_log.as_mut().map(|view| view as &mut dyn RowSelect),
            Screen::File => self.file.as_mut().map(|view| view as &mut dyn RowSelect),
            Screen::Status | Screen::Graph | Screen::Runs | Screen::Prs | Screen::Stats => None,
        }
    }

    pub(crate) fn visual_active(&self) -> bool {
        match self.screen() {
            Screen::Diff => self.diff.as_ref().is_some_and(|v| v.anchor().is_some()),
            Screen::Log => self.log.as_ref().is_some_and(|v| v.anchor().is_some()),
            Screen::CiLog => self.ci_log.as_ref().is_some_and(|v| v.anchor().is_some()),
            Screen::File => self.file.as_ref().is_some_and(|v| v.anchor().is_some()),
            Screen::Status | Screen::Graph | Screen::Runs | Screen::Prs | Screen::Stats => false,
        }
    }

    /// The rows under the keyboard as plain text. The diff is absent on
    /// purpose: its rows only read against the model, so it yanks its own way.
    fn row_text_view(&self) -> Option<&dyn RowText> {
        match self.screen() {
            Screen::Log => self.log.as_ref().map(|view| view as &dyn RowText),
            Screen::CiLog => self.ci_log.as_ref().map(|view| view as &dyn RowText),
            Screen::File => self.file.as_ref().map(|view| view as &dyn RowText),
            Screen::Diff
            | Screen::Status
            | Screen::Graph
            | Screen::Runs
            | Screen::Prs
            | Screen::Stats => None,
        }
    }

    /// Yank the row selection (or the cursor's row) and drop the anchor.
    pub(crate) fn yank_rows(&mut self, note: &str) {
        let Some(text) = self.row_text_view().map(RowText::selection_text) else {
            return;
        };
        self.pending_clipboard = Some(text);
        if let Some(view) = self.row_select_mut() {
            view.set_anchor(None);
        }
        self.info(note.to_owned());
    }
}
