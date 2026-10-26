//! The repository's worktrees: the status group, the worktree screen, the
//! picker, and the git ops that add and remove one. A worktree opens as a
//! project tab.

use std::path::{Component, Path, PathBuf};

use crossterm::event::KeyEvent;
use diffler_core::vcs::{Worktree, WorktreeOp};

use super::fuzzy::{FuzzyKey, FuzzyList, selected};
use super::status::Row;
use super::tabs::TabOp;
use super::{App, BranchAction, InputOp, Modal, PendingOp, Screen};
use crate::keymap::Action;

/// Several git ops can be in flight, so these labels mark the ones whose
/// result `worktree_finished` takes over.
const ADD_LABEL: &str = "add worktree ";
const REMOVE_LABEL: &str = "remove worktree ";

impl App {
    pub(crate) fn adopt_worktrees(&mut self, worktrees: Vec<Worktree>) {
        self.worktrees_cursor = self.worktrees_cursor.min(worktrees.len().saturating_sub(1));
        self.status.worktrees = worktrees;
    }

    /// `false` when the listing failed, which we report, since the reader
    /// asked for it.
    fn read_worktrees(&mut self) -> bool {
        match self.review.vcs.worktrees() {
            Ok(worktrees) => {
                self.adopt_worktrees(worktrees);
                true
            }
            Err(err) => {
                self.error(err.to_string());
                false
            }
        }
    }

