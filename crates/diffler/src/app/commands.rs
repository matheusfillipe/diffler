//! Everything executable on the current screen: the active keymap's bindings
//! plus the reachable which-key leaves. Feeds the command palette.

use crate::keymap::{Action, render_chord};
use crate::transient::TransientKind;

use super::{App, Screen};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    pub action: Action,
    pub label: &'static str,
    /// Full chord path for which-key leaves, e.g. "c a".
    pub chord: String,
}

impl App {
    pub(crate) fn command_index_haystack(&self) -> (Vec<Command>, Vec<String>) {
        let commands = self.command_index();
        let haystack = commands
            .iter()
            .map(|c| format!("{} {} {}", c.label, c.action.name(), c.chord))
            .collect();
        (commands, haystack)
    }

    pub(crate) fn command_index(&self) -> Vec<Command> {
        let keymap = self.active_keymap();
        let mut commands: Vec<Command> = keymap
            .bindings()
            .iter()
            .map(|(chord, action)| Command {
                action: *action,
                label: action.label(),
                chord: render_chord(chord),
            })
            .collect();
        if self.screen() == Screen::Status {
            for kind in TransientKind::ALL {
                let Some(prefix) = keymap.prefix_chord(kind) else {
                    continue;
                };
                commands.extend(
                    self.transient(kind)
                        .flat_entries()
                        .map(|(key, entry)| Command {
                            action: entry.action,
                            label: entry.action.label(),
                            chord: format!("{prefix} {key}"),
                        }),
                );
            }
        }
        commands
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::keymap::Action;
    use crate::test_support::standard_fixture;

    #[test]
    fn status_index_covers_bindings_and_transient_leaves() {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        let commands = app.command_index();
        let find = |action: Action| {
            commands
                .iter()
                .find(|c| c.action == action)
                .unwrap_or_else(|| panic!("{} listed", action.name()))
        };
        assert_eq!(find(Action::Stage).chord, "s");
        assert_eq!(find(Action::CommitAmend).chord, "c a");
        assert_eq!(find(Action::PushSetUpstream).chord, "P u");
    }

    #[test]
    fn every_action_has_a_nonempty_label() {
        for action in Action::ALL {
            assert!(!action.label().is_empty(), "{} labelled", action.name());
        }
    }
}
