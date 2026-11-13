//! The CI job-log view: a job's log grouped into its real steps. Each line is
//! `<timestamp> <text>` (the REST log; the parser also tolerates `gh`'s
//! `<job>\t<step>\t<timestamp>` form). No API exposes per-step log content, so
//! lines are bucketed into the step metadata (name/status/timing from the jobs
//! API) by timestamp: a line belongs to the last step whose start it's at or
//! after. Without step metadata (e.g. GitLab) we group by the runner's
//! `##[group]` markers.

use ratatui::layout::Rect;

use super::rowsel::{RowSelect, RowText};
use super::{App, CiRequest, MouseGesture, Screen, hit_index, page_step};
use crate::ci::{JobStatus, LogStepMeta, ts_sort_key};
use crate::keymap::Action;

/// `name` is empty (and `status` `None`) for the leading section of pre-step
/// output.
pub struct CiLogStep {
    pub name: String,
    pub status: Option<JobStatus>,
    pub duration_secs: Option<i64>,
    pub lines: Vec<String>,
    /// One per line of `lines`.
    pub kinds: Vec<LogLineKind>,
    pub folded: bool,
}

impl CiLogStep {
    fn new(
        name: String,
        status: Option<JobStatus>,
        duration_secs: Option<i64>,
        lines: Vec<String>,
    ) -> Self {
        Self {
            name,
            status,
            duration_secs,
            lines,
            kinds: Vec::new(),
            folded: true,
        }
    }
}

/// What a log line is to its reader, from the runner's `##[...]` workflow
/// commands and the usual failure words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLineKind {
    Plain,
    /// What the runner prints inside a `##[group]` (the command, its shell and
    /// environment) and its debug output.
    Muted,
    GroupEdge,
    Command,
    Error,
    Warning,
    Notice,
}

impl LogLineKind {
    fn classify(lines: &[String]) -> Vec<Self> {
        let mut in_group = false;
        lines
            .iter()
            .map(|line| {
                let kind = Self::of(line, in_group);
                if kind == Self::GroupEdge {
                    in_group = line.starts_with("##[group]");
                }
                kind
            })
            .collect()
    }

    fn of(line: &str, in_group: bool) -> Self {
        if let Some((marker, _)) = workflow_command(line) {
            return match marker {
                "group" | "endgroup" => Self::GroupEdge,
                "command" => Self::Command,
                "error" => Self::Error,
                "warning" => Self::Warning,
                "notice" => Self::Notice,
                _ => Self::Muted,
            };
        }
        if in_group {
            return Self::Muted;
        }
        let trimmed = line.trim_start();
        let failed = ["error:", "error[", "ERROR", "FAILED", "FAIL ", "Traceback"]
            .iter()
            .any(|word| trimmed.starts_with(word))
            || trimmed.contains("panicked at");
        if failed { Self::Error } else { Self::Plain }
    }
}