    fn worktree_on(&self, branch: &str) -> Option<Worktree> {
        self.status
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(branch))
            .cloned()
    }

    pub(crate) fn open_worktrees(&mut self) {
        if !self.read_worktrees() {
            return;
        }
        self.worktrees_cursor = self
            .status
            .worktrees
            .iter()
            .position(|w| w.current)
            .unwrap_or(0);
        self.push_screen(Screen::Worktrees);
    }

    pub(super) fn dispatch_worktrees(&mut self, action: Action) {
        if let Some(cursor) =
            super::list_motion(self.worktrees_cursor, self.status.worktrees.len(), action)
        {
            self.worktrees_cursor = cursor;
            return;
        }
        let worktree = self.status.worktrees.get(self.worktrees_cursor).cloned();
        match (action, worktree) {
            (Action::Open, Some(worktree)) => self.open_worktree(&worktree),
            (Action::Yank | Action::CopyUrl, Some(worktree)) => {
                let path = worktree.path.display().to_string();
                self.copy_or_report(Some(path.clone()), &path);
            }
            _ => {}
        }
    }

    pub(crate) fn open_worktree(&mut self, worktree: &Worktree) {
        if worktree.missing {
            self.info(format!(
                "press x to forget {}, its folder is gone",
                worktree.label
            ));
        } else if worktree.current {
            self.info(format!(
                "pick another worktree, {} is the one on screen",
                worktree.label
            ));
        } else {
            self.pending_tab = Some(TabOp::Open(worktree.path.clone()));
        }
    }

    pub(crate) fn remove_worktree_here(&mut self) {
        let under_cursor = match self.screen() {
            Screen::Worktrees => self.status.worktrees.get(self.worktrees_cursor).cloned(),
            Screen::Status => match self.cursor_row() {
                Some(Row::Worktree { index }) => self.status.worktrees.get(index).cloned(),
                _ => None,
            },
            _ => None,
        };
        match under_cursor {
            Some(worktree) => self.confirm_remove_worktree(worktree, false),
            None => self.open_worktree_picker(true),
        }
    }

    pub(crate) fn confirm_remove_worktree(&mut self, worktree: Worktree, force: bool) {
        if worktree.main {
            self.info("pick a linked worktree, the main one stays with the repository");
            return;
        }
        if worktree.current {
            self.info("switch to another tab to remove the worktree on screen");
            return;
        }
        let argv = match self.review.vcs.worktree_argv(&WorktreeOp::Remove {
            worktree: worktree.clone(),
            force,
        }) {
            Ok(argv) => argv,
            Err(err) => {
                self.error(err.to_string());
                return;
            }
        };
        let message = if force {
            format!(
                "{} has changes you would lose. Remove it anyway?",
                worktree.label
            )
        } else if worktree.missing {
            format!("Forget worktree {}? Its folder is gone.", worktree.label)
        } else {
            format!("Remove worktree {}?", worktree.label)
        };
        let label = format!("{REMOVE_LABEL}{}", worktree.label);
        take_keyed(&mut self.pending_worktree_remove, &label);
        self.pending_worktree_remove.push((label.clone(), worktree));
        self.modal = Some(Modal::Confirm {
            message,
            on_confirm: PendingOp::RunGit { label, argv },
        });
    }

    /// jj names the new workspace and starts a change in it; it makes no
    /// bookmark.
    pub(crate) fn worktree_branch_input(&mut self) {
        let title = if self.review.vcs.has_index() {
            "New branch in a new worktree"
        } else {
            "New workspace name"
        };
        self.open_input(title.to_owned(), String::new(), InputOp::WorktreeBranch);
    }

    pub(crate) fn worktree_branch_list(&mut self) {
        self.open_branch_list(BranchAction::Worktree);
    }

    /// A branch some worktree already has checked out opens that worktree,
    /// since git checks a branch out in one worktree at a time.
    pub(crate) fn add_worktree(&mut self, branch: &str, new_branch: bool) {
        if !new_branch && let Some(worktree) = self.worktree_on(branch) {
            self.open_worktree(&worktree);
            return;
        }
        let path = self.new_worktree_path(branch);
        if path.exists() {
            self.error(format!(
                "{} already exists, set [worktree] path to put new worktrees elsewhere",
                path.display()
            ));
            return;
        }
        match self.review.vcs.worktree_argv(&WorktreeOp::Add {
            path: path.clone(),
            branch: branch.to_owned(),
            new_branch,
        }) {
            Ok(argv) => {
                let label = format!("{ADD_LABEL}{branch}");
                take_keyed(&mut self.pending_worktree_open, &label);
                self.pending_worktree_open.push((label.clone(), path));
                self.queue_network(label, argv);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    /// git refuses to fetch into a branch a worktree has checked out, so we
    /// open that worktree as it stands.
    pub(crate) fn pr_worktree(&mut self, pr: &crate::ci::PullRequest) {
        if let Some(worktree) = self.worktree_on(&pr.head_ref) {
            self.open_worktree(&worktree);
        } else if self.queue_pr_fetch(pr) {
            self.pending_pr_worktree = Some((Self::pr_fetch_label(pr.number), pr.head_ref.clone()));
        }
    }

    /// `[worktree] path` resolved against the main worktree, with `..` folded
    /// so the tab and the messages show a plain path.
    fn new_worktree_path(&self, branch: &str) -> PathBuf {
        let main = self
            .status
            .worktrees
            .iter()
            .find(|w| w.main)
            .map_or_else(|| self.review.repo_root.clone(), |w| w.path.clone());
        let repo = main
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative = self
            .config
            .worktree
            .path
            .replace("{repo}", &repo)
            .replace("{branch}", &branch.replace('/', "-"));
        fold_dots(&main.join(relative))
    }

    /// `true` when `label` was a worktree op, which we report here.
    pub(super) fn worktree_finished(&mut self, label: &str, ok: bool, output: &str) -> bool {
        if let Some(branch) = label.strip_prefix(ADD_LABEL) {
            let path = take_keyed(&mut self.pending_worktree_open, label);
            if !ok {
                return false;
            }
            self.queue_refresh();
            self.info(format!("created worktree {branch}"));
            self.pending_tab = path.map(TabOp::Open);
            return true;
        }
        let Some(name) = label.strip_prefix(REMOVE_LABEL) else {
            return false;
        };
        let Some(worktree) = take_keyed(&mut self.pending_worktree_remove, label) else {
            return false;
        };
        if ok {
            self.queue_refresh();
            if worktree.path.exists() {
                self.info(format!(
                    "removed worktree {name}, its folder stays at {}",
                    worktree.path.display()
                ));
            } else {
                self.info(format!("removed worktree {name}"));
            }
            self.pending_tab = Some(TabOp::Drop(worktree.path));
            return true;
        }
        if output.contains("--force") {
            self.message = None;
            self.confirm_remove_worktree(worktree, true);
            return true;
        }
        false
    }

    pub(crate) fn open_worktree_picker(&mut self, remove: bool) {
        if !self.read_worktrees() {
            return;
        }
        let worktrees: Vec<Worktree> = self
            .status
            .worktrees
            .iter()
            .filter(|w| !(w.current || remove && w.main))
            .cloned()
            .collect();
        if worktrees.is_empty() {
            self.info("no other worktree yet, w c to create one");
            return;
        }
        let mut list = FuzzyList::default();
        list.rerank(&haystack(&worktrees));
        self.modal = Some(Modal::WorktreeList {
            worktrees,
            list,
            remove,
        });
    }

    pub(super) fn handle_worktree_list_key(&mut self, key: &KeyEvent) {
        let Some(Modal::WorktreeList {
            worktrees,
            list,
            remove,
        }) = self.modal.as_mut()
        else {
            return;
        };
        match list.feed(key) {
            FuzzyKey::Submit => {
                // a query matching nothing keeps the dialog open, like fzf
                let Some(worktree) = selected(list, worktrees).cloned() else {
                    return;
                };
                let remove = *remove;
                self.modal = None;
                if remove {
                    self.confirm_remove_worktree(worktree, false);
                } else {
                    self.open_worktree(&worktree);
                }
            }
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => list.rerank(&haystack(worktrees)),
            FuzzyKey::Consumed | FuzzyKey::Other => {}
        }
    }
}

fn take_keyed<T>(pending: &mut Vec<(String, T)>, label: &str) -> Option<T> {
    let index = pending.iter().position(|(key, _)| key == label)?;
    Some(pending.remove(index).1)
}

fn haystack(worktrees: &[Worktree]) -> Vec<String> {
    worktrees
        .iter()
        .map(|w| format!("{} {}", w.label, w.path.display()))
        .collect()
}

fn fold_dots(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                folded.pop();
            }
            Component::CurDir => {}
            other => folded.push(other),
        }
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tabs::folder_key;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, key, standard_fixture};

    /// The folder `name` beside the fixture repo, as git reports it: macOS
    /// resolves the temp folder through `/private`.
    fn beside(fixture: &Fixture, name: &str) -> PathBuf {
        fixture
            .root
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .expect("temp folder")
            .join(name)
    }

    /// `standard_fixture` plus a linked worktree on `feat`, beside the repo.
    fn linked_fixture() -> (Fixture, PathBuf) {
        let fixture = standard_fixture();
        fixture.branch("feat");
        let linked = beside(&fixture, "fixture-feat");
        {
            let feat = fixture
                .repo
                .find_branch("feat", git2::BranchType::Local)
                .expect("branch")
                .into_reference();
            let mut options = git2::WorktreeAddOptions::new();
            options.reference(Some(&feat));
            fixture
                .repo
                .worktree("fixture-feat", &linked, Some(&options))
                .expect("add worktree");
        }
        (fixture, linked)
    }

    /// We key a tab op by its folder so the test compares git's spelling of a
    /// path with ours on every platform.
    fn tab_key(op: Option<&TabOp>) -> Option<String> {
        match op? {
            TabOp::Open(path) => Some(format!("open {}", folder_key(path))),
            TabOp::Drop(path) => Some(format!("drop {}", folder_key(path))),
            other => Some(format!("{other:?}")),
        }
    }

    fn argv_keys(argv: &[impl AsRef<str>]) -> Vec<String> {
        argv.iter()
            .map(|arg| folder_key(Path::new(arg.as_ref())))
            .collect()
    }

    fn worktree_row(app: &App) -> usize {
        app.visible_rows()
            .iter()
            .position(|row| matches!(row, Row::Worktree { index: 1 }))
            .expect("linked worktree row")
    }

    fn done(app: &mut App, label: &str, ok: bool, output: &str) {
        app.handle(AppEvent::GitDone {
            label: label.to_owned(),
            ok,
            output: output.to_owned(),
        });
    }

    #[test]
    fn the_status_group_shows_only_once_a_second_worktree_exists() {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| matches!(row, Row::WorktreesHeader { .. }))
        );
        let (fixture, linked) = linked_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.status.cursor = worktree_row(&app);
        app.handle(key('\n'));
        assert_eq!(
            tab_key(app.pending_tab.as_ref()),
            tab_key(Some(&TabOp::Open(linked)))
        );
    }

    #[test]
    fn creating_a_worktree_adds_it_beside_the_repo_and_opens_it_as_a_tab() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('w'));
        app.handle(key('c'));
        for c in "feat/x".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        let git = app.pending_git.take().expect("add queued");
        let path = beside(&fixture, "fixture-feat-x");
        assert_eq!(
            argv_keys(&git.argv),
            argv_keys(&[
                "git",
                "worktree",
                "add",
                "-b",
                "feat/x",
                &path.display().to_string()
            ])
        );
        done(&mut app, &git.label, true, "");
        assert_eq!(
            tab_key(app.pending_tab.as_ref()),
            tab_key(Some(&TabOp::Open(path)))
        );
    }

    #[test]
    fn removing_a_worktree_asks_again_before_forcing_and_closes_its_tab() {
        let (fixture, linked) = linked_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.status.cursor = worktree_row(&app);
        app.handle(key('x'));
        app.confirm_modal();
        let git = app.pending_git.take().expect("remove queued");
        assert_eq!(
            argv_keys(&git.argv),
            argv_keys(&["git", "worktree", "remove", &linked.display().to_string()])
        );
        done(
            &mut app,
            &git.label,
            false,
            "fatal: contains modified or untracked files, use --force to delete it",
        );
        app.confirm_modal();
        let forced = app.pending_git.take().expect("forced remove queued");
        assert!(forced.argv.contains(&"--force".to_owned()), "{forced:?}");
        done(&mut app, &forced.label, true, "");
        assert_eq!(
            tab_key(app.pending_tab.as_ref()),
            tab_key(Some(&TabOp::Drop(linked)))
        );
    }

    #[test]
    fn the_worktree_on_screen_and_the_main_one_stay() {
        let (fixture, linked) = linked_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let main = app.status.worktrees[0].clone();
        app.confirm_remove_worktree(main, false);
        assert_eq!(app.modal, None);
        let refusal = app.message.expect("refused").text;
        assert!(refusal.contains("main one stays"), "{refusal}");
        let mut linked_app = App::new(
            diffler_core::review::Review::open(&linked).expect("open linked"),
            LoadedConfig::default(),
        );
        let here = linked_app.status.worktrees[1].clone();
        assert!(here.current);
        linked_app.confirm_remove_worktree(here, false);
        assert_eq!(linked_app.modal, None);
        let refusal = linked_app.message.expect("refused").text;
        assert!(refusal.contains("another tab"), "{refusal}");
    }

    #[test]
    fn two_adds_in_flight_each_open_their_own_worktree() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.add_worktree("first", true);
        let first = app.pending_git.take().expect("first add");
        app.add_worktree("second", true);
        let second = app.pending_git.take().expect("second add");
        done(&mut app, &second.label, true, "");
        assert_eq!(
            tab_key(app.pending_tab.take().as_ref()),
            tab_key(Some(&TabOp::Open(beside(&fixture, "fixture-second"))))
        );
        done(&mut app, &first.label, true, "");
        assert_eq!(
            tab_key(app.pending_tab.as_ref()),
            tab_key(Some(&TabOp::Open(beside(&fixture, "fixture-first"))))
        );
    }

    #[test]
    fn another_pull_requests_fetch_leaves_the_worktree_waiting() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.pending_pr_worktree = Some((App::pr_fetch_label(7), "feat/widgets".to_owned()));
        done(&mut app, &App::pr_fetch_label(9), true, "");
        assert!(app.pending_pr_worktree.is_some());
        assert!(app.pending_git.is_none(), "no add for #9's fetch");
    }

    #[test]
    fn a_pull_request_worktree_fetches_its_head_then_adds_the_branch() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let pr = crate::ci::PullRequest {
            number: 7,
            title: "Add widgets".to_owned(),
            url: None,
            base_ref: "main".to_owned(),
            head_ref: "feat/widgets".to_owned(),
            head_oid: "0".repeat(40),
            author: "reviewer".to_owned(),
        };
        app.pr_worktree(&pr);
        let fetch = app.pending_git.take().expect("fetch queued");
        assert_eq!(
            fetch.argv,
            [
                "git",
                "fetch",
                "origin",
                "refs/pull/7/head:refs/heads/feat/widgets"
            ]
        );
        done(&mut app, &fetch.label, true, "");
        let add = app.pending_git.take().expect("add queued");
        let path = beside(&fixture, "fixture-feat-widgets");
        assert_eq!(
            argv_keys(&add.argv),
            argv_keys(&[
                "git",
                "worktree",
                "add",
                &path.display().to_string(),
                "feat/widgets"
            ])
        );
    }

    #[test]
    fn the_worktree_screen_moves_and_opens_like_the_other_lists() {
        let (fixture, linked) = linked_fixture();
        let mut app = App::new(
            diffler_core::review::Review::open(&linked).expect("open linked"),
            LoadedConfig::default(),
        );
        app.open_worktrees();
        assert_eq!(app.screen(), Screen::Worktrees);
        assert_eq!(app.worktrees_cursor, 1, "the cursor starts on this one");
        app.handle(key('k'));
        app.handle(key('\n'));
        let main = fixture.root.canonicalize().expect("canonical root");
        assert_eq!(
            tab_key(app.pending_tab.as_ref()),
            tab_key(Some(&TabOp::Open(main)))
        );
    }

    #[test]
    fn fold_dots_drops_parent_steps() {
        assert_eq!(
            fold_dots(Path::new("/work/widgets/../widgets-feat")),
            PathBuf::from("/work/widgets-feat")
        );
    }
}
