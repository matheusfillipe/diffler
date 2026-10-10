//! The symbol lens on the diff screen: `*` on a diff line labels every name on
//! it with a digit, tints each one's uses in its own colour, and `n`/`N` walk
//! them. A focused name lists its uses in the references sidebar.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use diffler_core::highlight::Highlighter;
use diffler_core::lens::{LensData, LensFile, LensOrigin, lens_files};
use diffler_core::model::{DiffModel, LineKind};
use diffler_core::review::Review;
use unicode_width::UnicodeWidthChar;

use super::{DiffRow, Pane};
use crate::app::{App, Flow};
use crate::config::FileLayout;

/// A lens the main loop should build off-thread.
#[derive(Debug, Clone)]
pub struct LensRequest {
    pub token: u64,
    pub origin: LensOrigin,
    pub files: Vec<LensFile>,
}

/// One line of a reference's preview, copied out of the hunk it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewLine {
    pub kind: LineKind,
    pub number: Option<u32>,
    pub text: String,
}

/// One use of the focused name, keyed by where it sits in the file so a
/// re-diff that moves hunks around still finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub path: String,
    pub on_old_side: bool,
    pub line: u32,
    pub range: Range<usize>,
    pub preview: Vec<PreviewLine>,
    /// Which preview line is the use's own.
    pub own: usize,
    /// On the first entry of each file, how many entries that file holds.
    pub group_len: Option<usize>,
}

/// What the reader has done with a built lens.
#[derive(Debug, Clone, Default)]
pub struct LensView {
    /// The one name the reader narrowed to, when they did.
    pub focus: Option<usize>,
    /// The focused name's uses, one per line, in diff order: what the
    /// references sidebar lists. Empty while no name is focused.
    pub refs: Vec<RefEntry>,
    /// The reference the sidebar has selected.
    pub ref_cursor: usize,
    /// The sidebar's first visible line, kept between frames.
    pub scroll: usize,
}

impl LensView {
    pub(crate) fn widen(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone)]
pub struct Lens {
    pub data: LensData,
    /// Indices into `data.uses`, by (path, old side, line number).
    by_line: HashMap<(String, bool, u32), Vec<usize>>,
    pub view: LensView,
}

impl Lens {
    fn new(data: LensData) -> Self {
        let mut by_line: HashMap<(String, bool, u32), Vec<usize>> = HashMap::new();
        for (at, found) in data.uses.iter().enumerate() {
            by_line
                .entry((found.path.clone(), found.on_old_side, found.line))
                .or_default()
                .push(at);
        }
        Self {
            data,
            by_line,
            view: LensView::default(),
        }
    }

    /// Whether `symbol`'s uses are tinted: all of them, or the focused one.
    pub(crate) fn shows(&self, symbol: usize) -> bool {
        self.view.focus.is_none_or(|focus| focus == symbol)
    }

    fn uses_on(
        &self,
        path: &str,
        on_old_side: bool,
        line: u32,
    ) -> impl Iterator<Item = &diffler_core::lens::LensUse> {
        self.by_line
            .get(&(path.to_owned(), on_old_side, line))
            .into_iter()
            .flatten()
            .filter_map(|&at| self.data.uses.get(at))
    }

    /// The tinted uses on one line, as byte ranges with their symbol.
    pub(crate) fn marks(
        &self,
        path: &str,
        on_old_side: bool,
        line: u32,
    ) -> Vec<(Range<usize>, usize)> {
        self.uses_on(path, on_old_side, line)
            .filter(|found| self.shows(found.symbol))
            .map(|found| (found.range.clone(), found.symbol))
            .collect()
    }