/// The marker and the text after it, for a line like `##[error]boom`. GitHub
/// prints the commands its own actions run as `[command]git version`.
pub fn workflow_command(line: &str) -> Option<(&str, &str)> {
    if let Some(rest) = line.strip_prefix("[command]") {
        return Some(("command", rest));
    }
    line.strip_prefix("##[")?.split_once(']')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiLogRow {
    Step(usize),
    Line { step: usize, line: usize },
}

pub struct CiLogView {
    pub steps: Vec<CiLogStep>,
    pub cursor: usize,
    pub scroll: usize,
    pub visual_anchor: Option<usize>,
    pub viewport: u16,
    pub body: Rect,
}

impl CiLogView {
    pub fn parse(raw: &str, metas: &[LogStepMeta]) -> Self {
        let mut steps = if metas.is_empty() {
            sections_by_group(raw)
        } else {
            sections_by_step(raw, metas)
        };
        for step in &mut steps {
            step.kinds = LogLineKind::classify(&step.lines);
        }
        Self {
            steps,
            cursor: 0,
            scroll: 0,
            visual_anchor: None,
            viewport: 0,
            body: Rect::default(),
        }
    }

    /// Carry the prior view's position and per-step folds onto a re-parsed
    /// view, so a re-poll that appends lines keeps where the reader was.
    #[must_use]
    pub fn carry_into(self, mut next: CiLogView) -> CiLogView {
        for step in &mut next.steps {
            // the leading section and an unlabeled `##[group]` share the empty
            // name, so we never match on it
            if step.name.is_empty() {
                continue;
            }
            if let Some(prev) = self.steps.iter().find(|p| p.name == step.name) {
                step.folded = prev.folded;
            }
        }
        let last = next.rows().len().saturating_sub(1);
        next.cursor = self.cursor.min(last);
        next.scroll = self.scroll;
        next.viewport = self.viewport;
        next.body = self.body;
        next.visual_anchor = self.visual_anchor.map(|a| a.min(last));
        next
    }

    pub fn rows(&self) -> Vec<CiLogRow> {
        let mut rows = Vec::new();
        for (s, step) in self.steps.iter().enumerate() {
            rows.push(CiLogRow::Step(s));
            if !step.folded {
                rows.extend((0..step.lines.len()).map(|line| CiLogRow::Line { step: s, line }));
            }
        }
        rows
    }

    pub fn line_text(&self, row: CiLogRow) -> &str {
        match row {
            CiLogRow::Step(s) => self.steps.get(s).map_or("", |st| st.name.as_str()),
            CiLogRow::Line { step, line } => self
                .steps
                .get(step)
                .and_then(|st| st.lines.get(line))
                .map_or("", String::as_str),
        }
    }

    pub fn toggle_fold_at_cursor(&mut self) {
        let rows = self.rows();
        let Some(step) = rows.get(self.cursor).map(|row| match row {
            CiLogRow::Step(s) | CiLogRow::Line { step: s, .. } => *s,
        }) else {
            return;
        };
        if let Some(st) = self.steps.get_mut(step) {
            st.folded = !st.folded;
        }
        let last = self.rows().len().saturating_sub(1);
        if let Some(pos) = self.rows().iter().position(|r| *r == CiLogRow::Step(step)) {
            self.cursor = pos;
        }
        // a collapse can drop the rows an anchor sat on
        self.clamp_anchor(last);
    }
}

impl RowSelect for CiLogView {
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

impl RowText for CiLogView {
    fn row_count(&self) -> usize {
        self.rows().len()
    }

    fn row_text(&self, row: usize) -> String {
        self.rows()
            .get(row)
            .map(|row| self.line_text(*row).to_owned())
            .unwrap_or_default()
    }
}

impl App {
    /// Open a job's log view. A provider with `LogMode::None` (Forgejo) would
    /// fail every poll, so we decline there.
    pub(super) fn open_ci_log(&mut self, job: crate::ci::JobId) {
        let Some(run) = self.open_run.clone() else {
            return;
        };
        let has_logs = self.ci_remote_for_open_run().is_none_or(|remote| {
            crate::ci::capabilities_for(remote.detected.kind).logs != crate::ci::LogMode::None
        });
        if !has_logs {
            self.info("this CI provider doesn't expose job logs");
            return;
        }
        self.open_job = Some(job.clone());
        self.log_text.clear();
        self.log_offset = 0;
        self.log_steps.clear();
        self.ci_log = None;
        self.log_done = false;
        self.push_screen(Screen::CiLog);
        self.pending_ci = Some(CiRequest::Log {
            run,
            job,
            offset: 0,
        });
    }

    /// Opens job `lint` of run `1` and hands back the answer `text` would get.
    #[cfg(test)]
    pub(crate) fn test_log_event(
        &mut self,
        text: &str,
        steps: Vec<crate::ci::LogStepMeta>,
    ) -> crate::event::AppEvent {
        let (run, job) = (
            crate::ci::RunId("1".into()),
            crate::ci::JobId("lint".into()),
        );
        self.open_run = Some(run.clone());
        self.open_job = Some(job.clone());
        crate::event::AppEvent::CiLog {
            run,
            job,
            from: self.log_offset,
            chunk: crate::ci::LogChunk {
                text: text.to_owned(),
                steps,
                next_offset: self.log_offset + text.len() as u64,
                done: true,
            },
        }
    }

    /// Requests run concurrently, so we drop an answer for a job that is no
    /// longer open or from an offset the log has already moved past.
    pub(super) fn on_ci_log(
        &mut self,
        run: &crate::ci::RunId,
        job: &crate::ci::JobId,
        from: u64,
        chunk: crate::ci::LogChunk,
    ) {
        let current = self.open_run.as_ref() == Some(run) && self.open_job.as_ref() == Some(job);
        if !current || from != self.log_offset {
            return;
        }
        self.log_text.push_str(&chunk.text);
        self.log_offset = chunk.next_offset;
        self.log_done = chunk.done;
        if !chunk.steps.is_empty() {
            self.log_steps = chunk.steps;
        }
        let rebuilt = CiLogView::parse(&self.log_text, &self.log_steps);
        self.ci_log = Some(match self.ci_log.take() {
            Some(prev) => prev.carry_into(rebuilt),
            None => rebuilt,
        });
    }

    pub(super) fn ci_log_mouse(&mut self, gesture: MouseGesture) {
        let Some(view) = self.ci_log.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        match gesture {
            MouseGesture::Scroll { down, .. } => {
                let delta = if down { 3 } else { -3 };
                view.cursor = view.cursor.saturating_add_signed(delta).min(last);
            }
            MouseGesture::Press { col, row } | MouseGesture::Select { col, row } => {
                if let Some(i) = hit_index(view.body, view.scroll, col, row).filter(|i| *i <= last)
                {
                    view.cursor = i;
                    view.visual_anchor = None;
                }
            }
            MouseGesture::DoublePress { col, row } => {
                if let Some(i) = hit_index(view.body, view.scroll, col, row).filter(|i| *i <= last)
                {
                    view.cursor = i;
                    view.toggle_fold_at_cursor();
                }
            }
            MouseGesture::Drag { col, row } => {
                if let Some(i) = hit_index(view.body, view.scroll, col, row).filter(|i| *i <= last)
                {
                    if view.visual_anchor.is_none() {
                        view.visual_anchor = Some(view.cursor);
                    }
                    view.cursor = i;
                }
            }
        }
    }

    pub(super) fn dispatch_ci_log(&mut self, action: Action) {
        let Some(view) = self.ci_log.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        match action {
            Action::MoveDown => view.cursor = (view.cursor + 1).min(last),
            Action::MoveUp => view.cursor = view.cursor.saturating_sub(1),
            Action::GoTop => view.cursor = 0,
            Action::GoBottom => view.cursor = last,
            Action::HalfPageDown => self.ci_log_page(false, false),
            Action::HalfPageUp => self.ci_log_page(true, false),
            Action::FullPageDown => self.ci_log_page(false, true),
            Action::FullPageUp => self.ci_log_page(true, true),
            Action::ToggleFold => view.toggle_fold_at_cursor(),
            Action::VisualSelect => view.toggle_visual(),
            Action::Yank | Action::CopyFileFeedback | Action::CopyAllFeedback => {
                self.yank_rows("yanked log selection");
            }
            _ => {}
        }
    }

    pub(super) fn ci_log_page(&mut self, up: bool, full: bool) {
        let Some(view) = self.ci_log.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        let step = page_step(view.viewport, full);
        view.cursor = if up {
            view.cursor.saturating_sub(step)
        } else {
            (view.cursor + step).min(last)
        };
    }
}

/// Step times have whole-second precision, so we switch steps on a `##[group]`
/// line naming one, and move on by time only once the current step has ended.
fn sections_by_step(raw: &str, metas: &[LogStepMeta]) -> Vec<CiLogStep> {
    let mut leading: Vec<String> = Vec::new();
    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); metas.len()];
    let mut current: Option<usize> = None;
    for raw_line in raw.lines() {
        let (key, content) = line_key_and_content(raw_line);
        let named = content.strip_prefix("##[group]").and_then(|name| {
            metas
                .iter()
                .enumerate()
                .skip(current.unwrap_or(0))
                .find(|(_, m)| m.start_key > 0 && m.name == name.trim())
                .map(|(i, _)| i)
        });
        let ended = |i: usize| {
            metas
                .get(i)
                .is_some_and(|m| key > m.end_key.max(m.start_key))
        };
        current = match (named, current) {
            (Some(i), _) => Some(i),
            (None, None) => step_running_at(metas, key, None),
            (None, Some(c)) if ended(c) => step_running_at(metas, key, Some(c)).or(Some(c)),
            (None, c) => c,
        };
        match current.and_then(|i| buckets.get_mut(i)) {
            Some(bucket) => bucket.push(content),
            None => leading.push(content),
        }
    }
    let mut steps = Vec::new();
    if !leading.is_empty() {
        steps.push(CiLogStep::new(String::new(), None, None, leading));
    }
    for (meta, lines) in metas.iter().zip(buckets) {
        steps.push(CiLogStep::new(
            meta.name.clone(),
            Some(meta.status),
            meta.duration_secs,
            lines,
        ));
    }
    steps
}

