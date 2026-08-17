//! The `/` search controller shared by the screens.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Flow, Pane, Screen, diff_row_text, tree_row_label};
use crate::graph::GraphView;
use crate::search::Search;

impl App {
    pub(super) fn search_start(&mut self) {
        let origin = self.focused_cursor_row();
        let rows = self.focused_search_rows();
        let mut search = Search::start(origin);
        search.recompute(&rows);
        self.search = Some(search);
    }

    pub(super) fn handle_search_key(&mut self, key: &KeyEvent) -> Flow {
        match key.code {
            KeyCode::Esc => self.search_cancel(),
            KeyCode::Enter => self.search_commit(),
            KeyCode::Backspace => self.search_edit(Search::backspace),
            KeyCode::Char(c) => self.search_edit(|s| s.insert(c)),
            _ => {}
        }
        Flow::Continue
    }

    pub(super) fn search_edit(&mut self, edit: impl FnOnce(&mut Search)) {
        if let Some(s) = self.search.as_mut() {
            edit(s);
        }
        let rows = self.focused_search_rows();
        if let Some(s) = self.search.as_mut() {
            s.recompute(&rows);
        }
        if let Some(row) = self.search.as_ref().and_then(Search::current_row) {
            self.focus_searched_row(row);
        }
    }

    /// `n`/`N`: step the committed search, or follow an edge on the graph,
    /// where the same keys walk edges when no search is up.
    pub(super) fn search_step_or_follow(&mut self, forward: bool) {
        if self.search.is_none()
            && self.screen() == Screen::Graph
            && let Some(graph) = self.graph.as_mut()
        {
            graph.follow_edge(forward);
            return;
        }
        self.search_step(forward);
    }

    pub(super) fn search_step(&mut self, forward: bool) {
        let row = match self.search.as_mut() {
            Some(s) if !s.open => {
                if forward {
                    s.next_match()
                } else {
                    s.prev_match()
                }
            }
            _ => return,
        };
        if let Some(row) = row {
            self.focus_searched_row(row);
        }
    }

