//! The context menu a right-click, or a press held still, opens over a row:
//! what the reader can do to the thing under the pointer, each entry naming
//! the key that does the same, so the menu also teaches the keys.

use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthStr;

use super::fuzzy::FuzzyList;
use super::{App, DiffRow, Modal, Pane, Screen};
use crate::keymap::Action;

/// How long a press has to stay still before it opens the menu, for touch
/// screens and trackpads with no right button.
const HOLD: Duration = Duration::from_millis(500);

/// A left press the reader has not let go of yet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeldPress {
    col: u16,
    row: u16,
    since: Instant,
}

impl HeldPress {
    pub(crate) fn new(col: u16, row: u16) -> Self {
        Self {
            col,
            row,
            since: Instant::now(),
        }
    }
}

impl App {
    /// Open the menu for the thing at `(col, row)`. A click lands there
    /// first so every entry acts on what the reader pointed at; an open
    /// selection stays, so the entries act on the whole range.
    pub(super) fn open_context_menu(&mut self, col: u16, row: u16) {
        let region = self.menu_region(col);
        if !self.visual_active() {
            self.press_at(col, row);
        }
        if let (Screen::Diff, Some(pane)) = (self.screen(), region) {
            self.diff_focus(pane);
        }
        let keymap = self.active_keymap();
        let entries: Vec<(Action, &str, String)> = self
            .menu_actions()
            .into_iter()
            .filter_map(|action| Some((action, action.label(), keymap.chord_for(action)?)))
            .collect();
        let widest = entries
            .iter()
            .map(|(_, label, _)| label.width())
            .max()
            .unwrap_or(0);
        let (actions, labels): (Vec<Action>, Vec<String>) = entries
            .into_iter()
            .map(|(action, label, chord)| {
                let pad = " ".repeat(widest - label.width());
                (action, format!("{label}{pad}  {chord}"))
            })
            .unzip();
        if actions.is_empty() {
            return;
        }
        let mut list = FuzzyList::default();
        list.rerank(&labels);
        self.modal = Some(Modal::Menu {
            actions,
            labels,
            list,
        });
    }

    /// Open the menu once a press has stayed still long enough.
    pub(super) fn check_held_press(&mut self) -> bool {
        let Some(held) = self.held_press else {
            return false;
        };
        if held.since.elapsed() < HOLD {
            return false;
        }
        self.held_press = None;
        if self.modal.is_some() || self.composer_open() {
            return false;
        }
        self.open_context_menu(held.col, held.row);
        true
    }

    /// Which diff pane a column falls in, so the menu reaches that pane's
    /// verbs.
    fn menu_region(&self, col: u16) -> Option<Pane> {
        let diff = self.diff.as_ref()?;
        if self.comments_col(col) {
            Some(Pane::Comments)
        } else if col < diff.pane.x {
            Some(Pane::List)
        } else {
            Some(Pane::Diff)
        }
    }

    /// The verbs that fit the thing under the cursor on this screen.
    fn menu_actions(&self) -> Vec<Action> {
        match self.screen() {
            Screen::Diff => self.diff_menu_actions(),
            Screen::Status => vec![
                Action::Open,
                Action::Stage,
                Action::Unstage,
                Action::Discard,
                Action::OpenEditor,
                Action::Blame,
                Action::CopyUrl,
            ],
            Screen::Log | Screen::CiLog | Screen::Prs | Screen::Runs => {
                vec![Action::Open, Action::CopyUrl, Action::CopyFileFeedback]
            }
            Screen::Graph | Screen::File | Screen::Stats => Vec::new(),
        }
    }

    fn diff_menu_actions(&self) -> Vec<Action> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let on_comment = matches!(diff.rows().get(diff.cursor), Some(DiffRow::Comment { .. }));
        match diff.focus {
            Pane::List => vec![
                Action::Open,
                Action::MarkViewed,
                Action::OpenEditor,
                Action::Blame,
                Action::CopyFileFeedback,
            ],
            Pane::Comments => comment_actions(),
            Pane::Diff | Pane::References if on_comment => comment_actions(),
            Pane::Diff | Pane::References if self.visual_active() => {
                vec![Action::Comment, Action::CopyFileFeedback]
            }
            Pane::Diff | Pane::References => vec![
                Action::Comment,
                Action::VisualSelect,
                Action::SymbolLens,
                Action::OpenEditor,
                Action::Blame,
                Action::MarkViewed,
                Action::CopyFileFeedback,
            ],
        }
    }
}

fn comment_actions() -> Vec<Action> {
    vec![
        Action::Reply,
        Action::Resolve,
        Action::DeleteComment,
        Action::ClaimComment,
        Action::ToggleFold,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, render};

    #[test]
    fn a_press_held_still_opens_the_menu() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "one\n");
        fixture.commit_all("base");
        fixture.write("a.txt", "two\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        render(&mut app);
        let pane = app.diff.as_ref().expect("diff").pane;
        let mut held = HeldPress::new(pane.x + 6, pane.y + 1);
        held.since = Instant::now()
            .checked_sub(HOLD * 2)
            .expect("a past instant");
        app.held_press = Some(held);
        app.handle(AppEvent::Tick);
        assert!(matches!(app.modal, Some(Modal::Menu { .. })));
    }
}
