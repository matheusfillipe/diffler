//! The project-tab side of one app: the tab row, the tab requests it passes to
//! the workspace, and the add-project picker.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use unicode_width::UnicodeWidthStr;

use super::fuzzy::{FuzzyKey, FuzzyList, name_haystack, selected};
use super::{App, Flow, Modal};
use crate::keymap::Action;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabStrip {
    pub names: Vec<String>,
    pub active: usize,
}

/// The words after the add-project key at the tab row's right edge.
pub const ADD_HINT: &str = " add project ";

impl TabStrip {
    pub fn label(index: usize, name: &str) -> String {
        format!(" {} {name} ", index + 1)
    }

    /// The tab request for a click on column `col`. The add hint sits at the
    /// right edge, `add_width` columns long, and counts only while the labels
    /// leave it room.
    pub fn hit(&self, col: u16, width: u16, add_width: u16) -> Option<TabOp> {
        let mut start = 0u16;
        for (index, name) in self.names.iter().enumerate() {
            let label = u16::try_from(Self::label(index, name).width()).unwrap_or(u16::MAX);
            if col >= start && col < start.saturating_add(label) {
                return Some(TabOp::Go(index));
            }
            // one blank column separates two labels
            start = start.saturating_add(label).saturating_add(1);
        }
        let hint = width.saturating_sub(add_width);
        (add_width > 0 && start <= hint && col >= hint).then_some(TabOp::Pick)
    }
}

/// A request only the workspace holding the tabs can carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabOp {
    Next,
    Prev,
    /// Tab number `n`, counted from 0.
    Go(usize),
    Close,
    Pick,
    Open(PathBuf),
}

pub fn tab_op(action: Action) -> Option<TabOp> {
    let op = match action {
        Action::NextTab => TabOp::Next,
        Action::PrevTab => TabOp::Prev,
        Action::AddProject => TabOp::Pick,
        Action::CloseTab => TabOp::Close,
        Action::GoTab1 => TabOp::Go(0),
        Action::GoTab2 => TabOp::Go(1),
        Action::GoTab3 => TabOp::Go(2),
        Action::GoTab4 => TabOp::Go(3),
        Action::GoTab5 => TabOp::Go(4),
        Action::GoTab6 => TabOp::Go(5),
        Action::GoTab7 => TabOp::Go(6),
        Action::GoTab8 => TabOp::Go(7),
        Action::GoTab9 => TabOp::Go(8),
        _ => return None,
    };
    Some(op)
}

impl App {
    /// Hand a tab action to the workspace. Closing waits until no draft or
    /// dialog is open, since it would take the unsaved text with the tab.
    pub(crate) fn request_tab(&mut self, action: Action) {
        let Some(op) = tab_op(action) else {
            return;
        };
        if op == TabOp::Close && (self.composer_open() || self.modal.is_some()) {
            self.info("finish or cancel the draft before closing the tab");
            return;
        }
        self.pending_tab = Some(op);
    }

    /// Open the add-project picker over `nearby`, the repositories next to
    /// the ones already open.
    pub fn open_project_picker(&mut self, nearby: Vec<String>) {
        let mut list = FuzzyList::typing();
        list.rerank(&name_haystack(&nearby));
        self.modal = Some(Modal::AddProject {
            entries: nearby.clone(),
            nearby,
            list,
        });
    }

    pub(super) fn handle_add_project_key(&mut self, key: &KeyEvent) -> Flow {
        let Some(Modal::AddProject {
            nearby,
            entries,
            list,
        }) = self.modal.as_mut()
        else {
            return Flow::Continue;
        };
        if matches!(key.code, KeyCode::Tab) {
            if let Some(entry) = selected(list, entries) {
                list.query = format!("{}/", entry.trim_end_matches('/'));
                list.cursor = list.query.chars().count();
                list.selected = 0;
                *entries = project_entries(&list.query, nearby);
                list.rerank(&name_haystack(entries));
            }
            return Flow::Continue;
        }
        match list.feed(key) {
            FuzzyKey::Submit => {
                let chosen = selected(list, entries)
                    .cloned()
                    .unwrap_or_else(|| list.query.clone());
                if !chosen.trim().is_empty() {
                    self.modal = None;
                    self.pending_tab = Some(TabOp::Open(expand_home(chosen.trim())));
                }
            }
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => {
                *entries = project_entries(&list.query, nearby);
                list.rerank(&name_haystack(entries));
            }
            FuzzyKey::Consumed | FuzzyKey::Other => {}
        }
        Flow::Continue
    }
}

