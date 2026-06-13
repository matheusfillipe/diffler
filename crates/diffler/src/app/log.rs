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
    /// Body height of the last render, drives half-page motions.
    pub(crate) viewport: u16,
}

impl App {
    pub(crate) fn open_log(&mut self) {
        match self.review.vcs.log(LOG_LIMIT) {
            Ok(entries) => {
                self.log = Some(LogView {
                    entries,
                    cursor: 0,
                    scroll: 0,
                    viewport: 0,
                });
                self.screens.push(super::Screen::Log);
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
            }
            Err(err) => {
                let text = err.to_string();
                self.error(text);
            }
        }
    }

    pub(super) fn dispatch_log(&mut self, action: Action) {
        if action == Action::Open {
            let oid = self
                .log
                .as_ref()
                .and_then(|log| log.entries.get(log.cursor))
                .map(|entry| entry.oid.clone());
            if let Some(oid) = oid {
                self.open_commit_diff(&oid);
            }
            return;
        }
        let half = self.log_half_page();
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
            other => {
                self.info(format!("{} is not implemented yet", other.name()));
            }
        }
    }

    fn log_half_page(&self) -> usize {
        let viewport = self.log.as_ref().map_or(0, |l| l.viewport);
        // before the first render the height is unknown; half a typical
        // terminal is a fine guess
        if viewport == 0 {
            20
        } else {
            usize::from(viewport / 2).max(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DiffSource, Screen};
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
        let DiffSource::Commit(oid) = &diff.source else {
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
}
