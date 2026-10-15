//! The jj change menu: its verbs act on the commit under the log's cursor, or
//! on `@` from every other screen.

use diffler_core::vcs::{ChangeOp, JJ_ONLY};

use super::{App, InputOp, Modal, PendingOp, Screen};
use crate::keymap::Action;

pub(super) fn is_change_action(action: Action) -> bool {
    matches!(
        action,
        Action::ChangeNew
            | Action::ChangeEdit
            | Action::ChangeDescribe
            | Action::ChangeSquash
            | Action::ChangeAbandon
            | Action::ChangeUndo
            | Action::ChangeRedo
    )
}

impl App {
    pub(super) fn change_menu(&mut self, action: Action) {
        if self.review.vcs.has_index() {
            self.error(JJ_ONLY);
            return;
        }
        match action {
            Action::ChangeUndo => {
                self.run_change(&ChangeOp::Undo, "undid the last jj operation".into());
                return;
            }
            Action::ChangeRedo => {
                self.run_change(&ChangeOp::Redo, "redid the last undone jj operation".into());
                return;
            }
            _ => {}
        }
        let Some((rev, label)) = self.change_target() else {
            self.info("move to a commit to act on it");
            return;
        };
        let working_copy = rev == "@";
        match action {
            Action::ChangeNew => {
                self.run_change(&ChangeOp::New(rev), format!("started a change on {label}"));
            }
            Action::ChangeEdit if working_copy => {
                self.info("pick a commit in the log to edit; @ is already the working copy");
            }
            Action::ChangeEdit => self.run_change(&ChangeOp::Edit(rev), format!("editing {label}")),
            Action::ChangeDescribe => self.describe_input(rev, label),
            Action::ChangeSquash => self.run_change(
                &ChangeOp::Squash(rev),
                format!("squashed {label} into its parent"),
            ),
            Action::ChangeAbandon => {
                let message = if working_copy {
                    format!("Abandon {label} and drop its edits from the working copy?")
                } else {
                    format!("Abandon {label}?")
                };
                self.modal = Some(Modal::Confirm {
                    message,
                    on_confirm: PendingOp::Change {
                        op: ChangeOp::Abandon(rev),
                        done: format!("abandoned {label}"),
                    },
                });
            }
            _ => {}
        }
    }

    /// The revision the menu acts on and the id a reader knows it by.
    fn change_target(&self) -> Option<(String, String)> {
        if self.screen() == Screen::Log {
            let log = self.log.as_ref()?;
            let entry = log.entries.get(log.cursor)?;
            return Some((entry.oid.clone(), entry.short_id().to_owned()));
        }
        let label = self
            .change
            .as_ref()
            .map_or_else(|| "@".to_owned(), |change| change.change_id.clone());
        Some(("@".to_owned(), label))
    }

    /// The input opens on the full description, read from the commit the view
    /// last saw, so a multi-line one survives an edit of its subject.
    fn describe_input(&mut self, rev: String, label: String) {
        let oid = if rev == "@" {
            self.change.as_ref().map(|change| change.commit_id.clone())
        } else {
            Some(rev.clone())
        };
        let current = oid
            .and_then(|oid| self.review.vcs.commit_message(&oid).ok())
            .map(|message| message.trim_end().to_owned())
            .unwrap_or_default();
        self.open_input(
            format!("Describe {label}"),
            current,
            InputOp::Describe { rev, label },
        );
    }

    pub(super) fn run_change(&mut self, op: &ChangeOp, done: String) {
        self.message = None;
        self.vcs_op(|vcs| vcs.change(op));
        if self.message.is_none() {
            self.info(done);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, InputOp, Modal, Screen};
    use crate::config::LoadedConfig;
    use crate::keymap::Action;
    use crate::test_support::{jj_fixture, key, standard_fixture};
    use crate::transient::TransientKind;

    fn jj(root: &std::path::Path, args: &[&str]) -> String {
        diffler_core::test_git::jj(root, args)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle(key(c));
        }
    }

