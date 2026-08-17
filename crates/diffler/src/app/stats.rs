//! The language breakdown screen: what the checkout is written in.
//!
//! Counting reads every tracked file, so it runs on the blocking pool and
//! answers over the event channel like every other worker here. The screen
//! opens immediately and says it is counting; the table replaces that when the
//! scan lands.

use diffler_core::stats::RepoStats;

use crate::app::{App, Flow, Screen};

/// How the table is ordered. Code lines first, since that is the number the
/// bar draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatsSort {
    #[default]
    Code,
    Files,
    Lines,
    Name,
}

impl StatsSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Files => "files",
            Self::Lines => "lines",
            Self::Name => "name",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Code => Self::Files,
            Self::Files => Self::Lines,
            Self::Lines => Self::Name,
            Self::Name => Self::Code,
        }
    }
}

/// The screen's own state. `stats` is `None` while the scan is out.
#[derive(Debug, Default)]
pub struct StatsView {
    pub stats: Option<RepoStats>,
    pub cursor: usize,
    pub scroll: usize,
    /// Language rows the last render fitted, so a paging key covers a page.
    pub viewport: u16,
    pub sort: StatsSort,
}

impl StatsView {
    /// The rows to draw, in the order the sort asks for.
    pub fn rows(&self) -> Vec<diffler_core::stats::LanguageCount> {
        let Some(stats) = self.stats.as_ref() else {
            return Vec::new();
        };
        let mut rows = stats.languages.clone();
        match self.sort {
            // the scan already returns code order, name-tied
            StatsSort::Code => {}
            StatsSort::Files => rows.sort_by(|a, b| b.files.cmp(&a.files).then(a.name.cmp(b.name))),
            StatsSort::Lines => rows.sort_by(|a, b| b.lines.cmp(&a.lines).then(a.name.cmp(b.name))),
            StatsSort::Name => rows.sort_by(|a, b| a.name.cmp(b.name)),
        }
        rows
    }
}

/// A queued repo scan; the token drops an answer for a scan the screen has
/// already replaced.
#[derive(Debug, Clone)]
pub struct StatsRequest {
    pub token: u64,
}

impl App {
    /// `L`: open the breakdown and start counting.
    pub(crate) fn open_stats(&mut self) {
        self.stats = Some(StatsView::default());
        self.queue_stats();
        self.push_screen(Screen::Stats);
    }

    /// `<c-r>` on the breakdown: count again, and empty the table while the
    /// answer is out so the numbers on screen are never from a stale scan.
    pub(crate) fn rescan_stats(&mut self) {
        if let Some(view) = self.stats.as_mut() {
            view.stats = None;
        }
        self.queue_stats();
    }

    pub(crate) fn queue_stats(&mut self) {
        self.stats_token = self.stats_token.wrapping_add(1);
        self.pending_stats = Some(StatsRequest {
            token: self.stats_token,
        });
    }

    pub(crate) fn on_repo_stats(&mut self, stats: RepoStats, token: u64) -> Flow {
        if token != self.stats_token {
            return Flow::Idle;
        }
        let Some(view) = self.stats.as_mut() else {
            return Flow::Idle;
        };
        view.cursor = view.cursor.min(stats.languages.len().saturating_sub(1));
        view.stats = Some(stats);
        Flow::Continue
    }

