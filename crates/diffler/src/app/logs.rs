//! The CI job-log view: the `gh ... --log` output parsed into collapsible
//! steps. Each line of that output is `<job>\t<step>\t<timestamp> <text>`, so
//! consecutive lines are grouped by their step column. Folded by default. The
//! screen is keymap-driven (motions/search/visual/yank) like the diff screen.

use ratatui::layout::Rect;

/// One collapsible step: its name and the log lines under it.
pub struct LogStep {
    pub name: String,
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
    /// Parse `gh ... --log` output into steps, folded by default.
    pub fn parse(raw: &str) -> Self {
        let mut steps: Vec<LogStep> = Vec::new();
        for raw_line in raw.lines() {
            let (step, text) = split_log_line(raw_line);
            let text = strip_ansi(&text);
            match steps.last_mut() {
                Some(last) if last.name == step => last.lines.push(text),
                _ => steps.push(LogStep {
                    name: step,
                    lines: vec![text],
                    folded: true,
                }),
            }
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

    /// Carry the prior view's cursor, scroll, viewport, visual selection, and
    /// per-step fold state (matched by step name) onto a freshly-parsed view, so
    /// a re-poll that appends lines doesn't reset what the user folded or where
    /// they are. New steps keep the default folded state.
    #[must_use]
    pub fn carry_into(self, mut next: LogsView) -> LogsView {
        for step in &mut next.steps {
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

/// Split a `gh --log` line `<job>\t<step>\t<timestamp> <text>` into
/// `(step, text)`. A line without that tab structure goes to an empty step.
fn split_log_line(line: &str) -> (String, String) {
    let mut fields = line.splitn(3, '\t');
    match (fields.next(), fields.next(), fields.next()) {
        (Some(_job), Some(step), Some(rest)) => (step.to_owned(), strip_timestamp(rest)),
        _ => (String::new(), strip_timestamp(line)),
    }
}

/// Drop a leading ISO-8601 timestamp token (`2026-…Z `) if present.
fn strip_timestamp(s: &str) -> String {
    if let Some((head, rest)) = s.split_once(' ')
        && head.ends_with('Z')
        && head.len() >= 20
        && head.starts_with(|c: char| c.is_ascii_digit())
    {
        rest.to_owned()
    } else {
        s.to_owned()
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

    const RAW: &str = "job\tBuild\t2026-06-20T00:00:01Z compiling…\n\
                       job\tBuild\t2026-06-20T00:00:02Z \u{1b}[32mok\u{1b}[0m\n\
                       job\tTest\t2026-06-20T00:00:03Z running\n";

    #[test]
    fn parse_groups_by_step_strips_prefix_and_ansi() {
        let view = LogsView::parse(RAW);
        assert_eq!(view.steps.len(), 2);
        assert_eq!(view.steps[0].name, "Build");
        assert_eq!(view.steps[0].lines, vec!["compiling…", "ok"]);
        assert_eq!(view.steps[1].lines, vec!["running"]);
        assert!(view.steps.iter().all(|s| s.folded));
    }

    #[test]
    fn folded_view_shows_only_headers() {
        let view = LogsView::parse(RAW);
        assert_eq!(view.rows(), vec![LogsRow::Step(0), LogsRow::Step(1)]);
    }

    #[test]
    fn toggle_fold_reveals_lines_and_reseats_cursor() {
        let mut view = LogsView::parse(RAW);
        view.toggle_fold_at_cursor();
        assert_eq!(
            view.rows(),
            vec![
                LogsRow::Step(0),
                LogsRow::Line { step: 0, line: 0 },
                LogsRow::Line { step: 0, line: 1 },
                LogsRow::Step(1),
            ]
        );
        view.cursor = 2;
        view.toggle_fold_at_cursor();
        assert_eq!(view.cursor, 0, "cursor re-seats on the folded step header");
    }

    #[test]
    fn selection_text_joins_the_visual_range() {
        let mut view = LogsView::parse(RAW);
        view.toggle_fold_at_cursor();
        view.visual_anchor = Some(1);
        view.cursor = 2;
        assert_eq!(view.selection_text(), "compiling…\nok");
    }

    #[test]
    fn folding_clamps_a_stale_visual_anchor() {
        let mut view = LogsView::parse(RAW);
        view.toggle_fold_at_cursor();
        view.visual_anchor = Some(2);
        view.cursor = 2;
        view.toggle_fold_at_cursor();
        let last = view.rows().len() - 1;
        assert!(view.visual_anchor.is_some_and(|a| a <= last));
    }

    #[test]
    fn carry_into_preserves_fold_state_by_name() {
        let mut prev = LogsView::parse(RAW);
        prev.toggle_fold_at_cursor();
        let next = prev.carry_into(LogsView::parse(RAW));
        assert!(!next.steps[0].folded, "Build stays unfolded across re-poll");
        assert!(next.steps[1].folded);
    }
}
