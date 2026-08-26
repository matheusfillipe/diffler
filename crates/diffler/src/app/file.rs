//! The whole-file view and its blame column, plus the fuzzy picker that
//! reaches any tracked file. The diff screens only ever list changed files,
//! so this is the one way into a file the review does not touch.

use diffler_core::highlight::StyledRange;
use diffler_core::vcs::BlameSpan;

use super::fuzzy::{FuzzyKey, FuzzyList, name_haystack, selected};
use super::rowsel::{RowSelect, RowText};
use super::{App, Flow, Modal, Screen};
use crate::keymap::Action;

/// What choosing a file in the picker does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    View,
    Blame,
    Editor,
}

/// A file the view is waiting on, dispatched to the blocking pool.
#[derive(Debug, Clone)]
pub struct FileOpen {
    pub path: String,
    /// Line to seat the cursor on once the content lands, 1-based.
    pub line: Option<u32>,
    pub blame: bool,
    /// The request this load answers. A result whose token no longer matches
    /// the app's is an answer to a question the user has moved on from, and
    /// installing it would resurrect a screen they left.
    pub token: u64,
}

#[derive(Debug)]
pub struct FileView {
    pub path: String,
    pub lines: Vec<String>,
    /// One entry per line; empty for a language with no grammar.
    pub highlights: Vec<Vec<StyledRange>>,
    pub spans: Vec<BlameSpan>,
    /// Span index per line, so the gutter costs one lookup per row.
    span_of_line: Vec<Option<usize>>,
    pub cursor: usize,
    pub scroll: usize,
    /// Line where `V` started; `Some` means range selection is active.
    pub visual_anchor: Option<usize>,
    pub show_blame: bool,
    /// Body height of the last draw, so paging steps a real screenful.
    pub viewport: u16,
}

impl FileView {
    pub fn new(
        path: String,
        content: &str,
        highlights: Vec<Vec<StyledRange>>,
        spans: Vec<BlameSpan>,
        show_blame: bool,
    ) -> Self {
        let lines: Vec<String> = content.lines().map(str::to_owned).collect();
        let span_of_line = index_spans(&spans, lines.len());
        Self {
            path,
            lines,
            highlights,
            spans,
            span_of_line,
            cursor: 0,
            scroll: 0,
            visual_anchor: None,
            show_blame,
            viewport: 0,
        }
    }

    /// The blame span owning a 0-based line.
    pub fn span_at(&self, line: usize) -> Option<&BlameSpan> {
        self.span_of_line
            .get(line)
            .copied()
            .flatten()
            .and_then(|index| self.spans.get(index))
    }

    /// True when this line opens a new commit's run, which is where the
    /// gutter prints the commit.
    pub fn starts_span(&self, line: usize) -> bool {
        let here = self.span_of_line.get(line).copied().flatten();
        let above = line
            .checked_sub(1)
            .and_then(|prev| self.span_of_line.get(prev).copied().flatten());
        here.is_some() && here != above
    }

    /// Commit under the cursor, when blame has one to give.
    pub fn cursor_commit(&self) -> Option<&BlameSpan> {
        self.span_at(self.cursor).filter(|span| span.committed)
    }

    fn step_span(&mut self, forward: bool) {
        let mut line = self.cursor;
        loop {
            let next = if forward {
                line + 1
            } else {
                match line.checked_sub(1) {
                    Some(next) => next,
                    None => return,
                }
            };
            if next >= self.lines.len() {
                return;
            }
            line = next;
            if self.starts_span(line) {
                self.cursor = line;
                return;
            }
        }
    }
}

impl RowSelect for FileView {
    fn cursor(&self) -> usize {
        self.cursor
    }

    fn anchor(&self) -> Option<usize> {
        self.visual_anchor
    }

    fn set_anchor(&mut self, anchor: Option<usize>) {
        self.visual_anchor = anchor;
    }
}

