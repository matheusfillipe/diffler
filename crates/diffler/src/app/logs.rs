//! The CI job-log view: a job's log grouped into its real steps. Each line is
//! `<timestamp> <text>` (the REST log; the parser also tolerates `gh`'s
//! `<job>\t<step>\t<timestamp>` form). No API exposes per-step log content, so
//! lines are bucketed into the step metadata (name/status/timing from the jobs
//! API) by timestamp: a line belongs to the last step whose start it's at or
//! after. Without step metadata (e.g. GitLab) it falls back to the runner's
//! `##[group]` markers. Folded by default; keymap-driven like the diff.

use diffler_ci::{JobStatus, LogStepMeta, ts_sort_key};
use ratatui::layout::Rect;

/// One collapsible step: its name, status, run time, and log lines. `name` is
/// empty (and `status` `None`) for the leading section of pre-step output.
pub struct LogStep {
    pub name: String,
    pub status: Option<JobStatus>,
    pub duration_secs: Option<i64>,
    pub lines: Vec<String>,
    pub folded: bool,
}

/// A cursor-addressable row of the log view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogsRow {
    Step(usize),
    Line { step: usize, line: usize },
}

/// State for the CI logs screen.
pub struct LogsView {
    pub steps: Vec<LogStep>,
    pub cursor: usize,
    pub scroll: usize,
    pub visual_anchor: Option<usize>,
    pub viewport: u16,
    pub body: Rect,
}

impl LogsView {
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
    pub fn carry_into(self, mut next: LogsView) -> LogsView {
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
    pub fn rows(&self) -> Vec<LogsRow> {
        let mut rows = Vec::new();
        for (s, step) in self.steps.iter().enumerate() {
            rows.push(LogsRow::Step(s));
            if !step.folded {
                rows.extend((0..step.lines.len()).map(|line| LogsRow::Line { step: s, line }));
            }
        }
        rows
    }

    /// Display text of a row (the step name, or a log line), for search/render.
    pub fn row_text(&self, row: LogsRow) -> &str {
        match row {
            LogsRow::Step(s) => self.steps.get(s).map_or("", |st| st.name.as_str()),
            LogsRow::Line { step, line } => self
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
            LogsRow::Step(s) | LogsRow::Line { step: s, .. } => *s,
        }) else {
            return;
        };
        if let Some(st) = self.steps.get_mut(step) {
            st.folded = !st.folded;
        }
        let last = self.rows().len().saturating_sub(1);
        if let Some(pos) = self.rows().iter().position(|r| *r == LogsRow::Step(step)) {
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

/// Bucket lines into the job's real steps by timestamp: a line joins the last
/// step whose start it's at or after; earlier lines form a leading section.
fn sections_by_step(raw: &str, metas: &[LogStepMeta]) -> Vec<LogStep> {
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
        steps.push(LogStep {
            name: String::new(),
            status: None,
            duration_secs: None,
            lines: leading,
            folded: true,
        });
    }
    for (meta, lines) in metas.iter().zip(buckets) {
        steps.push(LogStep {
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
fn sections_by_group(raw: &str) -> Vec<LogStep> {
    let mut steps: Vec<LogStep> = Vec::new();
    let mut leading: Vec<String> = Vec::new();
    for raw_line in raw.lines() {
        let (_, content) = line_key_and_content(raw_line);
        if let Some(name) = content.strip_prefix("##[group]") {
            steps.push(LogStep {
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
            LogStep {
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
        let view = LogsView::parse(RAW, &[]);
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
        let view = LogsView::parse(RAW, &[]);
        assert_eq!(
            view.rows(),
            vec![LogsRow::Step(0), LogsRow::Step(1), LogsRow::Step(2)]
        );
    }

    #[test]
    fn toggle_fold_reveals_lines_and_reseats_cursor() {
        let mut view = LogsView::parse(RAW, &[]);
        view.cursor = 1; // the Build section header
        view.toggle_fold_at_cursor();
        assert_eq!(
            view.rows(),
            vec![
                LogsRow::Step(0),
                LogsRow::Step(1),
                LogsRow::Line { step: 1, line: 0 },
                LogsRow::Line { step: 1, line: 1 },
                LogsRow::Step(2),
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
        let mut view = LogsView::parse(RAW, &[]);
        view.cursor = 1;
        view.toggle_fold_at_cursor();
        view.visual_anchor = Some(2);
        view.cursor = 3;
        assert_eq!(view.selection_text(), "compiling…\nok");
    }

    #[test]
    fn folding_clamps_a_stale_visual_anchor() {
        let mut view = LogsView::parse(RAW, &[]);
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
        let mut prev = LogsView::parse(RAW, &[]);
        prev.cursor = 1;
        prev.toggle_fold_at_cursor();
        let next = prev.carry_into(LogsView::parse(RAW, &[]));
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
        let view = LogsView::parse(RAW, &metas);
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
        let view = LogsView::parse(RAW, &metas);
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
