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

    /// Delete a tracked file and commit the removal, so `rel` is gone from
    /// both HEAD's tree and the worktree: a revision moved on from code a
    /// walkthrough was pinned to.
    pub(crate) fn remove_and_commit(&self, rel: &str, message: &str) {
        std::fs::remove_file(self.root.join(rel)).expect("remove");
        let mut index = self.repo.index().expect("index");
        index.remove_path(Path::new(rel)).expect("index remove");
        index.write().expect("index write");
        self.commit_all(message);
    }

    pub(crate) fn commit_all(&self, message: &str) {
        // fixed time: snapshots pin on the commit oid, which a real clock would churn
        let time = git2::Time::new(1_700_000_000, 0);
        let sig = git2::Signature::new("test", "test@test", &time).expect("sig");
        diffler_core::test_git::commit_all(&self.repo, message, &sig);
    }

    /// Commit with an explicit timestamp, for tests that assert on commit time.
    pub(crate) fn commit_all_at(&self, message: &str, unix: i64) {
        let time = git2::Time::new(unix, 0);
        let sig = git2::Signature::new("test", "test@test", &time).expect("sig");
        diffler_core::test_git::commit_all(&self.repo, message, &sig);
    }

    pub(crate) fn remote(&self, name: &str, url: &str) {
        self.repo.remote(name, url).expect("remote");
    }

    pub(crate) fn track(&self, branch: &str, at: &str) {
        diffler_core::test_git::track(&self.repo, branch, at);
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

/// Populate `session` with a walkthrough over `stops`, each
/// `(title, anchor, body)`, as the agent comments a stop is. Ids and times
/// are fixed so a snapshot never churns on them, and the anchors stay
/// unresolved until the worker answers, exactly as a fresh publish leaves
/// them. `session` is the caller's own choice, but only a session for a
/// `ReviewSource::Walkthrough` means anything once seated.
pub(crate) fn seat_walkthrough_session(
    session: &mut diffler_core::session::Session,
    id: &str,
    title: &str,
    stops: &[(&str, Option<&str>, &str)],
) {
    use diffler_core::session::{Anchor, Comment, CommentStatus};
    use diffler_core::walkthrough::{Target, Walkthrough};

    let path_of = |anchor: &str| Target::parse(anchor).path().to_owned();
    let fallback = stops
        .iter()
        .filter_map(|(_, anchor, _)| *anchor)
        .map(path_of)
        .next()
        .unwrap_or_default();
    let ids = stops
        .iter()
        .enumerate()
        .map(|(index, (stop_title, anchor, body))| {
            let comment_id = format!("stop-{index}");
            session.comments.push(Comment {
                id: comment_id.clone(),
                author: "agent".to_owned(),
                remote_id: None,
                thread_id: None,
                anchor: Anchor {
                    file: anchor.map_or_else(|| fallback.clone(), path_of),
                    line: None,
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                title: Some((*stop_title).to_owned()),
                anchor_ref: anchor.map(str::to_owned),
                body: (*body).to_owned(),
                status: CommentStatus::Open,
                replies: Vec::new(),
                at: 1_700_000_000,
            });
            comment_id
        })
        .collect::<Vec<String>>();
    session.set_walkthrough(Walkthrough {
        id: id.to_owned(),
        title: title.to_owned(),
        author: "agent".to_owned(),
        at: 1_700_000_000,
        stops: ids,
        skipped: None,
        summary: None,
        rev: None,
    });
}

/// Give the open walkthrough source `id` a summary, the way a revision that
/// passes `summary` would.
pub(crate) fn set_walkthrough_summary(app: &mut App, id: &str, summary: &str) {
    let source = diffler_core::source::ReviewSource::walkthrough(id);
    if let Some(walkthrough) = app.review.session_for_mut(&source).walkthrough.as_mut() {
        walkthrough.summary = Some(summary.to_owned());
    }
    app.review.save_for(&source).expect("save walkthrough");
}

/// Seat a walkthrough as its own review source (id `w1`), the way a fresh
/// `publish_walkthrough` leaves one, and refresh the status screen's cached
/// listing of walkthroughs so it shows up there too. Returns the source, for
/// a caller that goes on to open it.
pub(crate) fn seat_walkthrough(
    app: &mut App,
    title: &str,
    stops: &[(&str, Option<&str>, &str)],
) -> diffler_core::source::ReviewSource {
    seat_walkthrough_at(app, "w1", title, stops)
}

/// Like [`seat_walkthrough`], naming the walkthrough's id, for a test that
/// needs more than one.
pub(crate) fn seat_walkthrough_at(
    app: &mut App,
    id: &str,
    title: &str,
    stops: &[(&str, Option<&str>, &str)],
) -> diffler_core::source::ReviewSource {
    let source = diffler_core::source::ReviewSource::walkthrough(id);
    app.review
        .ensure_source(&source)
        .expect("ensure walkthrough source");
    seat_walkthrough_session(app.review.session_for_mut(&source), id, title, stops);
    app.review.save_for(&source).expect("save walkthrough");
    app.reload_walkthroughs();
    source
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

/// A 200-line file with one line changed near the middle, far enough from
/// both ends that a walkthrough stop's window has real, bounded context on
/// both sides rather than running into the file's edges.
pub(crate) fn big_file_fixture() -> Fixture {
    let fixture = Fixture::new();
    let lines: Vec<String> = (1..=200).map(|i| format!("line {i}")).collect();
    fixture.write("big.txt", &(lines.join("\n") + "\n"));
    fixture.commit_all("base");
    let mut edited = lines;
    edited[99] = "line 100 edited".to_owned();
    fixture.write("big.txt", &(edited.join("\n") + "\n"));
    fixture
}

/// A 200-line file with 70 contiguous lines (51-120) rewritten: one big
/// change wide enough that a stop spanning it exceeds a window's row budget.
pub(crate) fn huge_span_fixture() -> Fixture {
    let fixture = Fixture::new();
    let lines: Vec<String> = (1..=200).map(|i| format!("line {i}")).collect();
    fixture.write("big.txt", &(lines.join("\n") + "\n"));
    fixture.commit_all("base");
    let mut edited = lines;
    for entry in edited.iter_mut().take(120).skip(50) {
        entry.push_str(" edited");
    }
    fixture.write("big.txt", &(edited.join("\n") + "\n"));
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
