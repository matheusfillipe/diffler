//! The CI job-log view: a job's log grouped into its real steps. Each line is
//! `<timestamp> <text>` (the REST log; the parser also tolerates `gh`'s
//! `<job>\t<step>\t<timestamp>` form). No API exposes per-step log content, so
//! lines are bucketed into the step metadata (name/status/timing from the jobs
//! API) by timestamp: a line belongs to the last step whose start it's at or
//! after. Without step metadata (e.g. GitLab) it falls back to the runner's
//! `##[group]` markers. Folded by default; keymap-driven like the diff.

use ratatui::layout::Rect;

use super::{App, CiRequest, MouseGesture, Screen, hit_index, page_step};
use crate::ci::{JobStatus, LogStepMeta, ts_sort_key};
use crate::keymap::Action;

/// One collapsible step: its name, status, run time, and log lines. `name` is
/// empty (and `status` `None`) for the leading section of pre-step output.
pub struct CiLogStep {
    pub name: String,
    pub status: Option<JobStatus>,
    pub duration_secs: Option<i64>,
    pub lines: Vec<String>,
    pub folded: bool,
}

/// A cursor-addressable row of the log view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiLogRow {
    Step(usize),
    Line { step: usize, line: usize },
}

/// State for the CI logs screen.
pub struct CiLogView {
    pub steps: Vec<CiLogStep>,
    pub cursor: usize,
    pub scroll: usize,
    pub visual_anchor: Option<usize>,
    pub viewport: u16,
    pub body: Rect,
}

impl CiLogView {
    /// Group a job's log into folded steps. With step metadata, lines are
    /// bucketed by timestamp into the real steps; without it, sections come from
    /// `##[group]` markers. Folded by default.
    pub fn parse(raw: &str, metas: &[LogStepMeta]) -> Self {
        let steps = if metas.is_empty() {
            sections_by_group(raw)
        } else {
            sections_by_step(raw, metas)
        };
        Self {
            steps,
            cursor: 0,
            scroll: 0,
            visual_anchor: None,
            viewport: 0,
            body: Rect::default(),
        }
    }

    /// Carry the prior view's cursor, scroll, viewport, visual selection, and
    /// per-step fold state (matched by step name) onto a freshly-parsed view, so
    /// a re-poll that appends lines doesn't reset what the user folded or where
    /// they are. New steps keep the default folded state.
    #[must_use]
    pub fn carry_into(self, mut next: CiLogView) -> CiLogView {
        for step in &mut next.steps {
            // match by name, but never by the empty name — the leading section
            // and an unlabeled `##[group]` would otherwise share fold state
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

    /// Flattened cursor-addressable rows given the current fold state.
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

    /// Display text of a row (the step name, or a log line), for search/render.
    pub fn row_text(&self, row: CiLogRow) -> &str {
        match row {
            CiLogRow::Step(s) => self.steps.get(s).map_or("", |st| st.name.as_str()),
            CiLogRow::Line { step, line } => self
                .steps
                .get(step)
                .and_then(|st| st.lines.get(line))
                .map_or("", String::as_str),
        }
    }

    /// Toggle the step the cursor is on (a header, or a line under a step), and
    /// re-seat the cursor on that step's header.
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
        // a collapse can drop rows out from under an anchor set in the now-folded
        // step; keep the selection inside the new row range
        self.visual_anchor = self.visual_anchor.map(|a| a.min(last));
    }

    /// The inclusive visual selection `(lo, hi)` over rows, if anchored.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.visual_anchor
            .map(|a| (a.min(self.cursor), a.max(self.cursor)))
    }