    pub(super) fn search_commit(&mut self) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.query().is_empty() {
            self.search = None;
            return;
        }
        if let Some(row) = search.commit() {
            self.focus_searched_row(row);
        }
    }

    pub(super) fn search_cancel(&mut self) {
        if let Some(origin) = self.search.take().map(|s| s.origin_row()) {
            self.focus_searched_row(origin);
        }
    }

    pub(super) fn focused_cursor_row(&self) -> usize {
        match self.screen() {
            Screen::Status => self.status.cursor,
            Screen::Log => self.log.as_ref().map_or(0, |l| l.cursor),
            Screen::Diff => self.diff.as_ref().map_or(0, |d| match d.focus {
                Pane::List => d.tree_cursor,
                Pane::Comments => d.comments_cursor(),
                Pane::Diff => d.cursor,
            }),
            Screen::CiLog => self.ci_log.as_ref().map_or(0, |v| v.cursor),
            Screen::File => self.file.as_ref().map_or(0, |v| v.cursor),
            Screen::Runs => self.runs_cursor,
            Screen::Prs => self.prs_cursor,
            Screen::Graph => self.graph.as_ref().map_or(0, GraphView::selected_index),
            Screen::Stats => self.stats.as_ref().map_or(0, |view| view.cursor),
        }
    }

    pub(super) fn focused_search_rows(&self) -> Vec<(usize, String)> {
        match self.screen() {
            Screen::Status => self.status_search_rows(),
            Screen::Log => self.log.as_ref().map_or_else(Vec::new, |log| {
                log.entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (i, e.subject.clone()))
                    .collect()
            }),
            Screen::Diff => self.diff_search_rows(),
            Screen::Runs => self
                .runs
                .iter()
                .enumerate()
                .map(|(i, run)| (i, format!("{} {}", run.name, run.title)))
                .collect(),
            Screen::Prs => self
                .prs
                .iter()
                .enumerate()
                .map(|(i, pr)| (i, format!("#{} {} {}", pr.number, pr.title, pr.author)))
                .collect(),
            Screen::CiLog => self.ci_log.as_ref().map_or_else(Vec::new, |view| {
                view.rows()
                    .iter()
                    .enumerate()
                    .map(|(i, row)| (i, view.row_text(*row).to_owned()))
                    .collect()
            }),
            Screen::File => self.file.as_ref().map_or_else(Vec::new, |view| {
                view.lines.iter().cloned().enumerate().collect()
            }),
            Screen::Graph => self
                .graph
                .as_ref()
                .map_or_else(Vec::new, GraphView::search_rows),
            // the breakdown is a dozen rows the reader can already see
            Screen::Stats => Vec::new(),
        }
    }

    pub(super) fn diff_search_rows(&self) -> Vec<(usize, String)> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let model = diff.model(&self.review);
        match diff.focus {
            Pane::List => diff
                .tree_rows(model, self.review.session_for(&diff.source))
                .iter()
                .enumerate()
                .map(|(i, r)| (i, tree_row_label(&r.node)))
                .collect(),
            // the comments sidebar lists comments, so that is what it searches
            Pane::Comments => {
                let session = self.review.session_for(&diff.source);
                self.comment_order()
                    .iter()
                    .enumerate()
                    .filter_map(|(index, id)| {
                        let comment = session.comments.iter().find(|c| &c.id == id)?;
                        let line = comment
                            .anchor
                            .line
                            .map_or(String::new(), |line| format!(":{line}"));
                        Some((
                            index,
                            format!("{}{line} {}", comment.anchor.file, comment.body),
                        ))
                    })
                    .collect()
            }
            Pane::Diff => {
                let file = model.files.get(diff.selected);
                diff.rows()
                    .iter()
                    .enumerate()
                    .filter_map(|(i, row)| diff_row_text(file, row).map(|t| (i, t)))
                    .collect()
            }
        }
    }

    pub(super) fn focus_searched_row(&mut self, row: usize) {
        match self.screen() {
            Screen::Status => self.status.cursor = row,
            Screen::Runs => self.runs_cursor = row,
            Screen::Prs => self.prs_cursor = row,
            Screen::Log => {
                if let Some(l) = self.log.as_mut() {
                    l.cursor = row;
                }
            }
            // the sidebars select as they move, so a search jump lands through
            // the same call a motion key makes
            Screen::Diff => match self.diff.as_ref().map(|d| d.focus) {
                Some(Pane::List) => self.diff_tree_to(row),
                Some(Pane::Comments) => self.comments_to(row),
                Some(Pane::Diff) => {
                    if let Some(d) = self.diff.as_mut() {
                        d.cursor = row;
                    }
                }
                None => {}
            },
            Screen::CiLog => {
                if let Some(v) = self.ci_log.as_mut() {
                    v.cursor = row;
                }
            }
            Screen::File => {
                if let Some(v) = self.file.as_mut() {
                    v.cursor = row;
                }
            }
            Screen::Graph => {
                if let Some(g) = self.graph.as_mut() {
                    g.select_nth(row);
                }
            }
            Screen::Stats => {
                if let Some(v) = self.stats.as_mut() {
                    v.cursor = row;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, esc_key, key, standard_fixture};

    fn app() -> (Fixture, App) {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        (fixture, app)
    }

    // standard_fixture's status rows (flat layout): Untracked header,
    // todo.md, Unstaged header, src/lib.rs, Staged header, ci.yml, Recent
    // commits header: src/lib.rs sits at index 3, ci.yml at index 5.

    #[test]
    fn slash_search_filters_status_rows_and_moves_the_cursor_live() {
        let (_fixture, mut app) = app();
        app.handle(key('/'));
        assert!(app.search.as_ref().expect("search open").open);
        for c in "lib".chars() {
            app.handle(key(c));
        }
        assert_eq!(app.search.as_ref().unwrap().query(), "lib");
        assert_eq!(
            app.status.cursor, 3,
            "typing narrows the match onto src/lib.rs live"
        );
    }

    #[test]
    fn enter_commits_the_status_search_and_seats_the_cursor_on_the_match() {
        let (_fixture, mut app) = app();
        app.handle(key('/'));
        for c in "ci.yml".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        assert!(
            !app.search.as_ref().unwrap().open,
            "the prompt closes on commit"
        );
        assert_eq!(app.status.cursor, 5, "cursor lands on the matched row");
    }

    #[test]
    fn escape_cancels_the_status_search_and_restores_the_origin_cursor() {
        let (_fixture, mut app) = app();
        app.status.cursor = 2; // the Unstaged section header, the search origin
        app.handle(key('/'));
        for c in "ci.yml".chars() {
            app.handle(key(c));
        }
        assert_eq!(app.status.cursor, 5, "typing moved off the origin");
        app.handle(esc_key());
        assert!(app.search.is_none());
        assert_eq!(app.status.cursor, 2, "escape restores the origin row");
    }

    #[test]
    fn slash_search_filters_diff_list_rows_and_moves_the_tree_cursor() {
        let (_fixture, mut app) = app();
        app.open_working_tree_diff(None);
        assert_eq!(app.screen(), Screen::Diff);
        let target = app
            .diff_search_rows()
            .into_iter()
            .find(|(_, text)| text.contains("lib.rs"))
            .map(|(row, _)| row)
            .expect("src/lib.rs is in the diff file list");

        app.handle(key('/'));
        for c in "lib".chars() {
            app.handle(key(c));
        }
        assert_eq!(app.diff.as_ref().unwrap().tree_cursor, target);
        assert_eq!(
            app.diff.as_ref().and_then(|d| d.selected_path(&app.review)),
            Some("src/lib.rs".to_owned()),
            "landing on a file row opens it, as moving onto it would"
        );
    }

    /// The comments sidebar lists comments, so it searches comments and its
    /// jump seats the diff cursor on the one it finds.
    #[test]
    fn search_in_the_comments_sidebar_walks_comments_and_selects_them() {
        let (_fixture, mut app) = app();
        app.open_working_tree_diff(None);
        for (line, body) in [(1, "first note"), (2, "the widget leaks")] {
            app.review.session.add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(line),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                body,
            );
        }
        app.handle(key('C'));
        assert_eq!(app.diff.as_ref().unwrap().focus, Pane::Comments);

        app.handle(key('/'));
        for c in "widget".chars() {
            app.handle(key(c));
        }

        let diff = app.diff.as_ref().expect("diff");
        assert_eq!(diff.comments_cursor(), 1, "the matching comment is current");
        assert!(
            matches!(
                diff.rows().get(diff.cursor),
                Some(crate::app::DiffRow::Comment { line: 0, .. })
            ),
            "and the diff cursor sits on it, so the pane verbs reach it"
        );
    }

    #[test]
    fn enter_commits_the_diff_search_and_seats_the_cursor_on_the_match() {
        let (_fixture, mut app) = app();
        app.open_working_tree_diff(None);
        let target = app
            .diff_search_rows()
            .into_iter()
            .find(|(_, text)| text.contains("lib.rs"))
            .map(|(row, _)| row)
            .expect("src/lib.rs is in the diff file list");

        app.handle(key('/'));
        for c in "lib".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        assert!(!app.search.as_ref().unwrap().open);
        assert_eq!(app.diff.as_ref().unwrap().tree_cursor, target);
    }

    #[test]
    fn escape_cancels_the_diff_search_and_restores_the_origin_cursor() {
        let (_fixture, mut app) = app();
        app.open_working_tree_diff(None);
        let origin = app.diff.as_ref().unwrap().tree_cursor;

        app.handle(key('/'));
        for c in "lib".chars() {
            app.handle(key(c));
        }
        app.handle(esc_key());
        assert!(app.search.is_none());
        assert_eq!(
            app.diff.as_ref().unwrap().tree_cursor,
            origin,
            "escape restores the origin row"
        );
    }
}