    /// On the line the lens was opened on, each name's digit over the first
    /// character of its first appearance, so a digit picks the name it sits on.
    pub(crate) fn labels(
        &self,
        path: &str,
        on_old_side: bool,
        line: u32,
        text: &str,
    ) -> Vec<(Range<usize>, char, usize)> {
        let origin = &self.data.origin;
        if origin.path != path || origin.on_old_side != on_old_side || origin.line != line {
            return Vec::new();
        }
        let mut labelled = HashSet::new();
        let mut out = Vec::new();
        for found in self.uses_on(path, on_old_side, line) {
            if !labelled.insert(found.symbol) {
                continue;
            }
            // a digit over a wide character would shift the rest of the line
            let Some(first) = text
                .get(found.range.start..)
                .and_then(|rest| rest.chars().next())
                .filter(|first| first.width() == Some(1))
            else {
                continue;
            };
            let Some(digit) = u32::try_from(found.symbol + 1)
                .ok()
                .and_then(|n| char::from_digit(n, 10))
            else {
                continue;
            };
            out.push((
                found.range.start..found.range.start + first.len_utf8(),
                digit,
                found.symbol,
            ));
        }
        out
    }

    /// `*` again on the same line: overview, then each name in turn, then
    /// back to the overview; `#` walks the same ring the other way.
    fn cycle_focus(&mut self, forward: bool) {
        let count = self.data.symbols.len();
        self.view.focus = match (self.view.focus, forward) {
            (_, _) if count == 0 => None,
            (None, true) => Some(0),
            (None, false) => Some(count - 1),
            (Some(at), true) if at + 1 < count => Some(at + 1),
            (Some(at), false) if at > 0 => Some(at - 1),
            _ => None,
        };
    }
}

/// Build the lens `request` asks for. Runs on the blocking pool, since it
/// parses both sides of every file in the diff.
pub fn compute_lens(highlighter: &Highlighter, request: &LensRequest) -> Lens {
    Lens::new(diffler_core::lens::compute(
        highlighter,
        &request.origin,
        &request.files,
    ))
}

/// Hunk lines a reference's preview shows on each side of the use.
const REF_CONTEXT: usize = 1;

/// Rough lines one reference takes in the sidebar: its preview and a gap.
const REF_LINES: usize = 2 * REF_CONTEXT + 2;

impl App {
    /// `*` (`forward`) or `#`: open the lens on the cursor's line, or on the
    /// line it is already open on, narrow it to the next or previous name.
    pub(crate) fn symbol_lens(&mut self, forward: bool) {
        if self.diff.as_ref().is_some_and(|diff| diff.side_by_side) {
            self.info("switch to the unified view (|) to find references");
            return;
        }
        let Some(origin) = self.lens_origin_at_cursor() else {
            self.info("move onto a code line to find references");
            return;
        };
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if let Some(lens) = diff.lens.as_mut()
            && lens.data.origin == origin
        {
            lens.cycle_focus(forward);
            self.lens_focus_changed();
            return;
        }
        let files = lens_files(&diff.model_for_rows(&self.review));
        self.lens_token = self.lens_token.wrapping_add(1);
        diff.lens_wanted = Some(self.lens_token);
        self.pending_lens = Some(LensRequest {
            token: self.lens_token,
            origin,
            files,
        });
    }

    fn lens_origin_at_cursor(&self) -> Option<LensOrigin> {
        let diff = self.diff.as_ref()?;
        let DiffRow::Line { file, hunk, line } = *diff.rows().get(diff.cursor)? else {
            return None;
        };
        let model = diff.model_for_rows(&self.review);
        let file = model.files.get(file)?;
        let line = file.hunks.get(hunk)?.lines.get(line)?;
        let on_old_side = line.kind == LineKind::Deleted;
        Some(LensOrigin {
            path: file.path.clone(),
            on_old_side,
            line: line.number_on(on_old_side)?,
        })
    }

    /// Install a built lens on the view that asked for it, dropping one the
    /// reader has moved past or a view that has since been replaced.
    pub(crate) fn on_lens(&mut self, token: u64, lens: Lens) -> Flow {
        let Some(diff) = self
            .diff
            .as_mut()
            .filter(|diff| diff.lens_wanted == Some(token))
        else {
            return Flow::Idle;
        };
        diff.lens_wanted = None;
        if lens.data.symbols.is_empty() {
            self.info("move onto a line with a name on it to find references");
        } else {
            diff.lens = Some(lens);
            diff.settle_focus();
        }
        Flow::Continue
    }

