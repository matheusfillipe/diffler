//! The `/` search controller shared by the screens.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Flow, Pane, Screen, diff_row_text, tree_row_label};
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
                Pane::Diff => d.cursor,
            }),
            Screen::Logs => self.logs.as_ref().map_or(0, |v| v.cursor),
            Screen::Graph | Screen::Runs => 0,
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
            Screen::Logs => self.logs.as_ref().map_or_else(Vec::new, |view| {
                view.rows()
                    .iter()
                    .enumerate()
                    .map(|(i, row)| (i, view.row_text(*row).to_owned()))
                    .collect()
            }),
            Screen::Graph | Screen::Runs => Vec::new(),
        }
    }

    pub(super) fn diff_search_rows(&self) -> Vec<(usize, String)> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let model = diff.model(&self.review);
        match diff.focus {
            Pane::List => diff
                .tree_rows(model)
                .iter()
                .enumerate()
                .map(|(i, r)| (i, tree_row_label(&r.node)))
                .collect(),
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
            Screen::Log => {
                if let Some(l) = self.log.as_mut() {
                    l.cursor = row;
                }
            }
            Screen::Diff => {
                if let Some(d) = self.diff.as_mut() {
                    match d.focus {
                        Pane::List => d.tree_cursor = row,
                        Pane::Diff => d.cursor = row,
                    }
                }
            }
            Screen::Logs => {
                if let Some(v) = self.logs.as_mut() {
                    v.cursor = row;
                }
            }
            Screen::Graph | Screen::Runs => {}
        }
    }
}