impl RowText for FileView {
    fn row_count(&self) -> usize {
        self.lines.len()
    }

    fn row_text(&self, row: usize) -> String {
        self.lines.get(row).cloned().unwrap_or_default()
    }
}

/// Map each line onto the span that owns it. Spans are line runs, so a line
/// no span covers (blame gave up on it) stays `None` and renders plain.
fn index_spans(spans: &[BlameSpan], line_count: usize) -> Vec<Option<usize>> {
    let mut out = vec![None; line_count];
    for (index, span) in spans.iter().enumerate() {
        let start = span.start_line.saturating_sub(1) as usize;
        let end = start.saturating_add(span.line_count as usize);
        for slot in out.iter_mut().take(end).skip(start) {
            *slot = Some(index);
        }
    }
    out
}

impl App {
    /// Open the file view on a repo-relative path. The content and blame land
    /// through a worker, so the caller returns immediately.
    pub(crate) fn open_file(&mut self, path: &str, line: Option<u32>, blame: bool) {
        self.file_token += 1;
        self.pending_file = Some(FileOpen {
            path: path.to_owned(),
            line,
            blame,
            token: self.file_token,
        });
        self.info(format!("opening {path}"));
    }

    /// Abandon whatever file load is in flight, so its answer never lands on a
    /// screen the user has since left.
    pub(crate) fn cancel_file_load(&mut self) {
        self.file_token += 1;
        self.pending_file = None;
    }

    /// Blame whatever file the current screen has focused, at its line.
    pub(crate) fn blame_focused(&mut self) {
        let target = match self.screen() {
            Screen::Diff => self.diff_cursor_file_line(),
            Screen::Status => self.status_cursor_file_line(),
            Screen::File => self
                .file
                .as_ref()
                .map(|view| (view.path.clone(), Some(view.cursor as u32 + 1))),
            Screen::Log
            | Screen::Runs
            | Screen::Graph
            | Screen::Prs
            | Screen::CiLog
            | Screen::Stats => None,
        };
        let Some((path, line)) = target else {
            self.info("no file under the cursor");
            return;
        };
        self.open_file(&path, line, true);
    }

    pub(crate) fn on_file_loaded(
        &mut self,
        result: Result<FileView, String>,
        line: Option<u32>,
        token: u64,
    ) -> Flow {
        if token != self.file_token {
            return Flow::Idle;
        }
        match result {
            Ok(view) => self.install_file(view, line),
            Err(err) => self.error(err),
        }
        Flow::Continue
    }

    pub(crate) fn install_file(&mut self, view: FileView, line: Option<u32>) {
        let mut view = view;
        if let Some(line) = line {
            view.cursor = (line.saturating_sub(1) as usize).min(view.lines.len().saturating_sub(1));
        }
        self.file = Some(view);
        self.message = None;
        if self.screen() != Screen::File {
            self.push_screen(Screen::File);
        }
    }

    pub(super) fn dispatch_file(&mut self, action: Action) {
        let Some(view) = self.file.as_mut() else {
            return;
        };
        let last = view.lines.len().saturating_sub(1);
        let height = usize::from(view.viewport);
        let page = |full| super::page_step(view.viewport, full);
        match action {
            Action::MoveDown => view.cursor = (view.cursor + 1).min(last),
            Action::MoveUp => view.cursor = view.cursor.saturating_sub(1),
            Action::GoTop => view.cursor = 0,
            Action::GoBottom => view.cursor = last,
            Action::HalfPageDown => view.cursor = (view.cursor + page(false)).min(last),
            Action::HalfPageUp => view.cursor = view.cursor.saturating_sub(page(false)),
            Action::FullPageDown => view.cursor = (view.cursor + page(true)).min(last),
            Action::FullPageUp => view.cursor = view.cursor.saturating_sub(page(true)),
            Action::CenterCursor => view.scroll = view.cursor.saturating_sub(height / 2),
            Action::CursorTop => view.scroll = view.cursor,
            Action::CursorBottom => {
                view.scroll = view.cursor.saturating_sub(height.saturating_sub(1));
            }
            Action::NextSection => view.step_span(true),
            Action::PrevSection => view.step_span(false),
            Action::ToggleBlame => view.show_blame = !view.show_blame,
            Action::VisualSelect => view.toggle_visual(),
            Action::CopyFileFeedback | Action::CopyAllFeedback => {
                self.yank_rows("yanked lines");
            }
            Action::Open => self.open_cursor_commit(),
            Action::OpenEditor => {
                let (path, line) = (view.path.clone(), view.cursor as u32 + 1);
                self.request_editor(&path, Some(line));
            }
            _ => {}
        }
    }