    /// Whether the open diff shows a lens, which digits and `esc` then reach.
    /// Side-by-side draws no lens, so there it counts as closed.
    pub(crate) fn lens_active(&self) -> bool {
        self.diff
            .as_ref()
            .is_some_and(|diff| diff.lens.is_some() && !diff.side_by_side)
    }

    /// A digit on a label: narrow the lens to that name, or widen it back
    /// when it is already the one in focus.
    pub(crate) fn lens_focus(&mut self, symbol: usize) {
        let Some(lens) = self.diff.as_mut().and_then(|diff| diff.lens.as_mut()) else {
            return;
        };
        if symbol < lens.data.symbols.len() {
            lens.view.focus = (lens.view.focus != Some(symbol)).then_some(symbol);
            self.lens_focus_changed();
        }
    }

    /// A focused name brings up its references sidebar in the comments
    /// sidebar's place; widening back to every name puts it away.
    fn lens_focus_changed(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.order_refs(review);
        if diff.refs_visible() {
            diff.comments_open = false;
        }
        diff.settle_focus();
    }

    pub(crate) fn lens_clear(&mut self) {
        if let Some(diff) = self.diff.as_mut() {
            diff.drop_lens();
        }
    }

    /// Move the references sidebar's selection by `delta`, wrapping past
    /// either end when `wrap` (`n`/`N`) and stopping there otherwise (`j`/`k`),
    /// and seat the diff on the reference it lands on.
    pub(crate) fn refs_step(&mut self, delta: isize, wrap: bool) {
        let Some(lens) = self.diff.as_ref().and_then(|diff| diff.lens.as_ref()) else {
            return;
        };
        let count = lens.view.refs.len();
        if count == 0 {
            return;
        }
        let at = lens.view.ref_cursor;
        let target = if wrap {
            let count = isize::try_from(count).unwrap_or(isize::MAX);
            let at = isize::try_from(at).unwrap_or(0);
            usize::try_from((at + delta).rem_euclid(count)).unwrap_or(0)
        } else {
            at.saturating_add_signed(delta)
        };
        self.refs_to(target);
    }

    /// Select reference `index`, clamped to the list, and seat the diff on it.
    pub(crate) fn refs_to(&mut self, index: usize) {
        let Some(lens) = self.diff.as_mut().and_then(|diff| diff.lens.as_mut()) else {
            return;
        };
        lens.view.ref_cursor = index.min(lens.view.refs.len().saturating_sub(1));
        self.seat_ref();
    }

