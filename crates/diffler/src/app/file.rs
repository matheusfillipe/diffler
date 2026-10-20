//! The whole-file view and its blame column, plus the fuzzy picker that
//! reaches any tracked file, including files the review does not touch.

use diffler_core::highlight::{Highlighter, StyledRange};
use diffler_core::review::Review;
use diffler_core::vcs::{BlameSpan, FileVersion};

use super::fuzzy::{FuzzyKey, FuzzyList, name_haystack, selected};
use super::rowsel::{RowSelect, RowText};
use super::{App, Flow, Modal, Screen};
use crate::keymap::Action;

/// How many past versions one history read lists.
pub const HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    View,
    Blame,
    Editor,
}

/// A file the view is waiting on, dispatched to the blocking pool.
#[derive(Debug, Clone)]
pub struct FileOpen {
    /// The path as the version read has it.
    pub path: String,
    /// Rows the reference covers, 1-based and inclusive.
    pub span: Option<(u32, u32)>,
    pub blame: bool,
    /// Whether this load recolours the open view, keeping the reader's place.
    pub reload: bool,
    /// The past version to read; `None` reads the working tree.
    pub at: Option<VersionAt>,
    /// A history read elsewhere, for the view to step through.
    pub history: Option<FileHistory>,
    /// We drop a result whose token no longer matches the app's, so it never
    /// reopens a screen the user left.
    pub token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionAt {
    pub oid: String,
    /// Index into [`FileHistory::versions`].
    pub index: usize,
}

/// The commits that changed a file, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHistory {
    /// The working-tree path the history was read for.
    pub path: String,
    pub versions: Vec<FileVersion>,
}

/// What a history read does once it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryNext {
    Step {
        older: bool,
    },
    List,
    /// Update an open list in place; nothing opens.
    Refresh,
}

#[derive(Debug, Clone)]
pub struct HistoryRequest {
    pub path: String,
    pub then: HistoryNext,
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
    /// Rows a reference brought the reader here for, 0-based and inclusive.
    pub referenced: Option<(usize, usize)>,
    /// Line where `V` started; `Some` means range selection is active.
    pub visual_anchor: Option<usize>,
    pub show_blame: bool,
    /// Body height of the last draw, so paging steps a real screenful.
    pub viewport: u16,
    pub history: Option<FileHistory>,
    /// The past version on screen, an index into the history; `None` is the
    /// working tree.
    pub version: Option<usize>,
    /// The step a load in flight brings, so a quick second step counts from
    /// there.
    requested: Option<Requested>,
}

