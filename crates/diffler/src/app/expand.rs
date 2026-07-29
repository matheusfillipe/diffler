//! Diff context expansion: re-diff the selected file's own text at more (or
//! all) context so the reviewer can see around a hunk. Highlights index by line
//! number and so cover the revealed lines for free; changed-line emphasis is
//! carried onto the rebuilt hunks.

use std::collections::HashMap;
use std::ops::Range;

use diffler_core::git::rehunk_file;
use diffler_core::model::{FileDiff, Hunk};

use super::App;

/// Emphasis ranges keyed by a line's (old, new) line numbers.
type EmphasisByLine = HashMap<(Option<u32>, Option<u32>), Vec<Range<usize>>>;

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
        // rebuild at `target` even when collapsing to default (re-diffing at the
        // default context restores the original hunks)
        self.rebuild_file(&path, target);
    }

    fn rebuild_file(&mut self, path: &str, context: u32) {
        let changed = self
            .diff_file_mut(path)
            .is_some_and(|file| apply_context(file, context));
        if changed && let Some(diff) = self.diff.as_mut() {
            diff.mark_rows_dirty();
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

/// Rebuild `file`'s hunks at `context`, carrying changed-line emphasis onto the
/// rebuilt lines. Returns whether the hunks were replaced.
pub(super) fn apply_context(file: &mut FileDiff, context: u32) -> bool {
    let Some(mut hunks) = rehunk_file(file, context) else {
        return false;
    };
    carry_emphasis(&file.hunks, &mut hunks);
    file.hunks = hunks;
    true
}

/// Copy emphasis from the current hunks onto rebuilt ones by line number: the
/// changed lines are identical across contexts, so their emphasis survives a
/// re-diff without re-enriching.
fn carry_emphasis(old: &[Hunk], new: &mut [Hunk]) {
    let mut prior: EmphasisByLine = HashMap::new();
    for line in old.iter().flat_map(|h| &h.lines) {
        if !line.emphasis.is_empty() {
            prior.insert((line.old_no, line.new_no), line.emphasis.clone());
        }
    }
    if prior.is_empty() {
        return;
    }
    for line in new.iter_mut().flat_map(|h| &mut h.lines) {
        if let Some(ranges) = prior.get(&(line.old_no, line.new_no)) {
            line.emphasis.clone_from(ranges);
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
}