/// What the picker lists for `query`: the folders inside the typed path's
/// parent when the query is a path, else the nearby repositories.
fn project_entries(query: &str, nearby: &[String]) -> Vec<String> {
    if !looks_like_path(query) {
        return nearby.to_vec();
    }
    // a bare `~`, `.` or `..` names a folder to list, the way its `/` form does
    let query = &if matches!(query, "~" | "." | "..") {
        format!("{query}/")
    } else {
        query.to_owned()
    };
    let typed = expand_home(query);
    let (dir, prefix) = if query.ends_with(std::path::is_separator) {
        (typed.as_path(), String::new())
    } else {
        (
            typed.parent().unwrap_or_else(|| Path::new("/")),
            typed
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        )
    };
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let shown_dir = query
        .rfind(std::path::is_separator)
        .map_or_else(String::new, |at| query[..=at].to_owned());
    let mut folders: Vec<String> = read
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.') || prefix.starts_with('.'))
        .filter(|name| name.to_lowercase().starts_with(&prefix))
        .map(|name| format!("{shown_dir}{name}"))
        .collect();
    folders.sort();
    folders
}

fn looks_like_path(query: &str) -> bool {
    query.starts_with(['/', '~', '.']) || Path::new(query).is_absolute()
}

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

/// The git repositories beside each of `roots`, the ones not already open,
/// shown with `~` for the home directory.
pub fn nearby_repos(roots: &[PathBuf]) -> Vec<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut found: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.parent())
        .filter_map(|parent| std::fs::read_dir(parent).ok())
        .flat_map(|read| read.filter_map(Result::ok).map(|entry| entry.path()))
        .filter(|path| path.join(".git").exists() && !roots.contains(path))
        .collect();
    found.sort();
    found.dedup();
    found
        .iter()
        .map(|path| {
            match home
                .as_deref()
                .and_then(|home| path.strip_prefix(home).ok())
            {
                Some(rest) => format!("~/{}", rest.display()),
                None => path.display().to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_path_lists_the_folders_it_could_complete_to() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["api", "apollo", "web", ".hidden"] {
            std::fs::create_dir(dir.path().join(name)).expect("folder");
        }
        std::fs::write(dir.path().join("apex.txt"), "").expect("file");
        let root = dir.path().display().to_string();
        assert_eq!(
            project_entries(&format!("{root}/ap"), &[]),
            [format!("{root}/api"), format!("{root}/apollo")],
            "folders only, by prefix"
        );
        assert_eq!(
            project_entries(&format!("{root}/"), &[]).len(),
            3,
            "a trailing slash lists the folder, hidden ones left out"
        );
    }

    #[test]
    fn a_click_on_the_tab_row_picks_the_tab_or_the_add_hint() {
        let strip = TabStrip {
            names: vec!["api".to_owned(), "web".to_owned()],
            active: 0,
        };
        assert_eq!(
            strip.hit(1, 80, 18),
            Some(TabOp::Go(0)),
            "inside \" 1 api \""
        );
        assert_eq!(strip.hit(7, 80, 18), None, "the gap between labels");
        assert_eq!(
            strip.hit(9, 80, 18),
            Some(TabOp::Go(1)),
            "inside \" 2 web \""
        );
        assert_eq!(strip.hit(70, 80, 18), Some(TabOp::Pick), "the add hint");
        assert_eq!(strip.hit(40, 80, 18), None, "empty row");
    }

    #[test]
    fn an_overflowing_tab_row_sends_no_click_to_the_hidden_hint() {
        let strip = TabStrip {
            names: (0..8).map(|n| format!("project-{n}")).collect(),
            active: 0,
        };
        assert_eq!(
            strip.hit(75, 80, 18),
            Some(TabOp::Go(5)),
            "the label drawn there"
        );
    }

    #[test]
    fn a_bare_tilde_lists_the_home_folder() {
        let listed = project_entries("~", &[]);
        assert!(
            listed.iter().all(|entry| entry.starts_with("~/")),
            "{listed:?}"
        );
    }

    #[test]
    fn a_name_lists_the_nearby_repositories() {
        let nearby = vec!["~/projects/api".to_owned()];
        assert_eq!(project_entries("ap", &nearby), nearby);
    }

    #[test]
    fn nearby_repositories_are_the_open_ones_siblings_with_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["open", "sibling", "plain"] {
            std::fs::create_dir(dir.path().join(name)).expect("folder");
        }
        std::fs::create_dir(dir.path().join("open/.git")).expect("git");
        std::fs::create_dir(dir.path().join("sibling/.git")).expect("git");
        let found = nearby_repos(&[dir.path().join("open")]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].ends_with("sibling"));
    }
}
