//! Diff context expansion: re-diff the selected file's own text at more (or
//! all) context. Highlights index by line number, so they already cover the
//! revealed lines.

use std::collections::HashMap;
use std::ops::Range;

use diffler_core::git::rehunk_file;
use diffler_core::model::{FileDiff, Hunk};

use super::App;

/// Emphasis ranges and the reformat-only flag, keyed by a line's (old, new)
/// line numbers.
type EmphasisByLine = HashMap<(Option<u32>, Option<u32>), (Vec<Range<usize>>, bool)>;

/// Lines added to a file's context on each expand step.
const STEP: u32 = 20;
const WHOLE_FILE: u32 = u32::MAX;

impl App {
    pub(crate) fn expand_context(&mut self) {
        self.change_context(|c| c.saturating_add(STEP));
    }

    pub(crate) fn collapse_context(&mut self) {
        let floor = self.default_context();
        // WHOLE_FILE is u32::MAX and can't step down, so snap it to the floor
        self.change_context(move |c| {
            if c == WHOLE_FILE {
                floor
            } else {
                c.saturating_sub(STEP).max(floor)
            }
        });
    }

    pub(crate) fn expand_whole_file(&mut self) {
        self.change_context(|_| WHOLE_FILE);
    }

    fn default_context(&self) -> u32 {
        self.config.ui.context_lines
    }

    fn change_context(&mut self, next: impl Fn(u32) -> u32) {
        let Some(path) = self
            .diff
            .as_ref()
            .and_then(|d| d.selected_path(&self.review))
        else {
            return;
        };
        let default = self.default_context();
        let current = self
            .diff
            .as_ref()
            .and_then(|d| d.context.get(&path).copied())
            .unwrap_or(default);
        let target = next(current);
        if let Some(diff) = self.diff.as_mut() {
            if target <= default {
                diff.context.remove(&path);
            } else {
                diff.context.insert(path.clone(), target);
            }
        }
        // collapsing to default rebuilds too, which restores the original hunks
        self.rebuild_file(&path, target);
    }

    /// Re-diff `path` at `context`, keeping the cursor on its line and that
    /// line on its screen row.
    fn rebuild_file(&mut self, path: &str, context: u32) {
        let algorithm = self.config.diff.algorithm;
        let indent_heuristic = self.config.diff.indent_heuristic;
        // we name the cursor's line before the hunks change, since its row
        // indices only mean something against the hunks they were built from
        let positions = self
            .diff
            .as_ref()
            .map(|diff| diff.capture_positions(&self.review));
        let changed = self
            .diff_file_mut(path)
            .is_some_and(|file| apply_context(file, context, algorithm, indent_heuristic));
        if changed
            && let Some(diff) = self.diff.as_mut()
            && let Some(positions) = positions
        {
            diff.rebuild_in_place(&self.review, positions);
        }
    }

    fn diff_file_mut(&mut self, path: &str) -> Option<&mut FileDiff> {
        match self.diff.as_mut()?.commit_model.as_mut() {
            Some(model) => model.files.iter_mut().find(|f| f.path == path),
            None => self
                .review
                .model_mut()
                .files
                .iter_mut()
                .find(|f| f.path == path),
        }
    }
}

/// Rebuild `file`'s hunks at `context`. Returns whether the hunks were
/// replaced.
pub(super) fn apply_context(
    file: &mut FileDiff,
    context: u32,
    algorithm: diffler_core::diffalgo::DiffAlgorithm,
    indent_heuristic: bool,
) -> bool {
    let Some(mut hunks) = rehunk_file(file, context, algorithm, indent_heuristic) else {
        return false;
    };
    carry_emphasis(&file.hunks, &mut hunks);
    file.hunks = hunks;
    true
}