    pub(super) fn dispatch_stats(&mut self, action: crate::keymap::Action) {
        use crate::keymap::Action;
        let len = self.stats.as_ref().map_or(0, |view| view.rows().len());
        let last = len.saturating_sub(1);
        let Some(view) = self.stats.as_mut() else {
            return;
        };
        let page = crate::app::page_step(view.viewport, false);
        match action {
            Action::MoveDown => view.cursor = (view.cursor + 1).min(last),
            Action::MoveUp => view.cursor = view.cursor.saturating_sub(1),
            Action::GoTop => view.cursor = 0,
            Action::GoBottom => view.cursor = last,
            Action::HalfPageDown => view.cursor = (view.cursor + page).min(last),
            Action::HalfPageUp => view.cursor = view.cursor.saturating_sub(page),
            Action::CycleSort => {
                view.sort = view.sort.next();
                view.cursor = 0;
                let label = view.sort.label();
                self.info(format!("sorted by {label}"));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::stats::{LanguageCount, RepoStats};

    use super::StatsSort;
    use crate::app::{App, Screen};
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, key, standard_fixture};

    fn app() -> (Fixture, App) {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        (fixture, app)
    }

    fn counted(names: &[(&'static str, usize)]) -> RepoStats {
        RepoStats {
            languages: names
                .iter()
                .map(|(name, code)| LanguageCount {
                    name,
                    color: (0x80, 0x80, 0x80),
                    files: 1,
                    lines: *code,
                    code: *code,
                    comments: 0,
                    blanks: 0,
                    bytes: 0,
                })
                .collect(),
            unknown_files: 0,
            skipped_files: 0,
            generated_files: 0,
        }
    }

    fn deliver(app: &mut App, stats: RepoStats) {
        let token = app.pending_stats.take().expect("a queued scan").token;
        app.handle(AppEvent::RepoStats {
            stats: Box::new(stats),
            token,
        });
    }

    #[test]
    fn l_opens_the_breakdown_and_queues_the_scan() {
        let (_fixture, mut app) = app();
        app.handle(key('L'));
        assert_eq!(app.screen(), Screen::Stats);
        assert!(app.pending_stats.is_some(), "the scan is queued, not run");
        assert!(
            app.stats.as_ref().is_some_and(|view| view.stats.is_none()),
            "the screen opens before the answer"
        );

        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::Status);
        assert!(app.stats.is_none(), "leaving drops the table");
    }

    #[test]
    fn a_scan_from_a_closed_screen_is_dropped() {
        let (_fixture, mut app) = app();
        app.handle(key('L'));
        let stale = app.pending_stats.take().expect("a queued scan").token;
        app.handle(key('q'));
        app.handle(key('L'));

        app.handle(AppEvent::RepoStats {
            stats: Box::new(counted(&[("Rust", 10)])),
            token: stale,
        });
        assert!(
            app.stats.as_ref().is_some_and(|view| view.stats.is_none()),
            "the answer to the first scan does not fill the second screen"
        );
    }

    #[test]
    fn s_cycles_the_sort_and_reorders_the_table() {
        let (_fixture, mut app) = app();
        app.handle(key('L'));
        deliver(&mut app, counted(&[("Rust", 100), ("Shell", 10)]));

        let names = |app: &App| -> Vec<&'static str> {
            app.stats
                .as_ref()
                .expect("view")
                .rows()
                .iter()
                .map(|row| row.name)
                .collect()
        };
        assert_eq!(names(&app), vec!["Rust", "Shell"], "code order to start");

        app.handle(key('s'));
        app.handle(key('s'));
        app.handle(key('s'));
        assert_eq!(
            app.stats.as_ref().expect("view").sort,
            StatsSort::Name,
            "code → files → lines → name"
        );
        assert_eq!(names(&app), vec!["Rust", "Shell"]);
        app.handle(key('s'));
        assert_eq!(app.stats.as_ref().expect("view").sort, StatsSort::Code);
    }

    #[test]
    fn the_cursor_stays_inside_the_table() {
        let (_fixture, mut app) = app();
        app.handle(key('L'));
        deliver(&mut app, counted(&[("Rust", 100), ("Shell", 10)]));
        let cursor = |app: &App| app.stats.as_ref().expect("view").cursor;

        app.handle(key('G'));
        assert_eq!(cursor(&app), 1);
        app.handle(key('j'));
        assert_eq!(cursor(&app), 1, "the last row is the last row");
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(cursor(&app), 0);
        app.handle(key('k'));
        assert_eq!(cursor(&app), 0);
    }

    #[test]
    fn refresh_rescans_and_empties_the_table_while_it_waits() {
        let (_fixture, mut app) = app();
        app.handle(key('L'));
        deliver(&mut app, counted(&[("Rust", 100)]));
        assert!(app.stats.as_ref().is_some_and(|view| view.stats.is_some()));

        app.handle(crate::test_support::ctrl_key('r'));
        assert!(
            app.stats.as_ref().is_some_and(|view| view.stats.is_none()),
            "the stale count goes away with the request"
        );
        assert!(app.pending_stats.is_some());
    }
}