    /// Enter on a blamed line reviews the commit that wrote it.
    fn open_cursor_commit(&mut self) {
        let Some(oid) = self
            .file
            .as_ref()
            .and_then(|view| view.cursor_commit().map(|span| span.oid.clone()))
        else {
            self.info("no commit for this line");
            return;
        };
        self.open_commit_diff(&oid);
    }

    // --- the picker ---

    pub(crate) fn open_file_picker(&mut self) {
        let files = match self.review.vcs.tracked_files() {
            Ok(files) => files,
            Err(err) => {
                self.info(format!("cannot list files: {err}"));
                return;
            }
        };
        if files.is_empty() {
            self.info("no tracked files");
            return;
        }
        let paths: Vec<String> = files
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect();
        let mut list = FuzzyList::typing();
        list.rerank(&name_haystack(&paths));
        self.modal = Some(Modal::FilePicker { paths, list });
    }

    pub(super) fn handle_file_picker_key(&mut self, key: &crossterm::event::KeyEvent) -> Flow {
        let Some(Modal::FilePicker { paths, list }) = self.modal.as_mut() else {
            return Flow::Continue;
        };
        match list.feed(key) {
            FuzzyKey::Submit => self.take_picked_file(FileAction::View),
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => {
                let haystack = name_haystack(paths);
                list.rerank(&haystack);
            }
            // list focus leaves the dialog's own verbs free
            FuzzyKey::Other => match key.code {
                crossterm::event::KeyCode::Char('b') => self.take_picked_file(FileAction::Blame),
                crossterm::event::KeyCode::Char('e') => self.take_picked_file(FileAction::Editor),
                _ => {}
            },
            FuzzyKey::Consumed => {}
        }
        Flow::Continue
    }