/// Copy emphasis onto rebuilt hunks by line number. The changed lines are the
/// same at any context, so we skip re-enriching.
fn carry_emphasis(old: &[Hunk], new: &mut [Hunk]) {
    let mut prior: EmphasisByLine = HashMap::new();
    for line in old.iter().flat_map(|h| &h.lines) {
        if !line.emphasis.is_empty() || line.reformat_only {
            prior.insert(
                (line.old_no, line.new_no),
                (line.emphasis.clone(), line.reformat_only),
            );
        }
    }
    if prior.is_empty() {
        return;
    }
    for line in new.iter_mut().flat_map(|h| &mut h.lines) {
        if let Some((ranges, reformat_only)) = prior.get(&(line.old_no, line.new_no)) {
            line.emphasis.clone_from(ranges);
            line.reformat_only = *reformat_only;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use diffler_core::model::LineKind;

    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, key};

    fn context_count(app: &App) -> usize {
        app.review.model().files.first().map_or(0, |f| {
            f.hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.kind == LineKind::Context)
                .count()
        })
    }

    fn changed_file_app() -> App {
        let fixture = Fixture::new();
        let mut base = String::new();
        for i in 1..=40 {
            let _ = writeln!(base, "line {i}");
        }
        fixture.write("a.txt", &base);
        fixture.commit_all("base");
        fixture.write("a.txt", &base.replace("line 20\n", "LINE TWENTY\n"));
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("a.txt");
        app
    }

    #[test]
    fn plus_expands_and_equals_shows_the_whole_file() {
        let mut app = changed_file_app();
        let default = context_count(&app);
        assert_eq!(default, 6, "git default context each side");

        app.handle(key('+'));
        assert!(context_count(&app) > default, "+ reveals more context");

        app.handle(key('='));
        assert_eq!(context_count(&app), 39, "= shows every unchanged line");

        app.handle(key('-'));
        assert_eq!(context_count(&app), 6, "- collapses back to the default");
    }

    #[test]
    fn expanding_keeps_the_structural_reformat_dimming() {
        let fixture = Fixture::new();
        fixture.write("a.rs", "fn f() {\n    let x = compute();\n}\n");
        fixture.commit_all("base");
        fixture.write("a.rs", "fn f() {\n        let x = compute();\n}\n");
        let mut config = LoadedConfig::default();
        config.config.diff.algorithm = diffler_core::diffalgo::DiffAlgorithm::Structural;
        let mut app = App::new(fixture.review(), config);
        app.review.refresh().expect("refresh");
        app.open_working_tree_file("a.rs");
        app.queue_enrich_selected();
        app.enrich_now();
        let dimmed = |app: &App| {
            app.review.model().files[0]
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.reformat_only)
                .count()
        };
        assert_eq!(dimmed(&app), 2, "the reindented pair dims");
        app.handle(key('+'));
        assert_eq!(dimmed(&app), 2, "still dimmed after expanding");
    }

    #[test]
    fn expansion_survives_re_enrichment() {
        let mut app = changed_file_app();
        app.handle(key('='));
        let expanded = context_count(&app);
        // enrichment ships default-context hunks; the override must reinstall
        app.enrich_now();
        assert_eq!(
            context_count(&app),
            expanded,
            "still whole-file after enrich"
        );
    }

    /// The screen row showing `needle`, from a fresh render.
    fn screen_row_of(app: &mut App, needle: &str) -> Option<u16> {
        let terminal = crate::test_support::render(app);
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height).find(|&y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .contains(needle)
        })
    }

    #[test]
    fn expanding_keeps_the_cursor_line_where_it_sits_on_screen() {
        let fixture = Fixture::new();
        let mut base = String::new();
        for i in 1..=120 {
            let _ = writeln!(base, "line {i}");
        }
        fixture.write("a.txt", &base);
        fixture.commit_all("base");
        let changed = base
            .replace("line 30\n", "LINE THIRTY\n")
            .replace("line 70\n", "LINE SEVENTY\n");
        fixture.write("a.txt", &changed);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("a.txt");
        app.diff.as_mut().expect("diff").focus = crate::app::Pane::Diff;
        for _ in 0..12 {
            app.handle(key('j'));
        }
        let before = screen_row_of(&mut app, "LINE SEVENTY");
        assert!(before.is_some(), "the line is on screen");
        for press in ['+', '+', '=', '-'] {
            app.handle(key(press));
            assert_eq!(
                screen_row_of(&mut app, "LINE SEVENTY"),
                before,
                "after {press} the cursor's line stays on its screen row"
            );
        }
    }
}
