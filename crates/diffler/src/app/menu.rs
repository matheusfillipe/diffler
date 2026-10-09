//! The context menu a right-click or a held press opens over a row. Each entry
//! names its key, so the reader learns the keys from it.

use std::time::{Duration, Instant};

use super::fuzzy::FuzzyList;
use super::{App, Command, Modal, Screen};
use crate::keymap::Action;

/// How long a press has to stay still before it opens the menu, for touch
/// screens and trackpads with no right button.
const HOLD: Duration = Duration::from_millis(500);

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
    /// Open the menu for the thing at `(col, row)`. With `select`, we first
    /// select what the reader pointed at so every entry acts on it; an open
    /// selection stays, so the entries act on the whole range.
    pub(super) fn open_context_menu(&mut self, col: u16, row: u16, select: bool) {
        if select && !self.visual_active() {
            self.select_at(col, row);
        }
        let wanted = self.menu_actions();
        let index = self.command_index();
        let commands: Vec<Command> = wanted
            .iter()
            .filter_map(|action| index.iter().find(|command| command.action == *action))
            .cloned()
            .collect();
        if commands.is_empty() {
            return;
        }
        let labels: Vec<String> = commands
            .iter()
            .map(|command| command.label.to_owned())
            .collect();
        let mut list = FuzzyList::default();
        list.rerank(&labels);
        self.modal = Some(Modal::Menu { commands, list });
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
        // the press selected the row when it went down
        self.open_context_menu(held.col, held.row, false);
        true
    }

    /// The verbs that fit the thing under the cursor on this screen.
    fn menu_actions(&self) -> Vec<Action> {
        match self.screen() {
            Screen::Diff => self.diff_menu_actions(),
            Screen::Status => self.status_menu_actions(),
            Screen::Log | Screen::CiLog => {
                vec![Action::Open, Action::CopyUrl, Action::CopyFileFeedback]
            }
            // these screens select nothing on click, so a menu would act on
            // whatever row the keyboard left selected
            Screen::Prs | Screen::Runs | Screen::Graph | Screen::File | Screen::Stats => Vec::new(),
        }
    }
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