    /// `]`/`[` in the references sidebar: the first reference of the next or
    /// previous file.
    pub(crate) fn refs_jump_file(&mut self, forward: bool) {
        let Some(lens) = self.diff.as_ref().and_then(|diff| diff.lens.as_ref()) else {
            return;
        };
        let at = lens.view.ref_cursor;
        let starts: Vec<usize> = lens
            .view
            .refs
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.group_len.is_some())
            .map(|(index, _)| index)
            .collect();
        let target = if forward {
            starts.iter().find(|&&index| index > at)
        } else {
            let own = starts.iter().rev().find(|&&index| index <= at).copied();
            starts
                .iter()
                .rev()
                .find(|&&index| own.is_some_and(|own| index < own))
        }
        .copied();
        if let Some(target) = target {
            self.refs_to(target);
        }
    }

    /// How many references a page of the sidebar moves.
    pub(crate) fn refs_page(&self, full: bool) -> isize {
        let height = self
            .diff
            .as_ref()
            .map_or(0, |diff| usize::from(diff.comments_rect.height));
        let page = (height / REF_LINES / if full { 1 } else { 2 }).max(1);
        isize::try_from(page).unwrap_or(1)
    }

    /// Whether a column falls in the references sidebar.
    pub(crate) fn refs_col(&self, col: u16) -> bool {
        self.diff
            .as_ref()
            .is_some_and(|diff| diff.refs_visible() && col >= diff.comments_rect.x)
    }

    /// The reference a click lands on, through the last render's line table.
    pub(crate) fn refs_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        if !diff.refs_visible() {
            return None;
        }
        let rect = diff.comments_rect;
        if col < rect.x || row < rect.y || row >= rect.y.saturating_add(rect.height) {
            return None;
        }
        let lens = diff.lens.as_ref()?;
        let line = usize::from(row - rect.y) + lens.view.scroll;
        diff.ref_lines.get(line).copied().flatten()
    }

    /// Put the diff cursor on the selected reference, moving to its file and
    /// opening the fold that hides it when it has to.
    pub(crate) fn seat_ref(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let Some(entry) = diff
            .lens
            .as_ref()
            .and_then(|lens| lens.view.refs.get(lens.view.ref_cursor))
        else {
            return;
        };
        let (path, on_old_side, line) = (entry.path.clone(), entry.on_old_side, entry.line);
        diff.seat_line(review, &path, on_old_side, line);
    }

    /// `n`/`N` with a lens up: the next or previous use of what it shows, in
    /// diff order, moving to the next file that has one past the last.
    pub(crate) fn lens_step(&mut self, forward: bool) {
        let Some(lens) = self.diff.as_ref().and_then(|diff| diff.lens.as_ref()) else {
            return;
        };
        if lens.view.focus.is_some() {
            return self.refs_step(if forward { 1 } else { -1 }, true);
        }
        let mut wanted: HashMap<String, HashSet<(bool, u32)>> = HashMap::new();
        for found in &lens.data.uses {
            if lens.shows(found.symbol) {
                wanted
                    .entry(found.path.clone())
                    .or_default()
                    .insert((found.on_old_side, found.line));
            }
        }
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let paths: Vec<String> = diff
            .model_for_rows(review)
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect();
        let crosses_files = diff.layout != FileLayout::Walkthrough;
        let start = diff.selected;
        let count = paths.len().max(1);
        for step in 0..=count {
            let at = if forward {
                (start + step) % count
            } else {
                (start + count - step % count) % count
            };
            if step > 0 && !crosses_files && at != start {
                continue;
            }
            let Some(lines) = paths.get(at).and_then(|path| wanted.get(path)) else {
                continue;
            };
            if at != diff.selected {
                diff.select(at, review);
                diff.reveal_selected(review);
            }
            let mut rows = use_rows(diff, review, lines);
            rows.sort_unstable_by_key(|(row, _)| *row);
            let target = match (step, forward) {
                (0, true) => rows.iter().find(|(row, _)| *row > diff.cursor),
                (0, false) => rows.iter().rev().find(|(row, _)| *row < diff.cursor),
                (_, true) => rows.first(),
                (_, false) => rows.last(),
            };
            if let Some(&(_, position)) = target {
                diff.reveal_line(review, position);
                return;
            }
        }
        self.info("no other use of this name in the diff");
    }

    /// Run the queued lens inline, the way the runtime's worker does.
    #[cfg(test)]
    pub(crate) fn settle_lens(&mut self) {
        if let Some(request) = self.pending_lens.take() {
            let lens = compute_lens(&self.highlighter, &request);
            self.on_lens(request.token, lens);
        }
    }
}

impl super::DiffView {
    /// Whether the references sidebar shows: a name is focused, in the
    /// unified view.
    pub(crate) fn refs_visible(&self) -> bool {
        !self.side_by_side
            && self
                .lens
                .as_ref()
                .is_some_and(|lens| lens.view.focus.is_some())
    }

    /// Take the lens away, along with any answer still on its way.
    pub(crate) fn drop_lens(&mut self) {
        self.lens = None;
        self.lens_wanted = None;
        self.settle_focus();
    }

