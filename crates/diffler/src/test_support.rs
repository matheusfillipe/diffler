//! Shared test fixtures: deterministic git repos for App and render tests.
//! Snapshots depend on the commit oid, so commits use a fixed signature time
//! and the repo lives in a fixed-name subdirectory of the tempdir.

// fixture helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use diffler_core::review::Review;
use tempfile::TempDir;

use crate::event::AppEvent;

pub struct Fixture {
    _dir: TempDir,
    pub root: PathBuf,
    pub repo: git2::Repository,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("fixture");
        std::fs::create_dir(&root).expect("repo dir");
        let mut options = git2::RepositoryInitOptions::new();
        options.initial_head("main");
        let repo = git2::Repository::init_opts(&root, &options).expect("init");
        let mut config = repo.config().expect("config");
        config.set_str("user.name", "test").expect("config");
        config.set_str("user.email", "test@test").expect("config");
        // pin line endings so checkout restores exact bytes; without this,
        // Windows autocrlf re-CRLFs on discard and the file still reads dirty
        config.set_str("core.autocrlf", "false").expect("config");
        config.set_str("core.eol", "lf").expect("config");
        drop(config);
        Self {
            _dir: dir,
            root,
            repo,
        }
    }

    pub fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    pub fn stage(&self, rel: &str) {
        let mut index = self.repo.index().expect("index");
        index.add_path(Path::new(rel)).expect("add");
        index.write().expect("index write");
    }

    pub fn commit_all(&self, message: &str) {
        let mut index = self.repo.index().expect("index");
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .expect("add");
        index.write().expect("index write");
        let tree_id = index.write_tree().expect("tree");
        let tree = self.repo.find_tree(tree_id).expect("find tree");
        let time = git2::Time::new(1_700_000_000, 0);
        let sig = git2::Signature::new("test", "test@test", &time).expect("sig");
        let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        self.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .expect("commit");
    }

    pub fn branch(&self, name: &str) {
        let head = self
            .repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .expect("head commit");
        self.repo.branch(name, &head, false).expect("branch");
    }

    pub fn review(&self) -> Review {
        Review::open(&self.root).expect("review")
    }
}

/// One untracked + one modified-unstaged + one staged-new file, exactly the
/// shape the snapshot tests assert.
pub fn standard_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    41\n}\n");
    fixture.write("notes.txt", "alpha\n");
    fixture.commit_all("initial commit");
    fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n");
    fixture.write("ci.yml", "on: push\n");
    fixture.stage("ci.yml");
    fixture.write("todo.md", "- [ ] review\n");
    fixture
}

/// One committed 20-line file with unstaged edits at both ends, far enough
/// apart (context is 3 lines) to produce exactly two hunks.
pub fn two_hunk_fixture() -> Fixture {
    let fixture = Fixture::new();
    let lines: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
    let original = lines.join("\n") + "\n";
    fixture.write("data.txt", &original);
    fixture.commit_all("initial commit");
    let edited = original
        .replace("line 1\n", "line one\n")
        .replace("line 20\n", "line twenty\n");
    fixture.write("data.txt", &edited);
    fixture
}

/// Plain key press; `\t` and `\n` map to Tab/Enter.
pub fn key(c: char) -> AppEvent {
    let code = match c {
        '\t' => KeyCode::Tab,
        '\n' => KeyCode::Enter,
        c => KeyCode::Char(c),
    };
    let modifiers = if c.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    AppEvent::Key(KeyEvent::new(code, modifiers))
}

pub fn ctrl_key(c: char) -> AppEvent {
    AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

pub fn esc_key() -> AppEvent {
    AppEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
}

pub fn mouse_scroll(down: bool, col: u16, row: u16) -> AppEvent {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let kind = if down {
        MouseEventKind::ScrollDown
    } else {
        MouseEventKind::ScrollUp
    };
    AppEvent::Mouse(MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

pub fn mouse_click(col: u16, row: u16) -> AppEvent {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}