    #[test]
    fn describe_from_the_status_screen_rewrites_the_working_copy_description() {
        let fixture = jj_fixture();
        jj(&fixture.root, &["describe", "--message=draft"]);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('C'));
        assert_eq!(app.transient.map(|t| t.kind), Some(TransientKind::Change));
        app.handle(key('d'));
        let Some(Modal::Input {
            buffer, on_submit, ..
        }) = app.modal.as_ref()
        else {
            panic!("describe opens an input: {:?}", app.modal);
        };
        assert_eq!(buffer, "draft", "prefilled with the current description");
        assert!(matches!(on_submit, InputOp::Describe { rev, .. } if rev == "@"));
        type_text(&mut app, " done\n");
        let message = app.message.clone().expect("reported");
        assert!(message.text.starts_with("described "), "{}", message.text);
        let description = jj(
            &fixture.root,
            &["log", "--no-graph", "-r", "@", "-T", "description"],
        );
        assert_eq!(description.trim(), "draft done");
    }

    #[test]
    fn abandon_from_the_log_asks_then_drops_the_commit_under_the_cursor() {
        let fixture = jj_fixture();
        fixture.write("todo.md", "- [x] review\n");
        jj(&fixture.root, &["commit", "--message=finish review"]);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        assert_eq!(app.screen(), Screen::Log);
        let log = app.log.as_ref().expect("log");
        assert_eq!(log.entries[0].subject, "finish review");
        app.handle(key('c'));
        assert_eq!(app.transient.map(|t| t.kind), Some(TransientKind::Change));
        app.handle(key('a'));
        assert!(
            matches!(app.modal, Some(Modal::Confirm { .. })),
            "asks first"
        );
        app.handle(key('y'));
        app.settle_refresh();
        let subjects: Vec<String> = app
            .log
            .as_ref()
            .expect("log")
            .entries
            .iter()
            .map(|e| e.subject.clone())
            .collect();
        assert!(
            !subjects.contains(&"finish review".to_owned()),
            "{subjects:?}"
        );
    }

    #[test]
    fn new_and_undo_from_the_status_screen_move_the_working_copy() {
        let fixture = jj_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let before = app.change.clone().expect("@").change_id;
        app.handle(key('C'));
        app.handle(key('n'));
        app.settle_refresh();
        let after = app.change.clone().expect("@").change_id;
        assert_ne!(before, after, "a new change is the working copy");
        app.handle(key('C'));
        app.handle(key('u'));
        app.settle_refresh();
        assert_eq!(app.change.clone().expect("@").change_id, before);
        app.handle(key('C'));
        app.handle(key('U'));
        app.settle_refresh();
        assert_eq!(app.change.expect("@").change_id, after);
    }

    #[test]
    fn the_change_menu_is_unbound_in_a_git_repo() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('C'));
        assert_eq!(app.transient, None, "the change menu stays shut");
        assert_eq!(app.message, None, "a stray key says nothing");
        assert!(
            !app.command_index()
                .iter()
                .any(|command| command.action == Action::ChangeNew)
        );
    }

    #[test]
    fn the_log_lists_its_change_menu_in_the_palette() {
        let fixture = jj_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        let chords: Vec<String> = app
            .command_index()
            .into_iter()
            .filter(|command| command.action == Action::ChangeNew)
            .map(|command| command.chord)
            .collect();
        assert_eq!(chords, ["c n"]);
    }

    #[test]
    fn an_empty_description_clears_the_working_copy_description() {
        let fixture = jj_fixture();
        jj(&fixture.root, &["describe", "--message=draft"]);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('C'));
        app.handle(key('d'));
        for _ in 0.."draft".len() {
            app.handle(crate::test_support::key_backspace());
        }
        app.handle(key('\n'));
        let description = jj(
            &fixture.root,
            &["log", "--no-graph", "-r", "@", "-T", "description"],
        );
        assert_eq!(description, "");
    }

    #[test]
    fn abandoning_the_working_copy_warns_that_its_edits_go() {
        let fixture = jj_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('C'));
        app.handle(key('a'));
        let Some(Modal::Confirm { message, .. }) = app.modal.as_ref() else {
            panic!("asks first: {:?}", app.modal);
        };
        assert!(message.contains("drop its edits"), "{message}");
    }

    #[test]
    fn editing_the_working_copy_from_the_status_screen_points_at_the_log() {
        let fixture = jj_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('C'));
        app.handle(key('e'));
        let message = app.message.expect("declined");
        assert!(
            message.text.contains("pick a commit in the log"),
            "{}",
            message.text
        );
    }
}