    /// Hand the keyboard back to the diff when the sidebar holding it is
    /// hidden.
    pub(crate) fn settle_focus(&mut self) {
        let hidden = match self.focus {
            Pane::References => !self.refs_visible(),
            Pane::Comments => !self.comments_open,
            Pane::List | Pane::Diff => false,
        };
        if hidden {
            self.focus = Pane::Diff;
        }
    }

    /// List the focused name's uses in diff order, one per line, and select
    /// the one on the line the lens was opened on. In the walkthrough layout
    /// it lists the slide's own file.
    fn order_refs(&mut self, review: &Review) {
        let refs = self.lens.as_ref().map_or_else(Vec::new, |lens| {
            let model = self.model_for_rows(review);
            collect_refs(
                lens,
                &model,
                (self.layout == FileLayout::Walkthrough).then_some(self.selected),
            )
        });
        let Some(lens) = self.lens.as_mut() else {
            return;
        };
        let origin = &lens.data.origin;
        lens.view.ref_cursor = refs
            .iter()
            .position(|entry| {
                entry.path == origin.path
                    && entry.on_old_side == origin.on_old_side
                    && entry.line == origin.line
            })
            .unwrap_or(0);
        lens.view.refs = refs;
        lens.view.scroll = 0;
    }
}

/// The focused name's uses in `model`'s order, each with its preview, the
/// first of each file counting the file's own. `only` limits it to one file.
fn collect_refs(lens: &Lens, model: &DiffModel, only: Option<usize>) -> Vec<RefEntry> {
    let Some(symbol) = lens.view.focus else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    for (file_at, file) in model.files.iter().enumerate() {
        if only.is_some_and(|only| only != file_at) {
            continue;
        }
        let first = refs.len();
        for hunk in &file.hunks {
            for (line_at, line) in hunk.lines.iter().enumerate() {
                let on_old_side = line.kind == LineKind::Deleted;
                let Some(number) = line.number_on(on_old_side) else {
                    continue;
                };
                let Some(found) = lens
                    .uses_on(&file.path, on_old_side, number)
                    .find(|found| found.symbol == symbol)
                else {
                    continue;
                };
                let from = line_at.saturating_sub(REF_CONTEXT);
                let to = (line_at + REF_CONTEXT + 1).min(hunk.lines.len());
                refs.push(RefEntry {
                    path: file.path.clone(),
                    on_old_side,
                    line: number,
                    range: found.range.clone(),
                    preview: hunk
                        .lines
                        .get(from..to)
                        .unwrap_or_default()
                        .iter()
                        .map(|shown| PreviewLine {
                            kind: shown.kind,
                            number: shown.number_on(shown.kind == LineKind::Deleted),
                            text: shown.text.clone(),
                        })
                        .collect(),
                    own: line_at - from,
                    group_len: None,
                });
            }
        }
        let count = refs.len() - first;
        if let Some(entry) = refs.get_mut(first) {
            entry.group_len = Some(count);
        }
    }
    refs
}