#[derive(Debug, Clone, Copy)]
struct Requested {
    version: Option<usize>,
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
            referenced: None,
            visual_anchor: None,
            show_blame,
            viewport: 0,
            history: None,
            version: None,
            requested: None,
        }
    }

    /// The path the working tree has, whichever version is on screen.
    pub fn worktree_path(&self) -> &str {
        self.history
            .as_ref()
            .map_or(self.path.as_str(), |history| history.path.as_str())
    }

    /// The past version on screen, `None` on the working tree.
    pub fn shown_version(&self) -> Option<&FileVersion> {
        self.history.as_ref()?.versions.get(self.version?)
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

/// Read, blame and highlight what `request` names, for a worker.
pub fn load_file(
    root: &std::path::Path,
    highlighter: &Highlighter,
    request: FileOpen,
) -> Result<FileView, String> {
    let snapshot = match &request.at {
        Some(at) => Review::compute_file_at(root, &at.oid, &request.path),
        None => Review::compute_file(root, &request.path),
    }
    .map_err(|err| format!("{}: {err}", request.path))?;
    let highlights = highlighter.highlight(&snapshot.path, &snapshot.content);
    let mut view = FileView::new(
        snapshot.path,
        &snapshot.content,
        highlights,
        snapshot.blame,
        request.blame,
    );
    view.version = request.at.map(|at| at.index);
    view.history = request.history;
    Ok(view)
}

/// The version one step away from `current` among `count`, `None` being the
/// working tree, which sits newer than every committed version.
fn next_version(
    current: Option<usize>,
    count: usize,
    older: bool,
) -> Result<Option<usize>, StepEnd> {
    match (current, older) {
        (None, true) if count == 0 => Err(StepEnd::Uncommitted),
        (None, true) => Ok(Some(0)),
        (Some(index), true) if index + 1 >= count => Err(StepEnd::Oldest),
        (Some(index), true) => Ok(Some(index + 1)),
        (None, false) => Err(StepEnd::Newest),
        (Some(index), false) => Ok(index.checked_sub(1)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepEnd {
    Uncommitted,
    Oldest,
    Newest,
}

/// Map each line onto the span that owns it. A line no span covers stays
/// `None` and renders plain.
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
    /// Open the file view on a repo-relative path. A worker reads the content
    /// and blame.
    pub(crate) fn open_file(&mut self, path: &str, span: Option<(u32, u32)>, blame: bool) {
        self.file_token += 1;
        self.pending_file = Some(FileOpen {
            path: path.to_owned(),
            span,
            blame,
            reload: false,
            at: None,
            history: None,
            token: self.file_token,
        });
        self.info(format!("opening {path}"));
    }

    /// Open the file view on one version of `history`, ready to step from.
    pub(crate) fn open_file_version(&mut self, history: FileHistory, index: usize) {
        let Some((path, oid)) = history
            .versions
            .get(index)
            .map(|version| (version.path.clone(), version.entry.oid.clone()))
        else {
            return;
        };
        self.file_token += 1;
        self.info(format!("opening {path}"));
        self.pending_file = Some(FileOpen {
            path,
            span: None,
            blame: true,
            reload: false,
            at: Some(VersionAt { oid, index }),
            history: Some(history),
            token: self.file_token,
        });
    }

    /// Load the open file again under the current highlighter, at the
    /// version on screen.
    pub(crate) fn reload_file(&mut self) {
        let Some(view) = self.file.as_mut() else {
            return;
        };
        view.requested = None;
        let version = view.version;
        self.queue_version(version);
    }

    /// Read the open file's `version` in its place, keeping the reader's line.
    fn queue_version(&mut self, version: Option<usize>) {
        let Some(view) = self.file.as_ref() else {
            return;
        };
        let at = version.and_then(|index| {
            let shown = view.history.as_ref()?.versions.get(index)?;
            Some((shown.path.clone(), shown.entry.oid.clone(), index))
        });
        let (path, at) = match at {
            Some((path, oid, index)) => (path, Some(VersionAt { oid, index })),
            None => (view.worktree_path().to_owned(), None),
        };
        self.file_token += 1;
        self.pending_file = Some(FileOpen {
            path,
            span: None,
            blame: view.show_blame,
            reload: true,
            at,
            history: None,
            token: self.file_token,
        });
    }

    /// A repo change can add commits, so we drop the history a working-tree
    /// view holds and the next step reads it again. A past version keeps its
    /// history, so stepping from it stays where it was.
    pub(crate) fn forget_worktree_history(&mut self) {
        if let Some(view) = self
            .file
            .as_mut()
            .filter(|view| view.version.is_none() && view.requested.is_none())
        {
            view.history = None;
        }
    }

    /// Abandon whatever file or history load is in flight.
    pub(crate) fn cancel_file_load(&mut self) {
        self.file_token += 1;
        self.pending_file = None;
        self.history_token += 1;
        self.pending_history = None;
    }

    fn queue_history(&mut self, path: &str, then: HistoryNext) {
        self.history_token += 1;
        self.pending_history = Some(HistoryRequest {
            path: path.to_owned(),
            then,
            token: self.history_token,
        });
        self.info(format!("reading the history of {path}"));
    }

    fn step_version(&mut self, older: bool) {
        let Some(view) = self.file.as_ref() else {
            return;
        };
        let Some(history) = view.history.as_ref() else {
            let path = view.worktree_path().to_owned();
            self.queue_history(&path, HistoryNext::Step { older });
            return;
        };
        let current = view.requested.map_or(view.version, |step| step.version);
        match next_version(current, history.versions.len(), older) {
            Ok(next) => {
                self.queue_version(next);
                if let Some(view) = self.file.as_mut() {
                    view.requested = Some(Requested { version: next });
                }
            }
            Err(end) => {
                let chord = |action| self.active_keymap().chord_for(action).unwrap_or_default();
                let text = match end {
                    StepEnd::Uncommitted => "commit this file to give it versions".to_owned(),
                    StepEnd::Oldest => format!(
                        "this is the oldest version; {} to see a newer one",
                        chord(Action::NewerVersion)
                    ),
                    StepEnd::Newest => format!(
                        "this is the working-tree file; {} to see an older version",
                        chord(Action::OlderVersion)
                    ),
                };
                self.info(text);
            }
        }
    }

    /// List the history of whatever file the current screen has focused.
    pub(crate) fn file_history_focused(&mut self) {
        let path = match self.screen() {
            Screen::File => self
                .file
                .as_ref()
                .map(|view| view.worktree_path().to_owned()),
            Screen::Log => self
                .log
                .as_ref()
                .and_then(|log| log.file.as_ref())
                .map(|history| history.path.clone()),
            _ => self.focused_file_line().map(|(path, _)| path),
        };
        match path {
            Some(path) => self.queue_history(&path, HistoryNext::List),
            None => self.info("no file under the cursor"),
        }
    }

    pub(crate) fn on_file_history(
        &mut self,
        request: &HistoryRequest,
        result: Result<Vec<FileVersion>, String>,
    ) -> Flow {
        let refresh = request.then == HistoryNext::Refresh;
        // we match a refresh to the open list by its path, since it carries no token
        if !refresh && request.token != self.history_token {
            return Flow::Idle;
        }
        let versions = match result {
            Ok(versions) => versions,
            Err(_) if refresh => return Flow::Continue,
            Err(err) => {
                self.error(err);
                return Flow::Continue;
            }
        };
        let history = FileHistory {
            path: request.path.clone(),
            versions,
        };
        if !refresh {
            self.message = None;
        }
        match request.then {
            HistoryNext::Refresh => {
                self.refresh_file_history(history);
            }
            HistoryNext::Step { older } => {
                if let Some(view) = self
                    .file
                    .as_mut()
                    .filter(|view| view.worktree_path() == history.path)
                {
                    view.history = Some(history);
                    self.step_version(older);
                }
            }
            HistoryNext::List if history.versions.is_empty() => {
                self.info(format!("commit {} to give it a history", history.path));
            }
            HistoryNext::List => self.show_file_history(history),
        }
        Flow::Continue
    }

    /// The file and line the current screen has under its cursor.
    fn focused_file_line(&self) -> Option<(String, Option<u32>)> {
        match self.screen() {
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
            | Screen::Pr
            | Screen::CiLog
            | Screen::Stats => None,
        }
    }

    /// Blame whatever file the current screen has focused, at its line. In a
    /// file's history that is the version under the cursor.
    pub(crate) fn blame_focused(&mut self) {
        if let Some((history, index)) = self.log.as_ref().and_then(|log| {
            let history = log.file.as_ref().filter(|_| self.screen() == Screen::Log)?;
            Some((history.clone(), log.cursor))
        }) {
            self.open_file_version(history, index);
            return;
        }
        let Some((path, line)) = self.focused_file_line() else {
            self.info("no file under the cursor");
            return;
        };
        self.open_file(&path, line.map(|line| (line, line)), true);
    }

    pub(crate) fn on_file_loaded(
        &mut self,
        result: Result<FileView, String>,
        span: Option<(u32, u32)>,
        reload: bool,
        token: u64,
    ) -> Flow {
        if token != self.file_token {
            return Flow::Idle;
        }
        match result {
            Ok(view) if reload => self.install_reloaded_file(view),
            Ok(view) => self.install_file(view, span),
            Err(err) => {
                if let Some(view) = self.file.as_mut() {
                    view.requested = None;
                }
                self.error(err);
            }
        }
        Flow::Continue
    }

    pub(crate) fn install_file(&mut self, view: FileView, span: Option<(u32, u32)>) {
        let mut view = view;
        if let Some((line, end)) = span {
            let last = view.lines.len().saturating_sub(1);
            let start = (line.saturating_sub(1) as usize).min(last);
            view.cursor = start;
            view.referenced = Some((start, (end.saturating_sub(1) as usize).min(last).max(start)));
        }
        self.raise_screen(Screen::File);
        self.file = Some(view);
        self.message = None;
    }

    /// We match the old view by token, so a renamed past version still keeps
    /// the reader's line.
    fn install_reloaded_file(&mut self, mut view: FileView) {
        if let Some(old) = self.file.take() {
            let last = view.lines.len().saturating_sub(1);
            view.cursor = old.cursor.min(last);
            view.scroll = old.scroll.min(last);
            view.referenced = old.referenced.filter(|_| old.version == view.version);
            view.visual_anchor = old.visual_anchor.map(|anchor| anchor.min(last));
            view.viewport = old.viewport;
            if view.history.is_none() {
                view.history = old.history;
            }
        }
        self.file = Some(view);
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
            Action::CopyUrl => self.copy_file_link(),
            Action::OpenUrl => self.open_file_link(),
            Action::Open => self.open_cursor_commit(),
            Action::OlderVersion => self.step_version(true),
            Action::NewerVersion => self.step_version(false),
            Action::OpenEditor if view.version.is_some() => {
                self.info("step back to the working-tree file to edit it");
            }
            Action::OpenEditor => {
                let (path, line) = (view.path.clone(), view.cursor as u32 + 1);
                self.request_editor(&path, Some(line));
            }
            _ => {}
        }
    }

    fn copy_file_link(&mut self) {
        match self.file_link() {
            Ok(url) => {
                self.info(format!("copied {url}"));
                self.pending_clipboard = Some(url);
                if let Some(view) = self.file.as_mut() {
                    view.visual_anchor = None;
                }
            }
            Err(err) => self.info(err),
        }
    }

    fn open_file_link(&mut self) {
        match self.file_link() {
            Ok(url) => self.open_or_report(Some(url), "this file"),
            Err(err) => self.info(err),
        }
    }

    /// The forge page for the selected lines: on the branch when it is pushed
    /// with nothing ahead, else on the remote's default branch.
    fn file_link(&self) -> Result<String, String> {
        let view = self.file.as_ref().ok_or("open a file first")?;
        let (first, last) = view.selection().unwrap_or((view.cursor, view.cursor));
        let vcs = &self.review.vcs;
        let remotes = vcs.remotes().unwrap_or_default();
        let pushed = self
            .head
            .upstream
            .as_deref()
            .filter(|_| self.head.ahead == 0)
            .and_then(|upstream| {
                remotes.iter().find_map(|remote| {
                    let branch = upstream.strip_prefix(remote.as_str())?.strip_prefix('/')?;
                    Some((remote.clone(), branch.to_owned()))
                })
            });
        let (remote, rev) = if let Some(found) = pushed {
            found
        } else {
            let remote = remotes
                .iter()
                .find(|name| *name == "origin")
                .or_else(|| remotes.first())
                .ok_or("add a git remote to link to")?
                .clone();
            let rev = vcs
                .default_branch(&remote)
                .ok()
                .flatten()
                .unwrap_or_else(|| "main".to_owned());
            (remote, rev)
        };
        let rev = view
            .shown_version()
            .map_or(rev, |version| version.entry.oid.clone());
        let url = vcs
            .remote_url(&remote)
            .ok()
            .flatten()
            .ok_or_else(|| format!("remote {remote} has no url"))?;
        crate::ci::detect_for_repo(&self.review.repo_root, Some(&url), &self.config.ci)
            .and_then(|detected| {
                crate::ci::blob_url(
                    &detected,
                    &url,
                    &rev,
                    &view.path,
                    (first as u32 + 1, last as u32 + 1),
                )
            })
            .ok_or_else(|| format!("no known forge serves {url}"))
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
    fn shift_y_copies_the_forge_link_to_the_selected_lines() {
        let fixture = standard_fixture();
        fixture.remote("origin", "git@github.com:acme/widgets.git");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.install_file(view("one\ntwo\nthree\n", Vec::new()), None);
        app.handle(key('j'));
        app.handle(key('V'));
        app.handle(key('j'));
        app.handle(key('Y'));
        assert_eq!(
            app.pending_clipboard.as_deref(),
            Some("https://github.com/acme/widgets/blob/main/src/lib.rs#L2-L3")
        );
        assert!(app.file.as_ref().is_some_and(|v| v.anchor().is_none()));
    }

    #[test]
    fn shift_y_without_a_remote_says_what_to_add() {
        let (_fixture, mut app) = app();
        app.install_file(view("one\n", Vec::new()), None);
        app.handle(key('Y'));
        assert_eq!(app.pending_clipboard, None);
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("add a git remote"))
        );
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
        assert_eq!(
            (request.path.clone(), request.span),
            (path, line.map(|l| (l, l)))
        );
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

        let flow = app.on_file_loaded(
            Ok(view("one\n", vec![span(1, 1, true)])),
            None,
            false,
            stale,
        );
        assert_eq!(flow, Flow::Idle, "a stale load draws nothing");
        assert!(app.file.is_none());
        assert_eq!(
            app.screen(),
            Screen::Status,
            "the abandoned file view never comes back"
        );
    }

    #[test]
    fn a_reload_keeps_the_readers_place_in_the_open_file() {
        let (_fixture, mut app) = app();
        app.open_file("a.txt", None, false);
        let opened = app.pending_file.as_ref().expect("a queued file").token;
        app.on_file_loaded(Ok(view("a\nb\nc\n", Vec::new())), None, false, opened);
        app.file.as_mut().expect("view").cursor = 2;
        let depth = app.screens.len();

        app.reload_file();
        let reload = app.pending_file.as_ref().expect("a queued reload").token;
        app.on_file_loaded(Ok(view("a\nb\nc\n", Vec::new())), None, true, reload);
        assert_eq!(app.file.as_ref().expect("view").cursor, 2);
        assert_eq!(app.screens.len(), depth, "a reload opens no screen");
    }

    #[test]
    fn the_newest_request_wins_when_two_loads_are_in_flight() {
        let (_fixture, mut app) = app();
        app.open_file("a.txt", None, false);
        let first = app.pending_file.as_ref().expect("a queued file").token;
        app.open_file("b.txt", None, false);
        let second = app.pending_file.as_ref().expect("a queued file").token;

        app.on_file_loaded(
            Ok(view("second\n", vec![span(1, 1, true)])),
            None,
            false,
            second,
        );
        app.on_file_loaded(
            Ok(view("first\n", vec![span(1, 1, true)])),
            None,
            false,
            first,
        );
        assert_eq!(
            app.file.as_ref().expect("view").lines,
            vec!["second"],
            "the earlier load cannot overwrite the later one"
        );
    }

    #[test]
    fn a_step_clamps_at_the_oldest_version_and_returns_to_the_working_tree() {
        assert_eq!(next_version(None, 3, true), Ok(Some(0)));
        assert_eq!(next_version(Some(0), 3, true), Ok(Some(1)));
        assert_eq!(next_version(Some(2), 3, true), Err(StepEnd::Oldest));
        assert_eq!(next_version(Some(2), 3, false), Ok(Some(1)));
        assert_eq!(
            next_version(Some(0), 3, false),
            Ok(None),
            "back to the worktree"
        );
        assert_eq!(next_version(None, 3, false), Err(StepEnd::Newest));
        assert_eq!(next_version(None, 0, true), Err(StepEnd::Uncommitted));
    }

    /// Three commits of `a.txt` and a fourth line still in the worktree.
    fn history_fixture() -> Fixture {
        let fixture = Fixture::new();
        fixture.write("a.txt", "one\n");
        fixture.commit_all("first");
        fixture.write("a.txt", "one\ntwo\n");
        fixture.commit_all("second");
        fixture.write("a.txt", "one\ntwo\nthree\n");
        fixture.commit_all("third");
        fixture.write("a.txt", "one\ntwo\nthree\nfour\n");
        fixture
    }

    /// Answer queued history and file reads the way the workers do.
    fn settle(app: &mut App, fixture: &Fixture) {
        for _ in 0..8 {
            if let Some(request) = app.pending_history.take() {
                let result =
                    Review::compute_file_history(&fixture.root, &request.path, HISTORY_LIMIT)
                        .map_err(|err| err.to_string());
                app.on_file_history(&request, result);
            } else if let Some(request) = app.pending_file.take() {
                let (span, reload, token) = (request.span, request.reload, request.token);
                let highlighter = std::sync::Arc::clone(&app.highlighter);
                let result = load_file(&fixture.root, &highlighter, request);
                app.on_file_loaded(result, span, reload, token);
            } else {
                return;
            }
        }
    }

    fn shown(app: &App) -> (Option<usize>, usize, usize) {
        let view = app.file.as_ref().expect("a file view");
        (view.version, view.lines.len(), view.cursor)
    }

    fn message(app: &App) -> String {
        app.message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn braces_step_through_versions_keeping_the_line_number() {
        let fixture = history_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file("a.txt", None, false);
        settle(&mut app, &fixture);
        app.handle(key('G'));
        assert_eq!(shown(&app), (None, 4, 3));

        app.handle(key('{'));
        settle(&mut app, &fixture);
        assert_eq!(shown(&app), (Some(0), 3, 2), "third, the line clamped");
        app.handle(key('{'));
        app.handle(key('{'));
        settle(&mut app, &fixture);
        assert_eq!(shown(&app), (Some(2), 1, 0), "the add commit");

        app.handle(key('{'));
        assert!(app.pending_file.is_none());
        assert_eq!(
            message(&app),
            "this is the oldest version; } to see a newer one"
        );

        for _ in 0..3 {
            app.handle(key('}'));
            settle(&mut app, &fixture);
        }
        assert_eq!(shown(&app), (None, 4, 0), "back on the working tree");
        app.handle(key('}'));
        assert!(app.pending_file.is_none());
        assert!(message(&app).contains("working-tree"));
    }

    #[test]
    fn a_failed_step_counts_the_next_one_from_the_version_on_screen() {
        let fixture = history_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file("a.txt", None, false);
        settle(&mut app, &fixture);
        app.handle(key('{'));
        settle(&mut app, &fixture);
        app.handle(key('{'));
        let failed = app.pending_file.take().expect("a queued step");
        app.on_file_loaded(Err("binary".to_owned()), None, true, failed.token);

        app.handle(key('{'));
        let retry = app.pending_file.as_ref().expect("a queued step");
        assert_eq!(retry.at.as_ref().map(|at| at.index), Some(1));
    }

    #[test]
    fn a_past_version_declines_the_editor_and_reloads_in_place() {
        let fixture = history_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file("a.txt", None, false);
        settle(&mut app, &fixture);
        app.handle(key('{'));
        settle(&mut app, &fixture);
        app.handle(key('{'));
        settle(&mut app, &fixture);
        app.handle(key('e'));
        assert!(app.pending_editor.is_none());
        assert!(message(&app).contains("working-tree"));

        app.reload_file();
        settle(&mut app, &fixture);
        assert_eq!(shown(&app), (Some(1), 2, 0), "the reload keeps the version");
    }

    #[test]
    fn shift_h_lists_the_history_and_b_opens_a_version_from_it() {
        let fixture = history_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file("a.txt", None, false);
        settle(&mut app, &fixture);
        app.handle(key('H'));
        settle(&mut app, &fixture);
        assert_eq!(app.screen(), Screen::Log);
        let log = app.log.as_ref().expect("a log");
        let subjects: Vec<&str> = log.entries.iter().map(|e| e.subject.as_str()).collect();
        assert_eq!(subjects, vec!["third", "second", "first"]);
        assert_eq!(log.file.as_ref().map(|f| f.path.as_str()), Some("a.txt"));

        app.handle(key('j'));
        app.handle(key('B'));
        settle(&mut app, &fixture);
        assert_eq!(app.screen(), Screen::File);
        assert_eq!(
            app.screens.len(),
            2,
            "the file screen below takes the version"
        );
        assert_eq!(shown(&app), (Some(1), 2, 0));
        assert!(app.file.as_ref().is_some_and(|v| v.show_blame));

        app.handle(key('{'));
        settle(&mut app, &fixture);
        assert_eq!(
            shown(&app).0,
            Some(2),
            "the version steps from the list's history"
        );

        app.handle(key('H'));
        settle(&mut app, &fixture);
        assert_eq!(app.screen(), Screen::Log);
        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::File);
        assert_eq!(shown(&app).0, Some(2), "the version stays under the list");
    }

    #[test]
    fn a_review_above_a_file_view_survives_opening_a_file_or_a_history() {
        let fixture = history_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file("a.txt", None, false);
        settle(&mut app, &fixture);
        app.open_working_tree_diff(None);
        let draft = crate::app::composer::Composer::new(
            crate::app::composer::ComposerKind::Reply {
                comment_id: "c1".to_owned(),
            },
            "half a thought".to_owned(),
        );
        app.diff.as_mut().expect("a diff").parked_drafts.push(draft);

        app.open_file("a.txt", None, false);
        settle(&mut app, &fixture);
        app.queue_history("a.txt", HistoryNext::List);
        settle(&mut app, &fixture);
        assert_eq!(
            app.screens,
            vec![
                Screen::Status,
                Screen::File,
                Screen::Diff,
                Screen::File,
                Screen::Log
            ]
        );

        app.handle(key('q'));
        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::Diff);
        let parked = app.diff.as_ref().map(|diff| diff.parked_drafts.len());
        assert_eq!(parked, Some(1), "the draft is still parked");
        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::File);
        assert!(app.file.is_some(), "the file screen below still has a view");
    }

    #[test]
    fn a_new_commit_reaches_the_working_tree_view_but_not_a_past_version() {
        let fixture = history_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file("a.txt", None, false);
        settle(&mut app, &fixture);
        app.handle(key('{'));
        settle(&mut app, &fixture);
        fixture.commit_all("fourth");
        app.queue_refresh();
        app.settle_refresh();
        settle(&mut app, &fixture);
        let versions = |app: &App| {
            app.file
                .as_ref()
                .and_then(|view| view.history.as_ref())
                .map(|history| history.versions.len())
        };
        assert_eq!(versions(&app), Some(3), "a past version keeps its history");

        app.handle(key('}'));
        settle(&mut app, &fixture);
        assert_eq!(shown(&app).0, None);
        app.queue_refresh();
        app.settle_refresh();
        settle(&mut app, &fixture);
        assert_eq!(versions(&app), None);
        app.handle(key('{'));
        settle(&mut app, &fixture);
        assert_eq!(
            versions(&app),
            Some(4),
            "the next step reads the new commit"
        );
        assert_eq!(shown(&app), (Some(0), 4, 0));
    }

    #[test]
    fn enter_in_a_file_history_opens_the_commit_on_that_file() {
        let fixture = history_fixture();
        fixture.write("b.txt", "other\n");
        fixture.commit_all("touch both");
        fixture.write("a.txt", "changed\n");
        fixture.commit_all("edit a");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file("b.txt", None, false);
        settle(&mut app, &fixture);
        app.handle(key('H'));
        settle(&mut app, &fixture);
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("a diff");
        assert_eq!(diff.selected_path(&app.review).as_deref(), Some("b.txt"));
    }

    #[test]
    fn l_f_lists_the_history_of_the_file_under_the_status_cursor() {
        let (_fixture, mut app) = app();
        assert!(app.status_cursor_file_line().is_none());
        app.handle(key('l'));
        app.handle(key('f'));
        assert!(app.pending_history.is_none());
        assert_eq!(message(&app), "no file under the cursor");

        seek(&mut app, |app| app.status_cursor_file_line().is_some());
        let expected = app.status_cursor_file_line().expect("a file row").0;
        app.handle(key('l'));
        app.handle(key('f'));
        let request = app.pending_history.as_ref().expect("a history read");
        assert_eq!(request.path, expected);
        assert_eq!(request.then, HistoryNext::List);
    }

    #[test]
    fn a_file_with_no_commits_lists_nothing() {
        let (_fixture, mut app) = app();
        app.queue_history("todo.md", HistoryNext::List);
        let request = app.pending_history.take().expect("a history read");
        app.on_file_history(&request, Ok(Vec::new()));
        assert_eq!(app.screen(), Screen::Status);
        assert_eq!(message(&app), "commit todo.md to give it a history");
    }

    #[test]
    fn opening_a_file_at_a_line_seats_the_cursor_there_and_clamps() {
        let (_fixture, mut app) = app();
        app.install_file(view("one\ntwo\n", vec![span(1, 2, true)]), Some((2, 2)));
        assert_eq!(app.file.as_ref().expect("view").cursor, 1);
        app.install_file(view("one\ntwo\n", vec![span(1, 2, true)]), Some((99, 99)));
        assert_eq!(
            app.file.as_ref().expect("view").cursor,
            1,
            "a line past the end lands on the last one"
        );
    }
}