/// The first step after `after` that started by `key` and was still running
/// then, else the last one that started. A skipped step has start key 0 and
/// claims nothing.
fn step_running_at(metas: &[LogStepMeta], key: u64, after: Option<usize>) -> Option<usize> {
    let started: Vec<(usize, &LogStepMeta)> = metas
        .iter()
        .enumerate()
        .filter(|(i, m)| m.start_key > 0 && m.start_key <= key && after.is_none_or(|c| *i > c))
        .collect();
    started
        .iter()
        .find(|(_, m)| m.end_key == 0 || m.end_key >= key)
        .or(started.last())
        .map(|(i, _)| *i)
}

fn sections_by_group(raw: &str) -> Vec<CiLogStep> {
    let mut steps: Vec<CiLogStep> = Vec::new();
    let mut leading: Vec<String> = Vec::new();
    for raw_line in raw.lines() {
        let (_, content) = line_key_and_content(raw_line);
        if let Some(name) = content.strip_prefix("##[group]") {
            steps.push(CiLogStep::new(
                name.trim().to_owned(),
                None,
                None,
                Vec::new(),
            ));
            continue;
        }
        if content.trim_end() == "##[endgroup]" {
            continue;
        }
        match steps.last_mut() {
            Some(step) => step.lines.push(content),
            None => leading.push(content),
        }
    }
    if !leading.is_empty() {
        steps.insert(0, CiLogStep::new(String::new(), None, None, leading));
    }
    steps
}