    /// The selected line range as plain text, for yank.
    pub fn selection_text(&self) -> String {
        let rows = self.rows();
        let (lo, hi) = match self.visual_anchor {
            Some(a) => (a.min(self.cursor), a.max(self.cursor)),
            None => (self.cursor, self.cursor),
        };
        rows.iter()
            .skip(lo)
            .take(hi - lo + 1)
            .map(|row| self.row_text(*row))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl App {
    /// Open a job's log view from a graph node activation. Declines instead of
    /// opening onto a screen that can only ever show "waiting for logs…": a
    /// provider with `LogMode::None` (e.g. Forgejo, whose job-log endpoint
    /// isn't wired up) would otherwise error on every poll forever.
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

    /// Append a job-log chunk, refresh the step metadata, and rebuild the
    /// foldable view, carrying the prior fold state across the re-poll.
    pub(super) fn on_ci_log(
        &mut self,
        text: &str,
        steps: Vec<crate::ci::LogStepMeta>,
        offset: u64,
        done: bool,
    ) {
        self.log_text.push_str(text);
        self.log_offset = offset;
        self.log_done = done;
        if !steps.is_empty() {
            self.log_steps = steps;
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
            MouseGesture::Press { col, row } => {
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
            MouseGesture::Cancel => view.visual_anchor = None,
        }
    }

    /// Drive the foldable CI-log view from a keymap [`Action`]: motions, fold,
    /// visual select, and yank. The `CiLog` screen reuses the diff/log keymap.
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
            Action::VisualSelect => {
                view.visual_anchor = match view.visual_anchor {
                    Some(_) => None,
                    None => Some(view.cursor),
                };
            }
            Action::CopyFileFeedback | Action::CopyAllFeedback => {
                self.pending_clipboard = Some(view.selection_text());
                let view = self.ci_log.as_mut();
                if let Some(view) = view {
                    view.visual_anchor = None;
                }
                self.info("yanked log selection");
            }
            _ => {}
        }
    }

    /// Half/full-page cursor jump over the CI-log view, mirroring `log_page`.
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

/// Bucket lines into the job's real steps by timestamp: a line joins the last
/// step whose start it's at or after; earlier lines form a leading section.
fn sections_by_step(raw: &str, metas: &[LogStepMeta]) -> Vec<CiLogStep> {
    let mut leading: Vec<String> = Vec::new();
    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); metas.len()];
    for raw_line in raw.lines() {
        let (key, content) = line_key_and_content(raw_line);
        // a line joins the last step that *ran* (key > 0) at or before it; skipped
        // steps (key 0) claim nothing, and started steps are in ascending order
        match metas
            .iter()
            .enumerate()
            .filter(|(_, m)| m.start_key > 0 && m.start_key <= key)
            .map(|(i, _)| i)
            .next_back()
        {
            Some(i) => {
                if let Some(bucket) = buckets.get_mut(i) {
                    bucket.push(content);
                }
            }
            None => leading.push(content),
        }
    }
    let mut steps = Vec::new();
    if !leading.is_empty() {
        steps.push(CiLogStep {
            name: String::new(),
            status: None,
            duration_secs: None,
            lines: leading,
            folded: true,
        });
    }
    for (meta, lines) in metas.iter().zip(buckets) {
        steps.push(CiLogStep {
            name: meta.name.clone(),
            status: Some(meta.status),
            duration_secs: meta.duration_secs,
            lines,
            folded: true,
        });
    }
    steps
}

/// Fallback grouping by the runner's `##[group]`/`##[endgroup]` markers, for
/// providers that don't expose step metadata.
fn sections_by_group(raw: &str) -> Vec<CiLogStep> {
    let mut steps: Vec<CiLogStep> = Vec::new();
    let mut leading: Vec<String> = Vec::new();
    for raw_line in raw.lines() {
        let (_, content) = line_key_and_content(raw_line);
        if let Some(name) = content.strip_prefix("##[group]") {
            steps.push(CiLogStep {
                name: name.trim().to_owned(),
                status: None,
                duration_secs: None,
                lines: Vec::new(),
                folded: true,
            });
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
        steps.insert(
            0,
            CiLogStep {
                name: String::new(),
                status: None,
                duration_secs: None,
                lines: leading,
                folded: true,
            },
        );
    }
    steps
}

/// A `gh --log` line `<job>\t<step>\t<timestamp> <text>` split into its
/// timestamp sort key (for step bucketing) and display text (prefix, timestamp,
/// and ANSI removed). A line without the tab/timestamp structure keys to 0.
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

/// Remove ANSI CSI escape sequences (colors, cursor moves) for plain display.
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
        // Forgejo's job-log endpoint isn't wired up (`LogMode::None`); opening
        // the log screen anyway would poll a request that fails forever
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
                duration_secs: Some(1),
            },
            LogStepMeta {
                name: "Run build".into(),
                status: JobStatus::Failed,
                start_key: ts_sort_key("2026-06-20T00:00:05Z"),
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
    fn ci_log_mouse_cancel_drops_the_visual_anchor() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let mut view = CiLogView::parse(RAW, &[]);
        view.visual_anchor = Some(0);
        app.ci_log = Some(view);

        app.ci_log_mouse(MouseGesture::Cancel);
        assert!(app.ci_log.as_ref().unwrap().visual_anchor.is_none());
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
                duration_secs: Some(1),
            },
            LogStepMeta {
                name: "Cleanup (skipped)".into(),
                status: JobStatus::Skipped,
                start_key: 0,
                duration_secs: None,
            },
            LogStepMeta {
                name: "Run build".into(),
                status: JobStatus::Ok,
                start_key: ts_sort_key("2026-06-20T00:00:05Z"),
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