    #[cfg(test)]
    pub(crate) fn picker_paths(&self) -> Vec<String> {
        match &self.modal {
            Some(Modal::FilePicker { paths, .. }) => paths.clone(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn take_picked_file(&mut self, action: FileAction) {
        let Some(Modal::FilePicker { paths, list }) = &self.modal else {
            return;
        };
        // a query matching nothing keeps the dialog open, like fzf
        let Some(path) = selected(list, paths).cloned() else {
            return;
        };
        self.modal = None;
        match action {
            FileAction::View => self.open_file(&path, None, false),
            FileAction::Blame => self.open_file(&path, None, true),
            FileAction::Editor => self.request_editor(&path, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::vcs::BlameSpan;

    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, key, standard_fixture};

    /// The fixture rides along: dropping it deletes the repo the app reads
    /// lazily, and the diff model would come back empty.
    fn app() -> (Fixture, App) {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        (fixture, app)
    }

    fn span(start: u32, count: u32, committed: bool) -> BlameSpan {
        BlameSpan {
            start_line: start,
            line_count: count,
            oid: "a".repeat(40),
            oid7: "aaaaaaa".to_owned(),
            author: "reviewer".to_owned(),
            time_unix: 0,
            summary: "base".to_owned(),
            committed,
        }
    }

    fn view(text: &str, spans: Vec<BlameSpan>) -> FileView {
        FileView::new("src/lib.rs".to_owned(), text, Vec::new(), spans, true)
    }

    #[test]
    fn the_picker_lists_tracked_files_and_enter_opens_the_one_selected() {
        let (_fixture, mut app) = app();
        app.handle(key('g'));
        app.handle(key('f'));
        assert_eq!(
            app.picker_paths(),
            vec!["ci.yml", "notes.txt", "src/lib.rs"],
            "untracked todo.md stays out; staged ci.yml is in"
        );
        app.handle(key('\n'));
        let request = app.pending_file.as_ref().expect("a queued file");
        assert_eq!(request.path, "ci.yml");
        assert!(!request.blame, "enter opens the file, blame stays off");
    }

    #[test]
    fn the_picker_filters_on_typing_and_b_opens_the_match_with_blame() {
        let (_fixture, mut app) = app();
        app.handle(key('g'));
        app.handle(key('f'));
        for c in "lib".chars() {
            app.handle(key(c));
        }
        // tab out of the input so the dialog's own verbs get the key
        app.handle(key('\t'));
        app.handle(key('b'));
        let request = app.pending_file.as_ref().expect("a queued file");
        assert_eq!(request.path, "src/lib.rs");
        assert!(request.blame);
    }

    #[test]
    fn the_picker_sends_a_file_to_the_editor_without_opening_the_view() {
        let (_fixture, mut app) = app();
        app.handle(key('g'));
        app.handle(key('f'));
        app.handle(key('\t'));
        app.handle(key('e'));
        assert!(
            app.pending_file.is_none(),
            "the editor never loads the view"
        );
        assert!(app.pending_editor.is_some());
    }

    #[test]
    fn visual_select_yanks_the_lines_it_covers() {
        let (_fixture, mut app) = app();
        app.install_file(view("one\ntwo\nthree\nfour\n", Vec::new()), None);
        app.handle(key('V'));
        app.handle(key('j'));
        assert_eq!(
            app.file.as_ref().and_then(FileView::selection),
            Some((0, 1))
        );
        app.handle(key('y'));
        assert_eq!(app.pending_clipboard.as_deref(), Some("one\ntwo"));
        assert!(
            app.file.as_ref().is_some_and(|v| v.anchor().is_none()),
            "the yank drops the anchor"
        );
    }

    #[test]
    fn yank_without_a_selection_copies_the_cursor_line() {
        let (_fixture, mut app) = app();
        app.install_file(view("one\ntwo\nthree\n", Vec::new()), None);
        app.handle(key('j'));
        app.handle(key('y'));
        assert_eq!(app.pending_clipboard.as_deref(), Some("two"));
    }

    #[test]
    fn escape_drops_the_file_selection() {
        let (_fixture, mut app) = app();
        app.install_file(view("one\ntwo\n", Vec::new()), None);
        app.handle(key('V'));
        assert!(app.visual_active());
        app.handle(crate::test_support::esc_key());
        assert!(!app.visual_active());
    }

    /// Step `j` until `ready` holds, bounded so a regression fails instead of
    /// hanging the suite.
    fn seek(app: &mut App, ready: impl Fn(&App) -> bool) {
        for _ in 0..40 {
            if ready(app) {
                return;
            }
            app.handle(key('j'));
        }
        panic!("never reached the row the test needs");
    }

    #[test]
    fn blame_from_the_status_cursor_targets_the_file_under_it() {
        let (_fixture, mut app) = app();
        seek(&mut app, |app| app.status_cursor_file_line().is_some());
        let expected = app.status_cursor_file_line().expect("a file row").0;
        app.handle(key('B'));
        let request = app.pending_file.as_ref().expect("a queued file");
        assert_eq!(request.path, expected);
        assert!(request.blame);
    }

    #[test]
    fn blame_from_the_diff_cursor_carries_the_line_under_it() {
        let (_fixture, mut app) = app();
        // the single-file opener builds the rows and lands in the pane
        app.open_working_tree_file("src/lib.rs");
        seek(&mut app, |app| {
            app.diff_cursor_file_line()
                .is_some_and(|(_, line)| line.is_some())
        });
        let (path, line) = app.diff_cursor_file_line().expect("a diff line");
        app.handle(key('B'));
        let request = app.pending_file.as_ref().expect("a queued file");
        assert_eq!((request.path.clone(), request.line), (path, line));
        assert!(request.blame);
    }

    #[test]
    fn a_span_covers_its_run_and_only_its_first_line_starts_it() {
        let view = view(
            "one\ntwo\nthree\n",
            vec![span(1, 2, true), span(3, 1, true)],
        );
        assert!(view.starts_span(0));
        assert!(!view.starts_span(1), "the run continues");
        assert!(view.starts_span(2), "a new commit starts a new run");
        assert_eq!(view.span_at(1).map(|s| s.start_line), Some(1));
    }

    #[test]
    fn a_line_no_span_covers_has_no_commit() {
        let view = view("one\ntwo\n", vec![span(1, 1, true)]);
        assert!(view.span_at(1).is_none());
        assert!(!view.starts_span(1));
    }

    #[test]
    fn an_uncommitted_line_offers_no_commit_to_open() {
        let mut view = view("one\n", vec![span(1, 1, false)]);
        view.cursor = 0;
        assert!(
            view.cursor_commit().is_none(),
            "a worktree line has no commit to review"
        );
    }

    #[test]
    fn the_bracket_pair_steps_commit_blocks_and_b_toggles_the_column() {
        let (_fixture, mut app) = app();
        app.install_file(
            view(
                "one\ntwo\nthree\n",
                vec![span(1, 2, true), span(3, 1, true)],
            ),
            None,
        );
        app.handle(key(']'));
        assert_eq!(app.file.as_ref().expect("view").cursor, 2);
        app.handle(key('['));
        assert_eq!(app.file.as_ref().expect("view").cursor, 0);
        app.handle(key('b'));
        assert!(!app.file.as_ref().expect("view").show_blame);
    }

    #[test]
    fn a_load_the_user_navigated_away_from_never_lands() {
        let (_fixture, mut app) = app();
        app.open_working_tree_file("src/lib.rs");
        app.handle(key('B'));
        let stale = app.pending_file.as_ref().expect("a queued file").token;
        // back to the status screen while the load is still in flight
        app.handle(key('q'));

        let flow = app.on_file_loaded(Ok(view("one\n", vec![span(1, 1, true)])), None, stale);
        assert_eq!(flow, Flow::Idle, "a stale load draws nothing");
        assert!(app.file.is_none());
        assert_eq!(
            app.screen(),
            Screen::Status,
            "the abandoned file view never comes back"
        );
    }

    #[test]
    fn the_newest_request_wins_when_two_loads_are_in_flight() {
        let (_fixture, mut app) = app();
        app.open_file("a.txt", None, false);
        let first = app.pending_file.as_ref().expect("a queued file").token;
        app.open_file("b.txt", None, false);
        let second = app.pending_file.as_ref().expect("a queued file").token;

        app.on_file_loaded(Ok(view("second\n", vec![span(1, 1, true)])), None, second);
        app.on_file_loaded(Ok(view("first\n", vec![span(1, 1, true)])), None, first);
        assert_eq!(
            app.file.as_ref().expect("view").lines,
            vec!["second"],
            "the earlier load cannot overwrite the later one"
        );
    }

    #[test]
    fn opening_a_file_at_a_line_seats_the_cursor_there_and_clamps() {
        let (_fixture, mut app) = app();
        app.install_file(view("one\ntwo\n", vec![span(1, 2, true)]), Some(2));
        assert_eq!(app.file.as_ref().expect("view").cursor, 1);
        app.install_file(view("one\ntwo\n", vec![span(1, 2, true)]), Some(99));
        assert_eq!(
            app.file.as_ref().expect("view").cursor,
            1,
            "a line past the end lands on the last one"
        );
    }
}