/// The pane rows of the selected file's lines in `wanted` (side, number),
/// each with its model position (hunk, line). A line a fold hides answers
/// with the fold's own row, which `reveal_line` then opens.
fn use_rows(
    diff: &super::DiffView,
    review: &Review,
    wanted: &HashSet<(bool, u32)>,
) -> Vec<(usize, (usize, usize))> {
    let model = diff.model_for_rows(review);
    let Some(file) = model.files.get(diff.selected) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (hunk_at, hunk) in file.hunks.iter().enumerate() {
        for (line_at, line) in hunk.lines.iter().enumerate() {
            let on_old_side = line.kind == LineKind::Deleted;
            let shown = line
                .number_on(on_old_side)
                .is_some_and(|number| wanted.contains(&(on_old_side, number)));
            if let Some(row) = shown
                .then(|| diff.row_of_line((hunk_at, line_at)))
                .flatten()
            {
                out.push((row, (hunk_at, line_at)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, key};
    use diffler_core::lens::Reach;

    /// `apply` gains a parameter in `src/lib.rs`, and its caller in
    /// `src/main.rs` passes it; `other` has a `price` of its own.
    fn two_files() -> (Fixture, App) {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            "pub fn apply(price: u32, qty: u32) -> u32 {\n    price * qty\n}\n\npub fn other(price: u32) -> u32 {\n    price + 1\n}\n",
        );
        fixture.write(
            "src/main.rs",
            "use crate::apply;\n\npub fn run() -> u32 {\n    apply(2, 3)\n}\n",
        );
        fixture.commit_all("base");
        fixture.write(
            "src/lib.rs",
            "pub fn apply(price: u32, qty: u32, discount: u32) -> u32 {\n    let total = price * qty;\n    total - discount.min(total)\n}\n\npub fn other(price: u32) -> u32 {\n    price + 2\n}\n",
        );
        fixture.write(
            "src/main.rs",
            "use crate::apply;\n\npub fn run() -> u32 {\n    apply(2, 3, 1)\n}\n",
        );
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        (fixture, app)
    }

    /// Put the cursor on `path`'s new-side line `line` and press `*`.
    fn lens_on(app: &mut App, path: &str, line: u32) {
        let review = &app.review;
        let diff = app.diff.as_mut().expect("diff");
        let index = diff
            .model(review)
            .files
            .iter()
            .position(|file| file.path == path)
            .expect("the file is in the diff");
        diff.select(index, review);
        let model = diff.model_for_rows(review);
        let file = &model.files[index];
        let row = diff
            .rows()
            .iter()
            .position(|row| {
                let DiffRow::Line { hunk, line: at, .. } = *row else {
                    return false;
                };
                let found = &file.hunks[hunk].lines[at];
                found.kind != LineKind::Deleted && found.new_no == Some(line)
            })
            .expect("the line is on screen");
        drop(model);
        diff.cursor = row;
        diff.focus = crate::app::Pane::Diff;
        app.handle(key('*'));
        app.settle_lens();
    }

    fn lens(app: &App) -> &Lens {
        app.diff
            .as_ref()
            .and_then(|diff| diff.lens.as_ref())
            .expect("a lens")
    }

    #[test]
    fn star_names_the_lines_symbols_with_how_far_each_reaches() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        let lens = lens(&app);
        let names: Vec<&str> = lens.data.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["apply", "price", "qty", "discount"]);
        assert_eq!(
            lens.data.symbols[0].reach,
            Reach::Diff,
            "the diff defines apply"
        );
        assert_eq!(
            lens.data.symbols[1].reach,
            Reach::Function("apply".to_owned())
        );
        let uses = |symbol: usize| lens.data.uses.iter().filter(|u| u.symbol == symbol).count();
        let files: HashSet<&str> = lens
            .data
            .uses
            .iter()
            .filter(|u| u.symbol == 0)
            .map(|u| u.path.as_str())
            .collect();
        assert_eq!(uses(0), 5, "both sides of both files");
        assert_eq!(files.len(), 2);
        assert_eq!(uses(1), 4, "other's own price is not this one");
    }

    #[test]
    fn a_digit_narrows_the_lens_and_n_walks_that_names_uses() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('4'));
        let lens = lens(&app);
        assert_eq!(lens.view.focus, Some(3), "discount");
        assert!(
            lens.marks("src/lib.rs", false, 2).is_empty(),
            "price is faded out"
        );

        app.handle(key('n'));
        let diff = app.diff.as_ref().expect("diff");
        let DiffRow::Line { hunk, line, .. } = diff.rows()[diff.cursor] else {
            panic!("n lands on a code line");
        };
        let file = &diff.model_for_rows(&app.review).files[diff.selected];
        assert_eq!(
            file.hunks[hunk].lines[line].new_no,
            Some(3),
            "discount's next use"
        );
    }

    #[test]
    fn n_crosses_into_the_next_file_for_a_name_the_diff_defines() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('1'));
        let start = app.diff.as_ref().expect("diff").selected;
        app.handle(key('n'));
        let diff = app.diff.as_ref().expect("diff");
        assert_ne!(
            diff.selected, start,
            "apply's next use is in the other file"
        );
        assert_eq!(
            diff.model_for_rows(&app.review).files[diff.selected].path,
            "src/main.rs"
        );
    }

    #[test]
    fn star_again_cycles_the_focus_and_esc_closes_the_lens() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('*'));
        assert_eq!(lens(&app).view.focus, Some(0));
        app.handle(key('*'));
        assert_eq!(lens(&app).view.focus, Some(1));
        app.handle(AppEvent::Key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        )));
        assert!(!app.lens_active());
    }

    #[test]
    fn hash_cycles_the_focus_backwards() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('#'));
        assert_eq!(lens(&app).view.focus, Some(3), "from all names to the last");
        app.handle(key('#'));
        assert_eq!(lens(&app).view.focus, Some(2));
    }

    #[test]
    fn star_after_esc_opens_the_lens_again() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(AppEvent::Key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        )));
        assert!(!app.lens_active());
        app.handle(key('*'));
        app.settle_lens();
        assert!(app.lens_active(), "{:?}", app.message);
    }

    /// Each name carries its digit on the line the lens opened on, and its
    /// uses take its colour as their background.
    #[test]
    fn each_name_carries_its_digit_and_its_uses_wear_its_colour() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        let terminal = crate::test_support::render(&mut app);
        let buffer = terminal.backend().buffer();
        let row_text = |y: u16| -> String {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_owned())
                .collect()
        };
        let y = (1..buffer.area.height)
            .find(|&y| row_text(y).contains("iscount: u32) -> u32 {"))
            .expect("the new signature is on screen");
        let text = row_text(y);
        let column = |word: &str| {
            let byte = text.find(word).expect("the word is on the row");
            u16::try_from(text[..byte].chars().count()).expect("fits a row")
        };
        assert_ne!(
            buffer[(column("rice"), y)].bg,
            buffer[(column("pub"), y)].bg,
            "a use of price wears a tint the rest of the line does not"
        );
        assert!(
            text.contains("pub fn 1pply(2rice: u32, 3ty: u32, 4iscount"),
            "each name carries its digit on its first character: {text}"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn a_digit_opens_the_references_with_the_keyboard_still_in_the_diff() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('1'));
        let diff = app.diff.as_ref().expect("diff");
        assert!(diff.refs_visible());
        assert_eq!(diff.focus, crate::app::Pane::Diff);
        let lens = lens(&app);
        assert_eq!(lens.view.refs.len(), 5, "one per line apply is used on");
        let selected = &lens.view.refs[lens.view.ref_cursor];
        assert_eq!((selected.path.as_str(), selected.line), ("src/lib.rs", 1));
        assert!(!selected.on_old_side, "the line the lens was opened on");
    }

    #[test]
    fn walking_the_references_moves_the_diff_with_them() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('1'));
        app.handle(key('l'));
        assert_eq!(
            app.diff.as_ref().expect("diff").focus,
            crate::app::Pane::References
        );
        app.handle(key('j'));
        let diff = app.diff.as_ref().expect("diff");
        let model = diff.model_for_rows(&app.review);
        assert_eq!(model.files[diff.selected].path, "src/main.rs");
        let DiffRow::Line { hunk, line, .. } = diff.rows()[diff.cursor] else {
            panic!("the diff cursor sits on the reference");
        };
        assert!(
            model.files[diff.selected].hunks[hunk].lines[line]
                .text
                .contains("apply")
        );
    }

    #[test]
    fn the_comments_sidebar_puts_the_references_away() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('1'));
        app.handle(key('C'));
        let diff = app.diff.as_ref().expect("diff");
        assert!(!diff.refs_visible());
        assert!(diff.comments_open);
        assert!(app.lens_active(), "the labels stay; only the focus widens");
    }

    #[test]
    fn the_references_sidebar_previews_each_use() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('1'));
        insta::assert_snapshot!(crate::test_support::render(&mut app).backend());
    }

    /// A call reaches across the diff even when the function it calls is
    /// defined in a file the diff does not touch.
    #[test]
    fn a_call_links_every_changed_caller_of_a_function_the_diff_leaves_alone() {
        let fixture = Fixture::new();
        fixture.write(
            "src/util.rs",
            "pub fn log_it(n: u32) {}
",
        );
        fixture.write(
            "src/a.rs",
            "fn a() {
    log_it(1);
}
",
        );
        fixture.write(
            "src/b.rs",
            "fn b() {
    log_it(2);
}
",
        );
        fixture.commit_all("base");
        fixture.write(
            "src/a.rs",
            "fn a() {
    log_it(10);
}
",
        );
        fixture.write(
            "src/b.rs",
            "fn b() {
    log_it(20);
}
",
        );
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        lens_on(&mut app, "src/a.rs", 2);
        let lens = lens(&app);
        assert_eq!(lens.data.symbols[0].name, "log_it");
        assert_eq!(lens.data.symbols[0].reach, Reach::Diff);
        let files: HashSet<&str> = lens
            .data
            .uses
            .iter()
            .filter(|u| u.symbol == 0)
            .map(|u| u.path.as_str())
            .collect();
        assert_eq!(files, HashSet::from(["src/a.rs", "src/b.rs"]));
    }

    #[test]
    fn a_line_with_no_names_says_so() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 5);
        assert!(!app.lens_active());
        assert_eq!(
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("move onto a line with a name on it to find references")
        );
    }

    #[test]
    fn a_focus_left_on_a_hidden_sidebar_goes_back_to_the_diff() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.handle(key('1'));
        app.handle(key('l'));
        app.handle(key('|'));
        assert_eq!(
            app.diff.as_ref().expect("diff").focus,
            crate::app::Pane::Diff,
            "side-by-side hides the references"
        );
    }

    #[test]
    fn a_lens_answered_after_the_view_moved_on_is_dropped() {
        let (_fixture, mut app) = two_files();
        lens_on(&mut app, "src/lib.rs", 1);
        app.lens_clear();
        app.handle(key('*'));
        let request = app.pending_lens.take().expect("a request");
        app.open_working_tree_diff(None);
        assert_eq!(
            app.on_lens(request.token, compute_lens(&app.highlighter, &request)),
            Flow::Idle
        );
        assert!(!app.lens_active(), "the new view never asked for it");
    }

    /// Expanding the context after the references are listed shifts every
    /// line of the hunk, and the selection still lands on its own use.
    #[test]
    fn a_reference_is_found_again_after_the_context_grows() {
        let fixture = Fixture::new();
        let filler = "// filler\n".repeat(10);
        fixture.write(
            "src/lib.rs",
            &format!("{filler}fn f(a: u32) -> u32 {{\n    a + 1\n}}\n"),
        );
        fixture.commit_all("base");
        fixture.write(
            "src/lib.rs",
            &format!("{filler}fn f(a: u32) -> u32 {{\n    a + 2\n}}\n"),
        );
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        lens_on(&mut app, "src/lib.rs", 12);
        app.handle(key('1'));
        app.handle(key('='));
        app.handle(key('l'));
        app.handle(key('g'));
        app.handle(key('g'));
        let diff = app.diff.as_ref().expect("diff");
        let lens = lens(&app);
        let entry = &lens.view.refs[lens.view.ref_cursor];
        assert_eq!(entry.line, 11, "a's definition");
        let DiffRow::Line { hunk, line, .. } = diff.rows()[diff.cursor] else {
            panic!("the diff cursor sits on a code line");
        };
        let model = diff.model_for_rows(&app.review);
        assert_eq!(
            model.files[diff.selected].hunks[hunk].lines[line].new_no,
            Some(11)
        );
    }
}
