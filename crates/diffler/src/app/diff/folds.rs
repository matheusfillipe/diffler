//! Hide/show folds inside the diff pane: a run of unremarkable lines of the
//! selected file collapses into one dim row that names what it hides. A
//! region is found by one of the [`FoldKind`] rules and named by `key`, built
//! from its own content, so it survives a rebuild that moves its rows.
//!
//! A comment is never hidden: its rows sit inside a region without belonging
//! to it, so a closed region around one folds as two rows with the card
//! between them, and a region holding a commented line starts open.

use std::collections::{HashMap, HashSet};

use diffler_core::model::{DiffLine, DiffModel, FileDiff, LineKind};
use diffler_core::syntax::{Def, DefKind, ScopeIndex};

use super::rows::line_row_text;
use super::{DiffRow, RowCopy, SplitRow};
use crate::config::FoldKind;

/// A deleted run this long folds whole; a shorter one folds only its middle.
const DELETED_BODY_MIN: usize = 12;
const REMOVED_RUN_MIN: usize = 5;
const CONTEXT_MIN: usize = 5;
/// Closed regions at most this many unchanged rows apart share one fold row.
const MERGE_GAP: usize = 2;

/// One fold-worthy stretch, as the `(hunk, line)` pairs it covers, so the
/// unified and split row builders each find their own rows for it.
#[derive(Debug, Clone)]
pub(crate) struct FoldRegion {
    pub key: String,
    pub lines: Vec<(usize, usize)>,
    pub starts_closed: bool,
    noun: &'static str,
    what: Option<String>,
}

impl FoldRegion {
    /// The fold row's text for `hidden` of this region's lines.
    pub(crate) fn label(&self, hidden: usize) -> String {
        match &self.what {
            Some(what) => format!("⋯ {hidden} {} · {what}", self.noun),
            None => format!("⋯ {hidden} {}", self.noun),
        }
    }
}

/// What one unified `DiffRow::Fold` row stands for: one region, or several
/// once close neighbours merge.
#[derive(Debug, Clone)]
pub(crate) struct FoldGroup {
    pub keys: Vec<String>,
    /// Every line the row hides, in order, the gap lines of a merge included.
    pub lines: Vec<(usize, usize)>,
    pub label: String,
}

/// What decides which regions exist and which start closed, for one file.
pub(crate) struct FoldRules<'a> {
    pub scope: Option<&'a ScopeIndex>,
    pub enabled: &'a HashSet<FoldKind>,
    /// `false` where nothing starts folded, a walkthrough slide.
    pub defaults: bool,
    /// The reader widened this file's context with `+`/`=`, so we leave the
    /// context they asked to see open.
    pub context_expanded: bool,
    /// `(on_old_side, first, last)` of every comment anchored in the file.
    pub comment_spans: &'a [(bool, u32, u32)],
}

#[derive(Clone, Copy)]
struct Entry<'a> {
    hunk: usize,
    line: usize,
    diff: &'a DiffLine,
}

impl Entry<'_> {
    /// 0-based new-side row, the index the scope index speaks in.
    fn new_row(&self) -> Option<usize> {
        self.diff.new_no.map(|no| no.saturating_sub(1) as usize)
    }
}

/// A region before it is named: `start..end` of one run, and the test
/// definition it folds when it is a `Tests` one.
struct Piece<'a> {
    kind: FoldKind,
    start: usize,
    end: usize,
    test: Option<&'a Def>,
}

/// The `Line` rows split at every hunk header. Comment and composer rows sit
/// inside a run without breaking it, so adding a comment keeps the identity
/// of the region around it.
fn runs<'a>(rows: &[DiffRow], model: &'a DiffModel) -> Vec<Vec<Entry<'a>>> {
    let mut runs = vec![Vec::new()];
    for row in rows {
        match *row {
            DiffRow::Line { file, hunk, line } => {
                let diff = model
                    .files
                    .get(file)
                    .and_then(|f| f.hunks.get(hunk))
                    .and_then(|h| h.lines.get(line));
                if let (Some(diff), Some(run)) = (diff, runs.last_mut()) {
                    run.push(Entry { hunk, line, diff });
                }
            }
            DiffRow::Comment { .. } | DiffRow::Composer { .. } => {}
            DiffRow::Hunk { .. } | DiffRow::Summary { .. } | DiffRow::Fold { .. } => {
                runs.push(Vec::new());
            }
        }
    }
    runs.retain(|run| !run.is_empty());
    runs
}

/// `test_foo`, `testFoo`, `TestFoo` or `foo_test`, the names test runners
/// collect. A name that only contains the word (`latest`, `attestation`)
/// never matches.
fn test_named(name: &str) -> bool {
    let rest = name
        .strip_prefix("test")
        .or_else(|| name.strip_prefix("Test"));
    let prefixed = rest.is_some_and(|rest| {
        rest.is_empty()
            || rest.starts_with('_')
            || rest.starts_with(|c: char| c.is_ascii_uppercase())
    });
    prefixed || name.ends_with("_test")
}

fn looks_like_test(def: &Def) -> bool {
    match def.kind {
        DefKind::Module => matches!(def.name.to_ascii_lowercase().as_str(), "test" | "tests"),
        DefKind::Function => test_named(&def.name),
        DefKind::Other => false,
    }
}

