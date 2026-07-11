//! Log screen state and handlers: a flat list of commits, `<cr>` opens the
//! commit's diff in the diff view.

use diffler_core::vcs::LogEntry;

use super::App;
use crate::keymap::Action;

/// How much history one log screen loads (neogit's default max-count).
const LOG_LIMIT: usize = 256;

pub struct LogView {
    pub entries: Vec<LogEntry>,
    pub cursor: usize,
    /// First visible entry; the renderer keeps the cursor in view.
    pub scroll: usize,
    /// Row where `V` started; `Some` means range selection is active.
    pub visual_anchor: Option<usize>,
    /// Body height of the last render, drives half-page motions.
    pub(crate) viewport: u16,
    /// Entry-area rect of the last render, for mapping mouse clicks to rows.
    pub(crate) body: ratatui::layout::Rect,
}

impl LogView {
    /// Inclusive row span the visual selection covers, when active.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.visual_anchor?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }
}

impl App {
    pub(crate) fn open_log(&mut self) {
        match self.review.vcs.log(LOG_LIMIT) {
            Ok(entries) => {
                self.log = Some(LogView {
                    entries,
                    cursor: 0,
                    scroll: 0,
                    visual_anchor: None,
                    viewport: 0,
                    body: ratatui::layout::Rect::default(),
                });
                self.push_screen(super::Screen::Log);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    /// Reload entries after a repo change, keeping the cursor in bounds.
    pub(super) fn refresh_log(&mut self) {
        let Some(log) = self.log.as_mut() else {
            return;
        };
        match self.review.vcs.log(LOG_LIMIT) {
            Ok(entries) => {
                log.entries = entries;
                log.cursor = log.cursor.min(log.entries.len().saturating_sub(1));
                // anchors are row indices; new history would dangle them
                log.visual_anchor = None;
            }
            Err(err) => {
                let text = err.to_string();
                self.error(text);
            }
        }
    }

    pub(super) fn dispatch_log(&mut self, action: Action) {
        if action == Action::Open {
            self.open_log_selection();
            return;
        }
        let half = self.log_page(false);
        let full = self.log_page(true);
        let Some(log) = self.log.as_mut() else {
            return;
        };
        let last = log.entries.len().saturating_sub(1);
        match action {
            Action::MoveDown => log.cursor = (log.cursor + 1).min(last),
            Action::MoveUp => log.cursor = log.cursor.saturating_sub(1),
            Action::GoTop => log.cursor = 0,
            Action::GoBottom => log.cursor = last,
            Action::HalfPageDown => log.cursor = (log.cursor + half).min(last),
            Action::HalfPageUp => log.cursor = log.cursor.saturating_sub(half),
            Action::FullPageDown => log.cursor = (log.cursor + full).min(last),
            Action::FullPageUp => log.cursor = log.cursor.saturating_sub(full),
            // V toggles a range selection anchored at the cursor commit
            Action::VisualSelect => {
                if log.visual_anchor.take().is_none() {
                    log.visual_anchor = Some(log.cursor);
                }
            }
            other => {
                self.info(format!("{} is not implemented yet", other.name()));
            }
        }
    }

    /// `<cr>` in the log: with a selection, open the combined diff of the
    /// selected commit range (rows are newest-first, so the span's high index
    /// is the oldest commit); otherwise open the single commit under the
    /// cursor.
    fn open_log_selection(&mut self) {
        let Some(log) = self.log.as_ref() else {
            return;
        };
        if let Some((top, bottom)) = log.selection() {
            let (Some(newest), Some(oldest)) = (log.entries.get(top), log.entries.get(bottom))
            else {
                return;
            };
            let (oldest, newest) = (oldest.oid.clone(), newest.oid.clone());
            self.open_range_diff(&oldest, &newest);
        } else {
            let Some(oid) = log.entries.get(log.cursor).map(|entry| entry.oid.clone()) else {
                return;
            };
            self.open_commit_diff(&oid);
        }
    }

    pub(super) fn log_mouse(&mut self, gesture: super::MouseGesture) {
        use super::MouseGesture;
        match gesture {
            MouseGesture::Scroll { down, .. } => {
                let delta = if down { 3 } else { -3 };
                if let Some(log) = self.log.as_mut() {
                    let last = log.entries.len().saturating_sub(1);
                    log.cursor = log.cursor.saturating_add_signed(delta).min(last);
                }
            }
            MouseGesture::Press { col, row } => {
                if let Some(index) = self.log_row_at(col, row)
                    && let Some(log) = self.log.as_mut()
                {
                    log.cursor = index;
                    log.visual_anchor = None;
                }
            }
            // double-click opens the commit's diff, like `<cr>`
            MouseGesture::DoublePress { col, row } => {
                if let Some(index) = self.log_row_at(col, row) {
                    if let Some(log) = self.log.as_mut() {
                        log.cursor = index;
                    }
                    self.open_log_selection();
                }
            }
            // drag grows the range selection from the press row
            MouseGesture::Drag { col, row } => {
                if let Some(index) = self.log_row_at(col, row)
                    && let Some(log) = self.log.as_mut()
                {
                    if log.visual_anchor.is_none() {
                        log.visual_anchor = Some(log.cursor);
                    }
                    log.cursor = index;
                }
            }
            MouseGesture::Cancel => {
                if let Some(log) = self.log.as_mut() {
                    log.visual_anchor = None;
                }
            }
        }
    }

    fn log_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let log = self.log.as_ref()?;
        let index = super::hit_index(log.body, log.scroll, col, row)?;
        (index < log.entries.len()).then_some(index)
    }

    fn log_page(&self, full: bool) -> usize {
        super::page_step(self.log.as_ref().map_or(0, |l| l.viewport), full)
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::source::ReviewSource;

    use super::super::Screen;
    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, ctrl_key, key, standard_fixture};

    /// Three commits and a second branch ref on HEAD, so log rows carry
    /// subjects and decorations.
    fn log_fixture() -> Fixture {
        let fixture = standard_fixture();
        fixture.write("notes.txt", "alpha\nbeta\n");
        fixture.commit_all("add beta note");
        fixture.write(
            "src/util.rs",
            "pub fn twice(x: u32) -> u32 {\n    x * 2\n}\n",
        );
        fixture.commit_all("add util module");
        fixture.branch("feat/topic");
        fixture
    }

    fn open_log(fixture: &Fixture) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        app
    }

    #[test]
    fn ll_opens_the_log_with_entries_newest_first() {
        let fixture = log_fixture();
        let app = open_log(&fixture);
        assert_eq!(app.screen(), Screen::Log);
        let log = app.log.as_ref().expect("log view");
        assert_eq!(log.entries.len(), 3);
        assert_eq!(log.entries[0].subject, "add util module");
        assert!(
            log.entries[0].refs.iter().any(|r| r == "feat/topic"),
            "HEAD decorations include the branch ref: {:?}",
            log.entries[0].refs
        );
        assert_eq!(log.cursor, 0);
    }

    #[test]
    fn cursor_moves_and_clamps_in_the_log() {
        let fixture = log_fixture();
        let mut app = open_log(&fixture);
        app.handle(key('k'));
        assert_eq!(app.log.as_ref().unwrap().cursor, 0);
        for _ in 0..10 {
            app.handle(key('j'));
        }
        assert_eq!(app.log.as_ref().unwrap().cursor, 2);
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(app.log.as_ref().unwrap().cursor, 0);
        app.handle(key('G'));
        assert_eq!(app.log.as_ref().unwrap().cursor, 2);
    }

    #[test]
    fn half_page_motions_move_the_log_cursor_by_the_viewport() {
        let fixture = standard_fixture();
        for index in 0..12 {
            fixture.write("notes.txt", &format!("rev {index}\n"));
            fixture.commit_all(&format!("commit number {index}"));
        }
        let mut app = open_log(&fixture);
        app.log.as_mut().unwrap().viewport = 10;
        app.handle(ctrl_key('d'));
        assert_eq!(app.log.as_ref().unwrap().cursor, 5);
        app.handle(ctrl_key('d'));
        app.handle(ctrl_key('d'));
        assert_eq!(app.log.as_ref().unwrap().cursor, 12, "clamps at the end");
        app.handle(ctrl_key('u'));
        assert_eq!(app.log.as_ref().unwrap().cursor, 7);
        app.handle(ctrl_key('u'));
        app.handle(ctrl_key('u'));
        assert_eq!(app.log.as_ref().unwrap().cursor, 0, "clamps at the top");
    }

    #[test]
    fn enter_opens_the_commit_diff_and_back_returns_to_the_log() {
        let fixture = log_fixture();
        let mut app = open_log(&fixture);
        app.handle(key('j'));
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff view");
        let ReviewSource::Commit { oid } = &diff.source else {
            panic!("expected a commit diff");
        };
        let expected = &app.log.as_ref().unwrap().entries[1].oid;
        assert_eq!(oid, expected);

        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::Log);
        assert!(app.diff.is_none());
        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::Status);
    }

    #[test]
    fn visual_select_anchors_and_extends_over_commit_rows() {
        let fixture = log_fixture();
        let mut app = open_log(&fixture);
        app.handle(key('V'));
        let log = app.log.as_ref().expect("log view");
        assert_eq!(log.visual_anchor, Some(0));
        assert_eq!(log.selection(), Some((0, 0)));
        // j extends the selection down to the older commit
        app.handle(key('j'));
        assert_eq!(app.log.as_ref().unwrap().selection(), Some((0, 1)));
        // k brings the cursor back up, collapsing the selection
        app.handle(key('k'));
        assert_eq!(app.log.as_ref().unwrap().selection(), Some((0, 0)));
    }

    #[test]
    fn enter_on_a_two_commit_selection_opens_the_combined_range_diff() {
        let fixture = log_fixture();
        let mut app = open_log(&fixture);
        // select the two newest commits (add util module, add beta note)
        app.handle(key('V'));
        app.handle(key('j'));
        let (newest, oldest) = {
            let log = app.log.as_ref().unwrap();
            (log.entries[0].oid.clone(), log.entries[1].oid.clone())
        };
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff view");
        let ReviewSource::Range {
            oldest: src_oldest,
            newest: src_newest,
        } = &diff.source
        else {
            panic!("expected a range diff, got {:?}", diff.source);
        };
        assert_eq!(src_oldest, &oldest);
        assert_eq!(src_newest, &newest);
        // the combined diff folds both commits' files into one model
        let paths: Vec<&str> = diff
            .model(&app.review)
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert!(paths.contains(&"notes.txt"), "{paths:?}");
        assert!(paths.contains(&"src/util.rs"), "{paths:?}");
    }

    #[test]
    fn enter_without_a_selection_opens_the_single_commit_diff() {
        let fixture = log_fixture();
        let mut app = open_log(&fixture);
        app.handle(key('j'));
        app.handle(key('\n'));
        let diff = app.diff.as_ref().expect("diff view");
        assert!(
            matches!(&diff.source, ReviewSource::Commit { .. }),
            "no selection means a single commit diff: {:?}",
            diff.source
        );
    }

    #[test]
    fn escape_cancels_the_log_visual_selection() {
        let fixture = log_fixture();
        let mut app = open_log(&fixture);
        app.handle(key('V'));
        assert!(app.log.as_ref().unwrap().visual_anchor.is_some());
        app.handle(crate::test_support::esc_key());
        assert!(app.log.as_ref().unwrap().visual_anchor.is_none());
        // V twice toggles off as well
        app.handle(key('V'));
        app.handle(key('V'));
        assert!(app.log.as_ref().unwrap().visual_anchor.is_none());
    }
}
