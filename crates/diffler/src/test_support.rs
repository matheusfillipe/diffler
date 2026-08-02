//! Shared test fixtures: deterministic git repos for App and render tests.
//! Snapshots depend on the commit oid, so commits use a fixed signature time
//! and the repo lives in a fixed-name subdirectory of the tempdir.

// fixture helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use diffler_core::review::Review;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;

use crate::app::App;
use crate::event::AppEvent;

pub(crate) struct Fixture {
    _dir: TempDir,
    pub root: PathBuf,
    pub repo: git2::Repository,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("fixture");
        std::fs::create_dir(&root).expect("repo dir");
        // Windows autocrlf re-CRLFs on discard, so a stale checkout still reads
        // dirty; init_repo pins core.autocrlf/eol to keep it byte-exact.
        let repo = diffler_core::test_git::init_repo(&root, Some("main"));
        Self {
            _dir: dir,
            root,
            repo,
        }
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    pub(crate) fn stage(&self, rel: &str) {
        let mut index = self.repo.index().expect("index");
        index.add_path(Path::new(rel)).expect("add");
        index.write().expect("index write");
    }

    pub(crate) fn commit_all(&self, message: &str) {
        // fixed time: snapshots pin on the commit oid, which a real clock would churn
        let time = git2::Time::new(1_700_000_000, 0);
        let sig = git2::Signature::new("test", "test@test", &time).expect("sig");
        diffler_core::test_git::commit_all(&self.repo, message, &sig);
    }

    pub(crate) fn remote(&self, name: &str, url: &str) {
        self.repo.remote(name, url).expect("remote");
    }

    pub(crate) fn branch(&self, name: &str) {
        let head = self
            .repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .expect("head commit");
        self.repo.branch(name, &head, false).expect("branch");
    }

    /// Point HEAD at an existing branch. The fixture's branches share one
    /// worktree, so nothing needs checking out.
    pub(crate) fn checkout(&self, name: &str) {
        self.repo
            .set_head(&format!("refs/heads/{name}"))
            .expect("set head");
    }

    pub(crate) fn review(&self) -> Review {
        Review::open(&self.root).expect("review")
    }
}

/// A `main` base commit, a `feature` branch one commit ahead of it, and an
/// uncommitted file on top: what a three-dot review against the base shows.
pub(crate) fn branch_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write("base.rs", "pub fn base() {}\n");
    fixture.commit_all("base");
    fixture.branch("feature");
    fixture.checkout("feature");
    fixture.write("landed.rs", "pub fn landed() -> u32 {\n    1\n}\n");
    fixture.commit_all("feature work");
    fixture.write("dirty.rs", "pub fn dirty() {}\n");
    fixture
}

/// One untracked + one modified-unstaged + one staged-new file, exactly the
/// shape the snapshot tests assert.
pub(crate) fn standard_fixture() -> Fixture {
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
pub(crate) fn two_hunk_fixture() -> Fixture {
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
pub(crate) fn key(c: char) -> AppEvent {
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

pub(crate) fn ctrl_key(c: char) -> AppEvent {
    AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

pub(crate) fn code_key(code: KeyCode) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub(crate) fn esc_key() -> AppEvent {
    AppEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
}

pub(crate) fn key_backspace() -> AppEvent {
    AppEvent::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
}

pub(crate) fn mouse_scroll(down: bool, col: u16, row: u16) -> AppEvent {
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

pub(crate) fn mouse_click(col: u16, row: u16) -> AppEvent {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

pub(crate) fn mouse_drag(col: u16, row: u16) -> AppEvent {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// Render through the top-level draw so modal overlays and screen switching
/// are covered too. The first draw only queues enrichment (intra-line
/// emphasis, syntax highlight); run it and draw again so the snapshot
/// captures the settled frame, as the real app converges to.
pub(crate) fn render(app: &mut App) -> Terminal<TestBackend> {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .expect("draw");
    app.enrich_now();
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .expect("draw");
    terminal
}

pub(crate) fn mouse_right_click(col: u16, row: u16) -> AppEvent {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    AppEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}