/// `defs` in file order, each one nested inside another dropped.
fn outermost(mut defs: Vec<&Def>) -> Vec<&Def> {
    defs.sort_by_key(|d| (d.start_row, std::cmp::Reverse(d.end_row)));
    let mut kept: Vec<&Def> = Vec::new();
    for def in defs {
        // spans nest like the tree they came from, so only the last kept one
        // can still contain this one
        let nested = kept
            .last()
            .is_some_and(|k| k.start_row <= def.start_row && def.end_row <= k.end_row);
        if !nested {
            kept.push(def);
        }
    }
    kept
}

fn def_name(def: &Def) -> String {
    match def.kind {
        DefKind::Module => format!("mod {}", def.name),
        DefKind::Function => format!("fn {}", def.name),
        DefKind::Other => def.name.clone(),
    }
}

/// Start rows of every definition enclosing a changed line: the scopes whose
/// signature and closing line a context fold leaves visible.
fn changed_scope_starts(runs: &[Vec<Entry<'_>>], scope: &ScopeIndex) -> HashSet<usize> {
    let mut starts = HashSet::new();
    let changed = runs
        .iter()
        .flatten()
        .filter(|e| e.diff.kind != LineKind::Context)
        .filter_map(Entry::new_row);
    for at in changed {
        for def in scope.defs() {
            if def.start_row <= at && at <= def.end_row {
                starts.insert(def.start_row);
            }
        }
    }
    starts
}

/// Shrink a context run so a changed scope's own signature or closing line
/// stays visible at either edge.
fn trim_context(
    run: &[Entry<'_>],
    (mut start, mut end): (usize, usize),
    changed: &HashSet<usize>,
    scope: Option<&ScopeIndex>,
) -> (usize, usize) {
    let Some(scope) = scope else {
        return (start, end);
    };
    let bounds_a_change = |index: usize| {
        run.get(index).and_then(Entry::new_row).is_some_and(|row| {
            scope
                .defs()
                .iter()
                .any(|d| (d.start_row == row || d.end_row == row) && changed.contains(&d.start_row))
        })
    };
    if bounds_a_change(start) {
        start += 1;
    }
    if end > start && bounds_a_change(end - 1) {
        end -= 1;
    }
    (start, end)
}

fn diff_pieces(
    run: &[Entry<'_>],
    (start, end): (usize, usize),
    changed: &HashSet<usize>,
    rules: &FoldRules<'_>,
    out: &mut Vec<Piece<'_>>,
) {
    let on = |kind| rules.enabled.contains(&kind);
    let mut k = start;
    while k < end {
        let kind = run.get(k).map(|e| e.diff.kind);
        let m = (k..end)
            .find(|&m| run.get(m).map(|e| e.diff.kind) != kind)
            .unwrap_or(end);
        let len = m - k;
        // deletions an addition replaces are the old side of a change the
        // reader compares, so only a deletion nothing replaces folds
        let removed = kind == Some(LineKind::Deleted)
            && run.get(m).map(|e| e.diff.kind) != Some(LineKind::Added);
        let piece = |kind, start, end| Piece {
            kind,
            start,
            end,
            test: None,
        };
        match kind {
            Some(LineKind::Deleted)
                if removed && len >= DELETED_BODY_MIN && on(FoldKind::DeletedBodies) =>
            {
                out.push(piece(FoldKind::DeletedBodies, k, m));
            }
            Some(LineKind::Deleted)
                if removed && len >= REMOVED_RUN_MIN && on(FoldKind::RemovedRuns) =>
            {
                out.push(piece(FoldKind::RemovedRuns, k + 1, m - 1));
            }
            Some(LineKind::Context) if len >= CONTEXT_MIN && on(FoldKind::Context) => {
                let (s, e) = trim_context(run, (k, m), changed, rules.scope);
                if e > s {
                    out.push(piece(FoldKind::Context, s, e));
                }
            }
            _ => {}
        }
        k = m;
    }
}

/// Every region of one run: stretches inside a test definition fold as one
/// (its signature line kept visible), the rest by line kind.
fn pieces<'a>(
    run: &[Entry<'_>],
    tests: &[&'a Def],
    changed: &HashSet<usize>,
    rules: &FoldRules<'_>,
) -> Vec<Piece<'a>> {
    let covering = |index: usize| {
        let row = run.get(index).and_then(Entry::new_row)?;
        tests
            .iter()
            .position(|t| t.start_row <= row && row <= t.end_row)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < run.len() {
        let test = covering(i);
        let j = (i + 1..run.len())
            .find(|&j| covering(j) != test)
            .unwrap_or(run.len());
        match test.and_then(|t| tests.get(t)) {
            Some(&def) => {
                let signature = run.get(i).and_then(Entry::new_row) == Some(def.start_row);
                let start = if signature { i + 1 } else { i };
                if start < j {
                    out.push(Piece {
                        kind: FoldKind::Tests,
                        start,
                        end: j,
                        test: Some(def),
                    });
                }
            }
            None => diff_pieces(run, (i, j), changed, rules, &mut out),
        }
        i = j;
    }
    out
}

/// What a fold hides in scope terms: the definitions that start inside it,
/// else the innermost one enclosing all of it.
fn hidden_scope(run: &[Entry<'_>], piece: &Piece<'_>, scope: &ScopeIndex) -> Option<String> {
    let hidden = run.get(piece.start..piece.end)?;
    let rows: Vec<usize> = hidden.iter().filter_map(Entry::new_row).collect();
    let (first, last) = if let (Some(&first), Some(&last)) = (rows.first(), rows.last()) {
        (first, last)
    } else {
        // deletions carry no new-side row, so we place them at the line
        // before them, or after them at the top of a hunk
        let before = run
            .get(..piece.start)?
            .iter()
            .rev()
            .find_map(Entry::new_row);
        let at = before.or_else(|| run.get(piece.end..)?.iter().find_map(Entry::new_row))?;
        (at, at)
    };
    let starts: Vec<&Def> = scope
        .defs()
        .iter()
        .filter(|d| first <= d.start_row && d.start_row <= last)
        .collect();
    if let Some((head, rest)) = outermost(starts).split_first() {
        return Some(match rest.len() {
            0 => def_name(head),
            more => format!("{} +{more} more", def_name(head)),
        });
    }
    scope
        .defs()
        .iter()
        .filter(|d| d.start_row <= first && last <= d.end_row)
        .max_by_key(|d| d.start_row)
        .map(def_name)
}

fn in_comment(line: &DiffLine, spans: &[(bool, u32, u32)]) -> bool {
    spans.iter().any(|&(old_side, first, last)| {
        line.number_on(old_side)
            .is_some_and(|no| first <= no && no <= last)
    })
}

fn region(
    run: &[Entry<'_>],
    piece: &Piece<'_>,
    rules: &FoldRules<'_>,
    seen: &mut HashMap<String, usize>,
) -> FoldRegion {
    let hidden = run.get(piece.start..piece.end).unwrap_or_default();
    let ident = match piece.test {
        Some(def) => def.name.as_str(),
        None => hidden
            .iter()
            .map(|e| e.diff.text.trim())
            .find(|text| !text.is_empty())
            .unwrap_or_default(),
    };
    // a region's first non-blank line names it, and the occurrence count
    // tells apart two that open on the same text; line numbers and the run's
    // length stay out, so an edit elsewhere never renames it
    let name = format!("{:?}:{ident}", piece.kind);
    let occurrence = seen.entry(name.clone()).or_default();
    let key = format!("{name}#{occurrence}");
    *occurrence += 1;
    let holds_comment = hidden
        .iter()
        .any(|e| in_comment(e.diff, rules.comment_spans));
    let expanded_context = piece.kind == FoldKind::Context && rules.context_expanded;
    let what = match piece.test {
        Some(def) => Some(def_name(def)),
        None => rules
            .scope
            .and_then(|scope| hidden_scope(run, piece, scope)),
    };
    FoldRegion {
        key,
        lines: hidden.iter().map(|e| (e.hunk, e.line)).collect(),
        starts_closed: rules.defaults && !holds_comment && !expanded_context,
        noun: match piece.kind {
            FoldKind::DeletedBodies | FoldKind::RemovedRuns => "deleted lines",
            FoldKind::Tests | FoldKind::Context => "lines",
        },
        what,
    }
}

/// Fold-worthy regions of `rows`, the selected file's freshly built (and,
/// for the walkthrough layout, slide-narrowed) row list.
pub(crate) fn compute_regions(
    rows: &[DiffRow],
    model: &DiffModel,
    rules: &FoldRules<'_>,
) -> Vec<FoldRegion> {
    let runs = runs(rows, model);
    let tests = rules
        .scope
        .filter(|_| rules.enabled.contains(&FoldKind::Tests))
        .map(|scope| outermost(scope.defs().iter().filter(|d| looks_like_test(d)).collect()))
        .unwrap_or_default();
    let changed = rules
        .scope
        .filter(|_| rules.enabled.contains(&FoldKind::Context))
        .map(|scope| changed_scope_starts(&runs, scope))
        .unwrap_or_default();
    let mut seen = HashMap::new();
    let mut regions = Vec::new();
    for run in &runs {
        for piece in pieces(run, &tests, &changed, rules) {
            regions.push(region(run, &piece, rules, &mut seen));
        }
    }
    regions
}

/// The region each closed line belongs to, the lookup both row builders read.
fn closed_index(
    regions: &[FoldRegion],
    overrides: &HashMap<String, bool>,
) -> HashMap<(usize, usize), usize> {
    let mut owner = HashMap::new();
    for (index, region) in regions.iter().enumerate() {
        let closed = overrides
            .get(&region.key)
            .copied()
            .unwrap_or(region.starts_closed);
        if closed {
            for &line in &region.lines {
                owner.insert(line, index);
            }
        }
    }
    owner
}

struct Span {
    start: usize,
    end: usize,
    regions: Vec<usize>,
}

fn line_kind_at(rows: &[DiffRow], model: &DiffModel, index: usize) -> Option<LineKind> {
    let DiffRow::Line { file, hunk, line } = *rows.get(index)? else {
        return None;
    };
    Some(
        model
            .files
            .get(file)?
            .hunks
            .get(hunk)?
            .lines
            .get(line)?
            .kind,
    )
}

/// Closed regions as row spans, merging two separated by at most
/// [`MERGE_GAP`] unchanged rows: a change or a comment in the gap never
/// merges away.
fn merged_spans(
    rows: &[DiffRow],
    model: &DiffModel,
    owner: &HashMap<(usize, usize), usize>,
) -> Vec<Span> {
    let region_of = |index: usize| match rows.get(index) {
        Some(DiffRow::Line { hunk, line, .. }) => owner.get(&(*hunk, *line)).copied(),
        _ => None,
    };
    let mut merged: Vec<Span> = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let Some(region) = region_of(i) else {
            i += 1;
            continue;
        };
        let end = (i + 1..rows.len())
            .find(|&j| region_of(j) != Some(region))
            .unwrap_or(rows.len());
        let joins = merged.last().is_some_and(|prev| {
            i - prev.end <= MERGE_GAP
                && (prev.end..i).all(|k| line_kind_at(rows, model, k) == Some(LineKind::Context))
        });
        match merged.last_mut() {
            Some(prev) if joins => {
                prev.end = end;
                prev.regions.push(region);
            }
            _ => merged.push(Span {
                start: i,
                end,
                regions: vec![region],
            }),
        }
        i = end;
    }
    merged
}

fn hidden_lines(rows: &[DiffRow], span: &Span) -> Vec<(usize, usize)> {
    rows.get(span.start..span.end)
        .unwrap_or_default()
        .iter()
        .filter_map(|row| match *row {
            DiffRow::Line { hunk, line, .. } => Some((hunk, line)),
            _ => None,
        })
        .collect()
}

/// The `DiffLine`s a fold row's own `(hunk, line)` pairs point to, resolved
/// against `file` and paired with their own position, so a yank, a search
/// match, and a search jump can each read the one thing a fold row hides.
pub(crate) fn resolve_hidden<'a>(
    file: &'a FileDiff,
    lines: &[(usize, usize)],
) -> Vec<((usize, usize), &'a DiffLine)> {
    lines
        .iter()
        .filter_map(|&(hunk, line)| {
            let diff_line = file.hunks.get(hunk)?.lines.get(line)?;
            Some(((hunk, line), diff_line))
        })
        .collect()
}

/// The hidden lines as a plain-text buffer holds them, so a yank over a fold
/// row copies the code it stands for.
fn hidden_copy(file: &FileDiff, lines: &[(usize, usize)]) -> String {
    resolve_hidden(file, lines)
        .iter()
        .map(|(_, diff_line)| line_row_text(diff_line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collapse the unified row list's closed regions into `Fold` rows.
pub(crate) fn apply(
    rows: &[DiffRow],
    copy: &[RowCopy],
    model: &DiffModel,
    regions: &[FoldRegion],
    overrides: &HashMap<String, bool>,
) -> (Vec<DiffRow>, Vec<RowCopy>, Vec<FoldGroup>) {
    let owner = closed_index(regions, overrides);
    let mut spans = merged_spans(rows, model, &owner).into_iter().peekable();
    let mut groups = Vec::new();
    let mut out_rows = Vec::with_capacity(rows.len());
    let mut out_copy = Vec::with_capacity(copy.len());
    let mut i = 0;
    while i < rows.len() {
        if let Some(span) = spans.next_if(|span| span.start == i) {
            let lines = hidden_lines(rows, &span);
            let label = match span.regions.as_slice() {
                [only] => regions
                    .get(*only)
                    .map_or_else(String::new, |r| r.label(lines.len())),
                many => format!("⋯ {} lines · {} folded regions", lines.len(), many.len()),
            };
            let file_index = match rows.get(span.start) {
                Some(DiffRow::Line { file, .. }) => *file,
                _ => 0,
            };
            out_rows.push(DiffRow::Fold {
                file: file_index,
                group: groups.len(),
            });
            let copy_text = model
                .files
                .get(file_index)
                .map_or_else(String::new, |file| hidden_copy(file, &lines));
            out_copy.push(RowCopy::Text(copy_text));
            groups.push(FoldGroup {
                keys: span
                    .regions
                    .iter()
                    .filter_map(|&r| regions.get(r))
                    .map(|r| r.key.clone())
                    .collect(),
                lines,
                label,
            });
            i = span.end;
        } else {
            let (Some(row), Some(text)) = (rows.get(i), copy.get(i)) else {
                break;
            };
            out_rows.push(*row);
            out_copy.push(text.clone());
            i += 1;
        }
    }
    (out_rows, out_copy, groups)
}

/// The side-by-side counterpart of [`apply`]: each closed stretch of a region
/// becomes one `Fold` row, with no merging. A pair folds only when every line
/// it shows is closed in the same region, so a pair half in a fold stays on
/// screen whole.
pub(crate) fn apply_split(
    split: Vec<SplitRow>,
    regions: &[FoldRegion],
    overrides: &HashMap<String, bool>,
) -> Vec<SplitRow> {
    let owner = closed_index(regions, overrides);
    let mut out: Vec<SplitRow> = Vec::with_capacity(split.len());
    for row in split {
        let SplitRow::Pair { hunk, left, right } = row else {
            out.push(row);
            continue;
        };
        let mut owners = [left, right]
            .into_iter()
            .flatten()
            .map(|line| owner.get(&(hunk, line)).copied());
        let first = owners.next().flatten();
        let Some(region) = first.filter(|region| owners.all(|o| o == Some(*region))) else {
            out.push(row);
            continue;
        };
        // a context pair names one line on both sides, so it counts once
        let count = if left.is_some() && right.is_some() && left != right {
            2
        } else {
            1
        };
        match out.last_mut() {
            Some(SplitRow::Fold {
                region: last,
                lines,
            }) if *last == region => *lines += count,
            _ => out.push(SplitRow::Fold {
                region,
                lines: count,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use diffler_core::model::{HashCache, Hunk, HunkId};
    use diffler_core::syntax::registry::REGISTRY;

    use super::*;

    /// One file, one hunk, `lines` in order from line 1 on both sides:
    /// enough to drive region detection without a real git repo.
    fn model_of(lines: &[(LineKind, &str)]) -> DiffModel {
        let lines = lines
            .iter()
            .enumerate()
            .map(|(i, (kind, text))| {
                let no = Some(i as u32 + 1);
                let (old_no, new_no) = match kind {
                    LineKind::Deleted => (no, None),
                    LineKind::Added => (None, no),
                    LineKind::Context => (no, no),
                };
                DiffLine::new(*kind, old_no, new_no, (*text).to_owned())
            })
            .collect::<Vec<_>>();
        let count = lines.len() as u32;
        DiffModel {
            files: vec![FileDiff {
                path: "f.rs".to_owned(),
                old_path: None,
                status: diffler_core::model::FileStatus::Modified,
                binary: false,
                old_text: None,
                new_text: None,
                hunks: vec![Hunk {
                    id: HunkId("h".to_owned()),
                    old_start: 1,
                    old_lines: count,
                    new_start: 1,
                    new_lines: count,
                    context: String::new(),
                    lines,
                }],
                hashes: HashCache::default(),
            }],
        }
    }

    fn numbered(kinds: &[LineKind]) -> DiffModel {
        let texts: Vec<String> = (1..=kinds.len()).map(|i| format!("line {i}")).collect();
        let lines: Vec<(LineKind, &str)> = kinds
            .iter()
            .zip(&texts)
            .map(|(kind, text)| (*kind, text.as_str()))
            .collect();
        model_of(&lines)
    }

    fn rows_of(model: &DiffModel) -> Vec<DiffRow> {
        (0..model.files[0].hunks[0].lines.len())
            .map(|line| DiffRow::Line {
                file: 0,
                hunk: 0,
                line,
            })
            .collect()
    }

    fn rules<'a>(enabled: &'a HashSet<FoldKind>, scope: Option<&'a ScopeIndex>) -> FoldRules<'a> {
        FoldRules {
            scope,
            enabled,
            defaults: true,
            context_expanded: false,
            comment_spans: &[],
        }
    }

    fn every_kind() -> HashSet<FoldKind> {
        FoldKind::ALL.into()
    }

    fn fold_count(rows: &[DiffRow]) -> usize {
        rows.iter()
            .filter(|row| matches!(row, DiffRow::Fold { .. }))
            .count()
    }

    fn folded(
        model: &DiffModel,
        rows: &[DiffRow],
        regions: &[FoldRegion],
    ) -> (Vec<DiffRow>, Vec<RowCopy>, Vec<FoldGroup>) {
        let copy = vec![RowCopy::Text(String::new()); rows.len()];
        apply(rows, &copy, model, regions, &HashMap::new())
    }

    #[test]
    fn a_removed_run_folds_the_middle_and_keeps_its_ends_visible() {
        let model = numbered(&[LineKind::Deleted; 7]);
        let regions = compute_regions(&rows_of(&model), &model, &rules(&every_kind(), None));
        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0].lines,
            vec![(0, 1), (0, 2), (0, 3), (0, 4), (0, 5)],
            "row 0 and row 6 (first and last of the 7-line run) stay visible"
        );
        assert_eq!(regions[0].label(5), "⋯ 5 deleted lines");
    }

    #[test]
    fn a_run_of_twelve_deleted_lines_folds_whole() {
        let model = numbered(&[LineKind::Deleted; 12]);
        let regions = compute_regions(&rows_of(&model), &model, &rules(&every_kind(), None));
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].lines.len(), 12);
    }

    #[test]
    fn runs_below_threshold_fold_nothing() {
        let mut kinds = vec![LineKind::Deleted; 4];
        kinds.extend([LineKind::Context; 4]);
        let model = numbered(&kinds);
        assert!(compute_regions(&rows_of(&model), &model, &rules(&every_kind(), None)).is_empty());
    }

    #[test]
    fn a_disabled_kind_never_folds() {
        let model = numbered(&[LineKind::Deleted; 12]);
        let enabled = [FoldKind::Context].into();
        assert!(compute_regions(&rows_of(&model), &model, &rules(&enabled, None)).is_empty());
    }

    #[test]
    fn a_test_module_folds_and_keeps_its_own_mod_line_visible() {
        let src = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    fn test_a() {\n        assert!(true);\n    }\n}\n";
        let scope = REGISTRY.scope_index("f.rs", src);
        let lines: Vec<(LineKind, &str)> = src.lines().map(|l| (LineKind::Context, l)).collect();
        let model = model_of(&lines);
        let enabled = [FoldKind::Tests].into();
        let regions = compute_regions(&rows_of(&model), &model, &rules(&enabled, Some(&scope)));
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert!(
            !regions[0].lines.contains(&(0, 3)),
            "`mod tests {{` is the module's own header and stays visible"
        );
        assert_eq!(regions[0].label(4), "⋯ 4 lines · mod tests");
    }

    #[test]
    fn a_context_fold_names_the_function_it_sits_in() {
        let src = "fn outer() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n    changed();\n}\n";
        let scope = REGISTRY.scope_index("f.rs", src);
        let mut lines: Vec<(LineKind, &str)> =
            src.lines().map(|l| (LineKind::Context, l)).collect();
        lines[6].0 = LineKind::Added;
        let model = model_of(&lines);
        let enabled = [FoldKind::Context].into();
        let regions = compute_regions(&rows_of(&model), &model, &rules(&enabled, Some(&scope)));
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert_eq!(
            regions[0].lines.first(),
            Some(&(0, 1)),
            "the changed function's signature stays visible"
        );
        assert_eq!(regions[0].label(5), "⋯ 5 lines · fn outer");
    }

    #[test]
    fn a_fold_over_whole_definitions_names_the_first_and_counts_the_rest() {
        let src = "fn a() {\n}\nfn b() {\n}\nfn c() {\n}\nchanged();\n";
        let scope = REGISTRY.scope_index("f.rs", src);
        let mut lines: Vec<(LineKind, &str)> =
            src.lines().map(|l| (LineKind::Context, l)).collect();
        lines[6].0 = LineKind::Added;
        let model = model_of(&lines);
        let enabled = [FoldKind::Context].into();
        let regions = compute_regions(&rows_of(&model), &model, &rules(&enabled, Some(&scope)));
        assert_eq!(regions[0].label(6), "⋯ 6 lines · fn a +2 more");
    }

    #[test]
    fn a_name_that_merely_contains_test_is_not_a_test() {
        for name in ["test_parse", "testParse", "TestParse", "parse_test", "test"] {
            assert!(test_named(name), "{name}");
        }
        for name in [
            "latest_version",
            "attestation",
            "testament",
            "contest",
            "Testimony",
        ] {
            assert!(!test_named(name), "{name}");
        }
    }

    #[test]
    fn two_runs_opening_on_the_same_text_get_their_own_keys() {
        let mut lines = vec![(LineKind::Context, ""); 6];
        lines.push((LineKind::Added, "x"));
        lines.extend(vec![(LineKind::Context, ""); 6]);
        let model = model_of(&lines);
        let enabled = [FoldKind::Context].into();
        let regions = compute_regions(&rows_of(&model), &model, &rules(&enabled, None));
        assert_eq!(regions.len(), 2);
        assert_ne!(regions[0].key, regions[1].key);
    }

    #[test]
    fn a_key_outlives_its_run_growing_or_shrinking() {
        let long = numbered(&[LineKind::Context; 9]);
        let short = numbered(&[LineKind::Context; 6]);
        let enabled = [FoldKind::Context].into();
        let key = |model: &DiffModel| {
            compute_regions(&rows_of(model), model, &rules(&enabled, None))[0]
                .key
                .clone()
        };
        assert_eq!(key(&long), key(&short));
    }

    #[test]
    fn adjacent_regions_with_a_small_gap_merge_into_one_row() {
        let mut kinds = vec![LineKind::Deleted; 12];
        kinds.extend([LineKind::Context; 2]);
        kinds.extend([LineKind::Deleted; 12]);
        let model = numbered(&kinds);
        let rows = rows_of(&model);
        let enabled = [FoldKind::DeletedBodies].into();
        let regions = compute_regions(&rows, &model, &rules(&enabled, None));
        assert_eq!(regions.len(), 2);
        let (out_rows, _, groups) = folded(&model, &rows, &regions);
        assert_eq!(out_rows.len(), 1, "{out_rows:?}");
        assert_eq!(groups[0].keys.len(), 2);
        assert_eq!(groups[0].lines.len(), 26, "the gap lines hide with them");
        assert_eq!(groups[0].label, "⋯ 26 lines · 2 folded regions");
    }

    #[test]
    fn a_comment_inside_a_closed_region_stays_on_screen_between_two_fold_rows() {
        let model = numbered(&[LineKind::Context; 11]);
        let line_rows = rows_of(&model);
        let mut rows = line_rows[..5].to_vec();
        rows.push(DiffRow::Comment {
            comment: 0,
            line: 0,
            outdated: false,
        });
        rows.extend_from_slice(&line_rows[5..]);
        let enabled = [FoldKind::Context].into();
        let regions = compute_regions(&rows, &model, &rules(&enabled, None));
        assert_eq!(regions.len(), 1, "a comment row never splits a region");
        let (out_rows, _, _) = folded(&model, &rows, &regions);
        assert_eq!(fold_count(&out_rows), 2, "{out_rows:?}");
        assert!(matches!(out_rows[1], DiffRow::Comment { .. }));
    }

    #[test]
    fn a_region_holding_a_commented_line_starts_open() {
        let model = numbered(&[LineKind::Context; 8]);
        let enabled = [FoldKind::Context].into();
        let spans = [(false, 3, 3)];
        let rules = FoldRules {
            comment_spans: &spans,
            ..rules(&enabled, None)
        };
        let regions = compute_regions(&rows_of(&model), &model, &rules);
        assert!(!regions[0].starts_closed);
    }

    #[test]
    fn expanded_context_starts_open_and_a_walkthrough_folds_nothing_by_default() {
        let model = numbered(&[LineKind::Context; 8]);
        let enabled = [FoldKind::Context].into();
        let expanded = FoldRules {
            context_expanded: true,
            ..rules(&enabled, None)
        };
        assert!(!compute_regions(&rows_of(&model), &model, &expanded)[0].starts_closed);
        let slide = FoldRules {
            defaults: false,
            ..rules(&enabled, None)
        };
        assert!(!compute_regions(&rows_of(&model), &model, &slide)[0].starts_closed);
    }

    #[test]
    fn an_override_reopens_a_region_that_would_otherwise_start_closed() {
        let model = numbered(&[LineKind::Context; 6]);
        let rows = rows_of(&model);
        let enabled = [FoldKind::Context].into();
        let regions = compute_regions(&rows, &model, &rules(&enabled, None));
        let overrides = HashMap::from([(regions[0].key.clone(), false)]);
        let copy = vec![RowCopy::Text(String::new()); rows.len()];
        let (out_rows, _, _) = apply(&rows, &copy, &model, &regions, &overrides);
        assert_eq!(fold_count(&out_rows), 0);
    }

    #[test]
    fn a_fold_row_copies_the_code_it_hides() {
        let model = numbered(&[LineKind::Context; 5]);
        let rows = rows_of(&model);
        let enabled = [FoldKind::Context].into();
        let regions = compute_regions(&rows, &model, &rules(&enabled, None));
        let (_, copy, _) = folded(&model, &rows, &regions);
        assert_eq!(
            copy[0].text(),
            " line 1\n line 2\n line 3\n line 4\n line 5"
        );
    }

    #[test]
    fn split_mode_folds_each_closed_region_as_its_own_row() {
        let mut kinds = vec![LineKind::Deleted; 12];
        kinds.extend([LineKind::Context; 2]);
        kinds.extend([LineKind::Deleted; 12]);
        let model = numbered(&kinds);
        let enabled = [FoldKind::DeletedBodies].into();
        let regions = compute_regions(&rows_of(&model), &model, &rules(&enabled, None));
        let split: Vec<SplitRow> = (0..26)
            .map(|line| SplitRow::Pair {
                hunk: 0,
                left: Some(line),
                right: None,
            })
            .collect();
        let out = apply_split(split, &regions, &HashMap::new());
        let folds: Vec<usize> = out
            .iter()
            .filter_map(|row| match row {
                SplitRow::Fold { lines, .. } => Some(*lines),
                _ => None,
            })
            .collect();
        assert_eq!(folds, [12, 12], "{out:?}");
    }

    #[test]
    fn a_deletion_an_addition_replaces_never_folds() {
        let mut kinds = vec![LineKind::Deleted; 12];
        kinds.extend([LineKind::Added; 12]);
        let model = numbered(&kinds);
        assert!(compute_regions(&rows_of(&model), &model, &rules(&every_kind(), None)).is_empty());
    }
}

#[cfg(test)]
mod app_tests {
    use crossterm::event::KeyCode;
    use diffler_core::session::Anchor;

    use crate::app::App;
    use crate::app::diff::{DiffRow, SplitRow};
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, code_key, key};

    /// 30 lines with `changed` edited: at the default three lines of context
    /// the unchanged lines between two edits six apart sit in one hunk.
    fn edited(fixture: &Fixture, changed: &[usize]) {
        let content: String = (1..=30)
            .map(|i| {
                if changed.contains(&i) {
                    format!("LINE {i}\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        fixture.write("a.txt", &content);
    }

    fn fixture(changed: &[usize]) -> Fixture {
        let fixture = Fixture::new();
        edited(&fixture, &[]);
        fixture.commit_all("base");
        edited(&fixture, changed);
        fixture
    }

    fn open(fixture: &Fixture, path: &str) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_file(path);
        app.queue_enrich_selected();
        app.enrich_now();
        app.diff.as_mut().expect("diff").ensure_rows(&app.review);
        app
    }

    fn rows(app: &App) -> Vec<DiffRow> {
        app.diff.as_ref().expect("diff").rows().to_vec()
    }

    fn folds(app: &App) -> Vec<usize> {
        rows(app)
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, DiffRow::Fold { .. }))
            .map(|(index, _)| index)
            .collect()
    }

    /// The new-side number of every line on screen.
    fn shown(app: &App) -> Vec<u32> {
        let diff = app.diff.as_ref().expect("diff");
        let file = &diff.model(&app.review).files[diff.selected];
        rows(app)
            .iter()
            .filter_map(|row| match *row {
                DiffRow::Line { hunk, line, .. } => file.hunks[hunk].lines[line].new_no,
                _ => None,
            })
            .collect()
    }

    /// The row showing new-side line `no`.
    fn shown_at(app: &App, no: u32) -> usize {
        let diff = app.diff.as_ref().expect("diff");
        let file = &diff.model(&app.review).files[diff.selected];
        diff.rows()
            .iter()
            .position(|row| {
                matches!(*row, DiffRow::Line { hunk, line, .. }
                    if file.hunks[hunk].lines[line].new_no == Some(no))
            })
            .expect("the line is on screen")
    }

    fn cursor_line(app: &App) -> Option<u32> {
        let diff = app.diff.as_ref().expect("diff");
        let file = &diff.model(&app.review).files[diff.selected];
        match *diff.rows().get(diff.cursor)? {
            DiffRow::Line { hunk, line, .. } => file.hunks[hunk].lines[line].new_no,
            _ => None,
        }
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            app.handle(key(c));
        }
    }

    fn seat(app: &mut App, row: usize) {
        app.diff.as_mut().expect("diff").cursor = row;
    }

    #[test]
    fn za_opens_a_default_fold_and_closes_it_again_with_the_cursor_on_it() {
        let fixture = fixture(&[5, 12]);
        let mut app = open(&fixture, "a.txt");
        assert_eq!(folds(&app).len(), 1, "{:?}", rows(&app));
        assert!(!shown(&app).contains(&8));

        let fold = folds(&app)[0];
        seat(&mut app, fold);
        press(&mut app, "za");
        assert!(folds(&app).is_empty());
        assert!(shown(&app).contains(&8));

        let row = shown_at(&app, 8);
        seat(&mut app, row);
        press(&mut app, "za");
        assert_eq!(folds(&app), [app.diff.as_ref().expect("diff").cursor]);
    }

    #[test]
    fn z_r_opens_every_fold_and_z_m_restores_the_defaults() {
        let fixture = fixture(&[5, 11, 17]);
        let mut app = open(&fixture, "a.txt");
        assert_eq!(folds(&app).len(), 2);
        press(&mut app, "zR");
        assert!(folds(&app).is_empty());
        press(&mut app, "zM");
        assert_eq!(folds(&app).len(), 2);
    }

    #[test]
    fn an_opened_fold_stays_open_when_an_edit_changes_its_length() {
        let fixture = fixture(&[5, 12]);
        let mut app = open(&fixture, "a.txt");
        let fold = folds(&app)[0];
        seat(&mut app, fold);
        press(&mut app, "za");
        assert!(folds(&app).is_empty());

        edited(&fixture, &[5, 11, 12]);
        app.handle(AppEvent::RepoChanged);
        app.settle_refresh();
        app.diff.as_mut().expect("diff").ensure_rows(&app.review);
        assert!(folds(&app).is_empty(), "{:?}", rows(&app));
        assert!(shown(&app).contains(&8));
    }

    #[test]
    fn expanded_context_stays_open_until_folded_by_hand() {
        let fixture = fixture(&[15]);
        let mut app = open(&fixture, "a.txt");
        app.handle(key('='));
        assert!(folds(&app).is_empty(), "= shows the whole file");
        assert_eq!(shown(&app).len(), 30);

        seat(&mut app, 1);
        press(&mut app, "za");
        assert_eq!(folds(&app), [1]);
    }

    #[test]
    fn a_region_holding_a_comment_starts_open() {
        let fixture = fixture(&[5, 12]);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.review.session.add_comment(
            Anchor {
                file: "a.txt".to_owned(),
                line: Some(8),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "why this line",
        );
        app.open_working_tree_file("a.txt");
        assert!(folds(&app).is_empty(), "{:?}", rows(&app));
    }

    #[test]
    fn disabling_every_fold_kind_in_config_folds_nothing() {
        let fixture = fixture(&[5, 12]);
        let mut loaded = LoadedConfig::default();
        loaded.config.diff.default_folds = Vec::new();
        let mut app = App::new(fixture.review(), loaded);
        app.open_working_tree_file("a.txt");
        assert!(folds(&app).is_empty());
    }

    #[test]
    fn side_by_side_folds_the_same_lines_as_unified() {
        let fixture = fixture(&[5, 12]);
        let mut app = open(&fixture, "a.txt");
        app.handle(key('|'));
        let diff = app.diff.as_ref().expect("diff");
        let split: Vec<usize> = diff
            .split_rows
            .iter()
            .filter_map(|row| match row {
                SplitRow::Fold { lines, .. } => Some(*lines),
                _ => None,
            })
            .collect();
        assert_eq!(split, [6]);
    }

    /// Three functions, the middle one unchanged and hidden under the fold
    /// between the other two's edits.
    fn functions() -> Fixture {
        let base = "fn a() {\n    one();\n}\nfn b() {\n    two();\n    three();\n}\nfn c() {\n    four();\n}\n";
        let fixture = Fixture::new();
        fixture.write("f.rs", base);
        fixture.commit_all("base");
        fixture.write(
            "f.rs",
            &base.replace("one()", "ONE()").replace("four()", "FOUR()"),
        );
        fixture
    }

    #[test]
    fn a_fold_names_the_function_it_hides() {
        let fixture = functions();
        let app = open(&fixture, "f.rs");
        let diff = app.diff.as_ref().expect("diff");
        let labels: Vec<&str> = diff.fold_groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["⋯ 4 lines · fn b"], "{:?}", rows(&app));
    }

    #[test]
    fn a_function_motion_opens_the_fold_hiding_its_target() {
        let fixture = functions();
        let mut app = open(&fixture, "f.rs");
        seat(&mut app, 0);
        app.handle(key(')'));
        assert_eq!(cursor_line(&app), Some(1));
        app.handle(key(')'));
        assert_eq!(cursor_line(&app), Some(4), "{:?}", rows(&app));
        assert!(folds(&app).is_empty());
    }

    #[test]
    fn a_search_opens_the_fold_holding_its_match() {
        let fixture = functions();
        let mut app = open(&fixture, "f.rs");
        seat(&mut app, 0);
        press(&mut app, "/three");
        app.handle(code_key(KeyCode::Enter));
        assert_eq!(cursor_line(&app), Some(6), "{:?}", rows(&app));
        assert!(folds(&app).is_empty());
    }
}