/// Split a `<job>\t<step>\t<timestamp> <text>` line into its timestamp sort
/// key and display text. A line with no timestamp keys to 0.
fn line_key_and_content(line: &str) -> (u64, String) {
    let field = line.splitn(3, '\t').nth(2).unwrap_or(line);
    match field.split_once(' ') {
        Some((head, rest))
            if head.ends_with('Z')
                && head.len() >= 20
                && head.starts_with(|c: char| c.is_ascii_digit()) =>
        {
            (ts_sort_key(head), strip_ansi(rest))
        }
        _ => (0, strip_ansi(field)),
    }
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CiRemote;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    #[test]
    fn open_ci_log_declines_when_the_provider_has_no_logs() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.ci_remotes = vec![CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::Forgejo,
                host: None,
            },
            url: None,
        }];
        app.open_run = Some(crate::ci::RunId("1".into()));
        app.open_run_remote = Some("origin".into());
        app.open_ci_log(crate::ci::JobId("lint".into()));
        assert_ne!(
            app.screen(),
            Screen::CiLog,
            "no log screen for a provider with no log support"
        );
        let message = app.message.expect("info message");
        assert!(message.text.contains("job logs"), "{}", message.text);
    }

    #[test]
    fn open_ci_log_opens_for_a_provider_with_logs() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.ci_remotes = vec![CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }];
        app.open_run = Some(crate::ci::RunId("1".into()));
        app.open_run_remote = Some("origin".into());
        app.open_ci_log(crate::ci::JobId("lint".into()));
        assert_eq!(app.screen(), Screen::CiLog);
        assert!(matches!(app.pending_ci, Some(CiRequest::Log { .. })));
    }

    // mirrors the real `gh --log` shape: a `<job>\t<step>\t<ts>` prefix (the step
    // column is the literal junk `gh` emits), `##[group]`/`##[endgroup]` markers,
    // ANSI, and a line that precedes the first group
    const RAW: &str = "lint\tUNKNOWN STEP\t2026-06-20T00:00:00Z runner v2.335.1\n\
                       lint\tUNKNOWN STEP\t2026-06-20T00:00:01Z ##[group]Build\n\
                       lint\tUNKNOWN STEP\t2026-06-20T00:00:02Z compiling…\n\
                       lint\tUNKNOWN STEP\t2026-06-20T00:00:03Z \u{1b}[32mok\u{1b}[0m\n\
                       lint\tUNKNOWN STEP\t2026-06-20T00:00:04Z ##[endgroup]\n\
                       lint\tUNKNOWN STEP\t2026-06-20T00:00:05Z ##[group]Test\n\
                       lint\tUNKNOWN STEP\t2026-06-20T00:00:06Z running\n";

    #[test]
    fn parse_sections_by_group_strips_prefix_and_ansi() {
        let view = CiLogView::parse(RAW, &[]);
        assert_eq!(view.steps.len(), 3);
        assert_eq!(view.steps[0].name, "", "pre-group lines lead unnamed");
        assert_eq!(view.steps[0].lines, vec!["runner v2.335.1"]);
        assert_eq!(view.steps[1].name, "Build");
        assert_eq!(view.steps[1].lines, vec!["compiling…", "ok"]);
        assert_eq!(view.steps[2].name, "Test");
        assert_eq!(view.steps[2].lines, vec!["running"]);
        assert!(view.steps.iter().all(|s| s.folded));
    }

    #[test]
    fn folded_view_shows_only_headers() {
        let view = CiLogView::parse(RAW, &[]);
        assert_eq!(
            view.rows(),
            vec![CiLogRow::Step(0), CiLogRow::Step(1), CiLogRow::Step(2)]
        );
    }

    #[test]
    fn toggle_fold_reveals_lines_and_reseats_cursor() {
        let mut view = CiLogView::parse(RAW, &[]);
        view.cursor = 1; // the Build section header
        view.toggle_fold_at_cursor();
        assert_eq!(
            view.rows(),
            vec![
                CiLogRow::Step(0),
                CiLogRow::Step(1),
                CiLogRow::Line { step: 1, line: 0 },
                CiLogRow::Line { step: 1, line: 1 },
                CiLogRow::Step(2),
            ]
        );
        view.cursor = 3; // a Build line
        view.toggle_fold_at_cursor();
        assert_eq!(
            view.cursor, 1,
            "cursor re-seats on the folded section header"
        );
    }

    #[test]
    fn selection_text_joins_the_visual_range() {
        let mut view = CiLogView::parse(RAW, &[]);
        view.cursor = 1;
        view.toggle_fold_at_cursor();
        view.visual_anchor = Some(2);
        view.cursor = 3;
        assert_eq!(view.selection_text(), "compiling…\nok");
    }

    #[test]
    fn folding_clamps_a_stale_visual_anchor() {
        let mut view = CiLogView::parse(RAW, &[]);
        view.cursor = 1;
        view.toggle_fold_at_cursor();
        view.visual_anchor = Some(3);
        view.cursor = 3;
        view.toggle_fold_at_cursor();
        let last = view.rows().len() - 1;
        assert!(view.visual_anchor.is_some_and(|a| a <= last));
    }

    #[test]
    fn carry_into_preserves_fold_state_by_name() {
        let mut prev = CiLogView::parse(RAW, &[]);
        prev.cursor = 1;
        prev.toggle_fold_at_cursor();
        let next = prev.carry_into(CiLogView::parse(RAW, &[]));
        assert!(!next.steps[1].folded, "Build stays unfolded across re-poll");
        assert!(next.steps[2].folded);
    }

    #[test]
    fn step_metadata_buckets_lines_by_timestamp() {
        let metas = vec![
            LogStepMeta {
                name: "Set up job".into(),
                status: JobStatus::Ok,
                start_key: ts_sort_key("2026-06-20T00:00:00Z"),
                end_key: 0,
                duration_secs: Some(1),
            },
            LogStepMeta {
                name: "Run build".into(),
                status: JobStatus::Failed,
                start_key: ts_sort_key("2026-06-20T00:00:05Z"),
                end_key: 0,
                duration_secs: Some(13),
            },
        ];
        // the `##[group]` markers are ignored when real steps drive the grouping
        let view = CiLogView::parse(RAW, &metas);
        assert_eq!(view.steps.len(), 2, "one section per real step, no leading");
        assert_eq!(view.steps[0].name, "Set up job");
        assert_eq!(view.steps[0].status, Some(JobStatus::Ok));
        // lines ts 00→04 fall in step 1; ts 05+ in step 2
        assert!(view.steps[0].lines.iter().any(|l| l == "runner v2.335.1"));
        assert!(view.steps[0].lines.iter().any(|l| l == "compiling…"));
        assert_eq!(view.steps[1].name, "Run build");
        assert_eq!(view.steps[1].duration_secs, Some(13));
        assert!(view.steps[1].lines.iter().any(|l| l == "running"));
    }

    #[test]
    fn steps_starting_in_the_same_second_split_on_their_group_line() {
        let meta = |name: &str| LogStepMeta {
            name: name.into(),
            status: JobStatus::Ok,
            start_key: ts_sort_key("2026-06-20T00:00:19Z"),
            end_key: 0,
            duration_secs: Some(0),
        };
        let metas = vec![
            meta("Run setup"),
            meta("Run python -m unittest"),
            meta("Complete job"),
        ];
        let raw = "2026-06-20T00:00:19.1Z ##[group]Run setup\n\
                   2026-06-20T00:00:19.2Z installed\n\
                   2026-06-20T00:00:19.3Z ##[group]Run python -m unittest\n\
                   2026-06-20T00:00:19.4Z Traceback (most recent call last):\n\
                   2026-06-20T00:00:19.5Z FAILED (errors=1)\n";
        let view = CiLogView::parse(raw, &metas);
        assert_eq!(view.steps[0].lines.len(), 2, "{:?}", view.steps[0].lines);
        assert!(view.steps[1].lines.iter().any(|l| l == "FAILED (errors=1)"));
        assert!(view.steps[2].lines.is_empty());
    }

    #[test]
    fn a_log_answer_counts_once_and_only_for_the_open_job() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let text = "2026-06-20T00:00:01Z ##[group]Build\n2026-06-20T00:00:02Z compiling\n";
        let first = app.test_log_event(text, Vec::new());
        let repeat = app.test_log_event(text, Vec::new());
        app.handle(first);
        app.handle(repeat);
        let stale = app.test_log_event(text, Vec::new());
        app.open_job = Some(crate::ci::JobId("test".into()));
        app.handle(stale);
        assert_eq!(
            app.log_text, text,
            "a repeat and another job's answer add nothing"
        );
    }

    #[test]
    fn a_step_finished_within_its_second_hands_over_to_the_one_still_running() {
        let meta = |name: &str, start: &str, end: &str| LogStepMeta {
            name: name.into(),
            status: JobStatus::Ok,
            start_key: ts_sort_key(start),
            end_key: ts_sort_key(end),
            duration_secs: Some(0),
        };
        let metas = vec![
            meta("Checkout", "2026-06-20T00:00:10Z", "2026-06-20T00:00:12Z"),
            meta("Setup", "2026-06-20T00:00:12Z", "2026-06-20T00:00:12Z"),
            meta("Test", "2026-06-20T00:00:12Z", "2026-06-20T00:00:60Z"),
        ];
        let raw = "2026-06-20T00:00:11Z cloning\n\
                   2026-06-20T00:00:13Z running tests\n\
                   2026-06-20T00:00:20Z FAILED\n";
        let view = CiLogView::parse(raw, &metas);
        assert_eq!(view.steps[0].lines, ["cloning"]);
        assert!(view.steps[1].lines.is_empty());
        assert_eq!(view.steps[2].lines, ["running tests", "FAILED"]);
    }

    #[test]
    fn ci_log_mouse_press_selects_the_row_and_drops_the_anchor() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let mut view = CiLogView::parse(RAW, &[]);
        view.body = ratatui::layout::Rect::new(0, 0, 40, 10);
        view.visual_anchor = Some(0);
        app.ci_log = Some(view);

        app.ci_log_mouse(MouseGesture::Press { col: 1, row: 2 });
        let view = app.ci_log.as_ref().expect("view");
        assert_eq!(view.cursor, 2);
        assert!(
            view.visual_anchor.is_none(),
            "a plain click drops any selection"
        );
    }

    #[test]
    fn ci_log_mouse_drag_extends_the_visual_selection() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let mut view = CiLogView::parse(RAW, &[]);
        view.body = ratatui::layout::Rect::new(0, 0, 40, 10);
        app.ci_log = Some(view);

        app.ci_log_mouse(MouseGesture::Drag { col: 0, row: 2 });
        let view = app.ci_log.as_ref().expect("view");
        assert_eq!(
            view.visual_anchor,
            Some(0),
            "drag anchors at the prior cursor"
        );
        assert_eq!(view.cursor, 2);
    }

    #[test]
    fn ci_log_page_moves_the_cursor_by_the_viewport_and_clamps() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let mut view = CiLogView::parse(RAW, &[]);
        // unfold every step so there are enough rows to page through
        for step in &mut view.steps {
            step.folded = false;
        }
        view.viewport = 4; // half-page = 2, full-page = 3
        app.ci_log = Some(view);

        app.ci_log_page(false, false); // half down
        assert_eq!(app.ci_log.as_ref().unwrap().cursor, 2);
        app.ci_log_page(false, true); // full down
        assert_eq!(app.ci_log.as_ref().unwrap().cursor, 5);
        app.ci_log_page(false, true); // clamps at the last row
        assert_eq!(app.ci_log.as_ref().unwrap().cursor, 6);
        app.ci_log_page(true, false); // half up
        assert_eq!(app.ci_log.as_ref().unwrap().cursor, 4);
        app.ci_log_page(true, true); // full up
        app.ci_log_page(true, true); // clamps at the top
        assert_eq!(app.ci_log.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn a_skipped_step_mid_list_claims_no_lines() {
        // a skipped step (start_key 0) sits between two real steps; its zero key
        // must not swallow the first step's output
        let metas = vec![
            LogStepMeta {
                name: "Set up job".into(),
                status: JobStatus::Ok,
                start_key: ts_sort_key("2026-06-20T00:00:00Z"),
                end_key: 0,
                duration_secs: Some(1),
            },
            LogStepMeta {
                name: "Cleanup (skipped)".into(),
                status: JobStatus::Skipped,
                start_key: 0,
                end_key: 0,
                duration_secs: None,
            },
            LogStepMeta {
                name: "Run build".into(),
                status: JobStatus::Ok,
                start_key: ts_sort_key("2026-06-20T00:00:05Z"),
                end_key: 0,
                duration_secs: Some(13),
            },
        ];
        let view = CiLogView::parse(RAW, &metas);
        assert!(
            view.steps[0].lines.iter().any(|l| l == "compiling…"),
            "early lines stay with the first real step, not the skipped one"
        );
        assert!(
            view.steps[1].lines.is_empty(),
            "skipped step claims nothing"
        );
        assert!(view.steps[2].lines.iter().any(|l| l == "running"));
    }
}
