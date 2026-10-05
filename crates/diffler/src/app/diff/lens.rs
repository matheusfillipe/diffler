//! The symbol lens: `*` on a diff line names every symbol on it, tints each
//! one's uses in its own colour, and `n`/`N` walk them. A local name reaches
//! as far as its enclosing function; a function, method or type the diff
//! defines reaches across every file of the diff. The names come from the
//! parse tree, so a word inside a string or a comment is not one of them.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use diffler_core::model::{DiffModel, LineKind};
use diffler_core::syntax::registry::REGISTRY;
use diffler_core::syntax::{Ident, ScopeIndex};

use super::{DiffRow, Pane};
use crate::app::{App, Flow};
use crate::config::FileLayout;

/// The most symbols the strip lists for one line, one per digit key.
pub const MAX_SYMBOLS: usize = 9;

/// The diff line the lens was opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensOrigin {
    pub path: String,
    pub on_old_side: bool,
    pub line: u32,
}

/// One file of the diff, as the worker reads it: both sides' text and the
/// line numbers the diff shows on each, a deleted line on the old side and an
/// added or context line on the new one, so a context line counts once.
#[derive(Debug, Clone)]
pub struct LensFile {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub old_lines: HashSet<u32>,
    pub new_lines: HashSet<u32>,
}

/// A lens the main loop should build off-thread.
#[derive(Debug, Clone)]
pub struct LensRequest {
    pub token: u64,
    pub origin: LensOrigin,
    pub files: Vec<LensFile>,
}

/// How far a symbol's uses are looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// A local name, inside the function named here.
    Function(String),
    /// A name at the top level of its file, outside any function.
    File,
    /// A function, method or type: a definition, a call or a type position,
    /// linked across every file of the diff.
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensSymbol {
    pub name: String,
    pub reach: Reach,
}

/// One use of a symbol on a line the diff shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensUse {
    pub symbol: usize,
    pub path: String,
    pub on_old_side: bool,
    pub line: u32,
    pub range: Range<usize>,
}

/// One use of the focused symbol, placed in the diff: its file and its line
/// in that file's hunks, in the order the diff reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub use_index: usize,
    pub file: usize,
    pub hunk: usize,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Lens {
    pub origin: LensOrigin,
    pub symbols: Vec<LensSymbol>,
    pub uses: Vec<LensUse>,
    /// The one symbol the reader narrowed to, when they did.
    pub focus: Option<usize>,
    /// The focused symbol's uses, one per line, in diff order: what the
    /// references sidebar lists. Empty while no symbol is focused.
    pub refs: Vec<RefEntry>,
    /// The reference the sidebar has selected.
    pub ref_cursor: usize,
}

impl Lens {
    /// Whether `symbol`'s uses are tinted: all of them, or the focused one.
    pub(crate) fn shows(&self, symbol: usize) -> bool {
        self.focus.is_none_or(|focus| focus == symbol)
    }

    /// The tinted uses on one line, as byte ranges with their symbol.
    pub(crate) fn marks(
        &self,
        path: &str,
        on_old_side: bool,
        line: u32,
    ) -> Vec<(Range<usize>, usize)> {
        self.uses
            .iter()
            .filter(|found| {
                self.shows(found.symbol)
                    && found.on_old_side == on_old_side
                    && found.line == line
                    && found.path == path
            })
            .map(|found| (found.range.clone(), found.symbol))
            .collect()
    }

    /// On the line the lens was opened on, each symbol's digit over the first
    /// character of the name's first appearance, so the strip's numbers read
    /// straight onto the code.
    pub(crate) fn labels(
        &self,
        path: &str,
        on_old_side: bool,
        line: u32,
        text: &str,
    ) -> Vec<(Range<usize>, char, usize)> {
        let origin = &self.origin;
        if origin.path != path || origin.on_old_side != on_old_side || origin.line != line {
            return Vec::new();
        }
        let mut labelled = HashSet::new();
        let mut out = Vec::new();
        for found in &self.uses {
            let here = found.path == path && found.on_old_side == on_old_side && found.line == line;
            if !here || !labelled.insert(found.symbol) {
                continue;
            }
            let Some(first) = text
                .get(found.range.start..)
                .and_then(|rest| rest.chars().next())
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

    /// `*` again on the same line: overview, then each symbol in turn, then
    /// back to the overview; `#` walks the same ring the other way.
    fn cycle_focus(&mut self, forward: bool) {
        let count = self.symbols.len();
        self.focus = match (self.focus, forward) {
            (_, _) if count == 0 => None,
            (None, true) => Some(0),
            (None, false) => Some(count - 1),
            (Some(at), true) if at + 1 < count => Some(at + 1),
            (Some(at), false) if at > 0 => Some(at - 1),
            _ => None,
        };
    }
}

/// One side of one file, parsed once for both its names and its scopes.
struct Side {
    idents: Vec<Ident>,
    scope: ScopeIndex,
}

impl Side {
    fn read(path: &str, text: Option<&str>) -> Option<Self> {
        let text = text?;
        Some(Self {
            idents: REGISTRY.identifiers(path, text),
            scope: REGISTRY.scope_index(path, text),
        })
    }
}

/// Build the lens `request` asks for. Runs on the blocking pool, since it
/// parses both sides of every file in the diff.
pub fn compute_lens(request: &LensRequest) -> Lens {
    let origin = &request.origin;
    let sides: Vec<(Option<Side>, Option<Side>)> = request
        .files
        .iter()
        .map(|file| {
            (
                Side::read(&file.path, file.old_text.as_deref()),
                Side::read(&file.path, file.new_text.as_deref()),
            )
        })
        .collect();
    let origin_side = request
        .files
        .iter()
        .position(|file| file.path == origin.path)
        .and_then(|at| sides.get(at))
        .and_then(|(old, new)| {
            if origin.on_old_side {
                old.as_ref()
            } else {
                new.as_ref()
            }
        });
    let symbols = line_symbols(origin, origin_side);
    let uses = symbol_uses(request, &sides, &symbols);
    Lens {
        origin: origin.clone(),
        symbols,
        uses,
        focus: None,
        refs: Vec::new(),
        ref_cursor: 0,
    }
}

/// The names on the origin line, first appearance first, each with how far
/// its uses are looked for.
fn line_symbols(origin: &LensOrigin, origin_side: Option<&Side>) -> Vec<LensSymbol> {
    let origin_row = origin.line.saturating_sub(1) as usize;
    let mut names: Vec<String> = Vec::new();
    let mut items: HashSet<&str> = HashSet::new();
    for ident in origin_side.map_or(&[][..], |side| side.idents.as_slice()) {
        if ident.line != origin_row {
            continue;
        }
        if ident.item {
            items.insert(ident.name.as_str());
        }
        if !names.contains(&ident.name) && names.len() < MAX_SYMBOLS {
            names.push(ident.name.clone());
        }
    }
    let enclosing = origin_side.and_then(|side| side.scope.enclosing(origin_row));
    names
        .into_iter()
        .map(|name| {
            // a local that only shares its name with some function elsewhere
            // stays local: only a call, a definition or a type reaches out
            let reach = if items.contains(name.as_str()) {
                Reach::Diff
            } else if let Some((function, _, _)) = enclosing {
                Reach::Function(function.to_owned())
            } else {
                Reach::File
            };
            LensSymbol { name, reach }
        })
        .collect()
}

/// Every use of `symbols` on a line the diff shows, within each one's reach.
fn symbol_uses(
    request: &LensRequest,
    sides: &[(Option<Side>, Option<Side>)],
    symbols: &[LensSymbol],
) -> Vec<LensUse> {
    let origin = &request.origin;
    let index: HashMap<&str, usize> = symbols
        .iter()
        .enumerate()
        .map(|(at, symbol)| (symbol.name.as_str(), at))
        .collect();
    let mut uses = Vec::new();
    for (file, (old, new)) in request.files.iter().zip(sides) {
        for (on_old_side, side, shown) in
            [(true, old, &file.old_lines), (false, new, &file.new_lines)]
        {
            let Some(side) = side else {
                continue;
            };
            for ident in &side.idents {
                let line = u32::try_from(ident.line + 1).unwrap_or(u32::MAX);
                let Some(&symbol) = index.get(ident.name.as_str()) else {
                    continue;
                };
                if !shown.contains(&line) {
                    continue;
                }
                let reaches = match symbols.get(symbol).map(|s| &s.reach) {
                    Some(Reach::Diff) => true,
                    Some(Reach::File) => file.path == origin.path,
                    Some(Reach::Function(function)) => {
                        file.path == origin.path
                            && side
                                .scope
                                .def_span(function)
                                .is_some_and(|(start, end)| (start..=end).contains(&ident.line))
                    }
                    None => false,
                };
                if reaches {
                    uses.push(LensUse {
                        symbol,
                        path: file.path.clone(),
                        on_old_side,
                        line,
                        range: ident.range.clone(),
                    });
                }
            }
        }
    }
    uses
}

/// The files of `model` as the lens worker reads them.
fn lens_files(model: &DiffModel) -> Vec<LensFile> {
    model
        .files
        .iter()
        .filter(|file| !file.binary)
        .map(|file| {
            let mut old_lines = HashSet::new();
            let mut new_lines = HashSet::new();
            for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
                match (line.kind, line.old_no, line.new_no) {
                    (LineKind::Deleted, Some(old), _) => {
                        old_lines.insert(old);
                    }
                    (LineKind::Added | LineKind::Context, _, Some(new)) => {
                        new_lines.insert(new);
                    }
                    _ => {}
                }
            }
            LensFile {
                path: file.path.clone(),
                old_text: file.old_text.clone(),
                new_text: file.new_text.clone(),
                old_lines,
                new_lines,
            }
        })
        .collect()
}

impl App {
    /// `*` (`forward`) or `#`: open the lens on the cursor's line, or on the
    /// line it is already open on, narrow it to the next or previous symbol.
    pub(crate) fn symbol_lens(&mut self, forward: bool) {
        if self.diff.as_ref().is_some_and(|diff| diff.side_by_side) {
            self.info("switch to the unified view (|) to use the lens");
            return;
        }
        let Some(origin) = self.lens_origin_at_cursor() else {
            self.info("move onto a code line to see its names");
            return;
        };
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if let Some(lens) = diff.lens.as_mut()
            && lens.origin == origin
        {
            lens.cycle_focus(forward);
            self.lens_focus_changed();
            return;
        }
        let files = lens_files(&diff.model_for_rows(&self.review));
        self.lens_token = self.lens_token.wrapping_add(1);
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
            line: if on_old_side {
                line.old_no?
            } else {
                line.new_no?
            },
        })
    }

    /// Install a built lens, dropping one the reader has moved past.
    pub(crate) fn on_lens(&mut self, token: u64, lens: Lens) -> Flow {
        if token != self.lens_token {
            return Flow::Idle;
        }
        if lens.symbols.is_empty() {
            self.info("no names on this line");
            return Flow::Continue;
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.lens = Some(lens);
        }
        Flow::Continue
    }

    /// Whether the open diff shows a lens, which digits and `esc` then reach.
    /// Side-by-side draws no lens, so there it is as good as closed.
    pub(crate) fn lens_active(&self) -> bool {
        self.diff
            .as_ref()
            .is_some_and(|diff| diff.lens.is_some() && !diff.side_by_side)
    }

    /// A digit on a label: narrow the lens to that symbol, or widen it back
    /// when it is already the one in focus.
    pub(crate) fn lens_focus(&mut self, symbol: usize) {
        let Some(lens) = self.diff.as_mut().and_then(|diff| diff.lens.as_mut()) else {
            return;
        };
        if symbol < lens.symbols.len() {
            lens.focus = (lens.focus != Some(symbol)).then_some(symbol);
            self.lens_focus_changed();
        }
    }

    /// A focused symbol brings up its references sidebar in the comments
    /// sidebar's place; widening back to every symbol puts it away.
    fn lens_focus_changed(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.order_refs(review);
        if diff.refs_visible() {
            diff.comments_open = false;
            if diff.focus == Pane::Comments {
                diff.focus = Pane::Diff;
            }
        } else if diff.focus == Pane::References {
            diff.focus = Pane::Diff;
        }
    }

    pub(crate) fn lens_clear(&mut self) {
        if let Some(diff) = self.diff.as_mut() {
            diff.lens = None;
            if diff.focus == Pane::References {
                diff.focus = Pane::Diff;
            }
        }
    }

    /// Move the references sidebar's selection by `delta`, wrapping past
    /// either end when `wrap` (`n`/`N`) and stopping there otherwise (`j`/`k`),
    /// and seat the diff on the reference it lands on.
    pub(crate) fn refs_step(&mut self, delta: isize, wrap: bool) {
        let Some(lens) = self.diff.as_mut().and_then(|diff| diff.lens.as_mut()) else {
            return;
        };
        let count = lens.refs.len();
        if count == 0 {
            return;
        }
        let last = count - 1;
        lens.ref_cursor = if wrap {
            let count = isize::try_from(count).unwrap_or(isize::MAX);
            let at = isize::try_from(lens.ref_cursor).unwrap_or(0);
            usize::try_from((at + delta).rem_euclid(count)).unwrap_or(0)
        } else {
            lens.ref_cursor.saturating_add_signed(delta).min(last)
        };
        self.seat_ref();
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
            .and_then(|lens| lens.refs.get(lens.ref_cursor).cloned())
        else {
            return;
        };
        if diff.selected != entry.file {
            diff.select(entry.file, review);
        }
        diff.reveal_line(review, (entry.hunk, entry.line));
    }

    /// `n`/`N` with a lens up: the next or previous use of what it shows, in
    /// diff order, moving to the next file that has one past the last.
    pub(crate) fn lens_step(&mut self, forward: bool) {
        if self
            .diff
            .as_ref()
            .and_then(|diff| diff.lens.as_ref())
            .is_some_and(|lens| lens.focus.is_some())
        {
            return self.refs_step(if forward { 1 } else { -1 }, true);
        }
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let Some(lens) = diff.lens.clone() else {
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
            if step > 0 {
                if !crosses_files && at != start {
                    continue;
                }
                if at != diff.selected {
                    diff.select(at, review);
                }
            }
            let Some(path) = paths.get(at) else {
                continue;
            };
            let mut rows = use_rows(diff, review, &lens, path);
            rows.sort_unstable_by_key(|(row, _)| *row);
            let target = match (step, forward) {
                (0, true) => rows.iter().find(|(row, _)| *row > diff.cursor),
                (0, false) => rows.iter().rev().find(|(row, _)| *row < diff.cursor),
                (_, true) => rows.first(),
                (_, false) => rows.last(),
            };
            if let Some(&(_, (hunk, line))) = target {
                diff.reveal_line(review, (hunk, line));
                return;
            }
        }
        self.info("no other use of this name in the diff");
    }

    /// Run the queued lens inline, the way the runtime's worker does.
    #[cfg(test)]
    pub(crate) fn settle_lens(&mut self) {
        if let Some(request) = self.pending_lens.take() {
            let lens = compute_lens(&request);
            self.on_lens(request.token, lens);
        }
    }
}

impl super::DiffView {
    /// Whether the references sidebar shows: a symbol is focused, in the
    /// unified view.
    pub(crate) fn refs_visible(&self) -> bool {
        !self.side_by_side && self.lens.as_ref().is_some_and(|lens| lens.focus.is_some())
    }

    /// List the focused symbol's uses in diff order, one per line, and select
    /// the one on the line the lens was opened on. The walkthrough layout
    /// shows one slide at a time, so there it lists the slide's own file.
    fn order_refs(&mut self, review: &diffler_core::review::Review) {
        let walkthrough = self.layout == FileLayout::Walkthrough;
        let selected = self.selected;
        let model = self.model_for_rows(review).into_owned();
        let Some(lens) = self.lens.as_mut() else {
            return;
        };
        lens.refs.clear();
        lens.ref_cursor = 0;
        let Some(symbol) = lens.focus else {
            return;
        };
        for (file_at, file) in model.files.iter().enumerate() {
            if walkthrough && file_at != selected {
                continue;
            }
            for (hunk_at, hunk) in file.hunks.iter().enumerate() {
                for (line_at, line) in hunk.lines.iter().enumerate() {
                    let on_old_side = line.kind == LineKind::Deleted;
                    let number = if on_old_side {
                        line.old_no
                    } else {
                        line.new_no
                    };
                    let found = lens.uses.iter().position(|found| {
                        found.symbol == symbol
                            && found.path == file.path
                            && found.on_old_side == on_old_side
                            && Some(found.line) == number
                    });
                    if let Some(use_index) = found {
                        lens.refs.push(RefEntry {
                            use_index,
                            file: file_at,
                            hunk: hunk_at,
                            line: line_at,
                        });
                    }
                }
            }
        }
        let origin = &lens.origin;
        lens.ref_cursor = lens
            .refs
            .iter()
            .position(|entry| {
                lens.uses.get(entry.use_index).is_some_and(|found| {
                    found.path == origin.path
                        && found.on_old_side == origin.on_old_side
                        && found.line == origin.line
                })
            })
            .unwrap_or(0);
    }
}

/// The rows of `path`'s shown uses in the pane, each with its model position
/// (hunk, line), for whatever the lens shows. A use a fold hides answers with
/// the fold's own row, which `reveal_line` then opens.
fn use_rows(
    diff: &super::DiffView,
    review: &diffler_core::review::Review,
    lens: &Lens,
    path: &str,
) -> Vec<(usize, (usize, usize))> {
    let model = diff.model_for_rows(review);
    let Some(file) = model.files.iter().find(|file| file.path == path) else {
        return Vec::new();
    };
    let wanted: HashSet<(bool, u32)> = lens
        .uses
        .iter()
        .filter(|found| found.path == path && lens.shows(found.symbol))
        .map(|found| (found.on_old_side, found.line))
        .collect();
    let mut out = Vec::new();
    for (hunk_at, hunk) in file.hunks.iter().enumerate() {
        for (line_at, line) in hunk.lines.iter().enumerate() {
            let on_old_side = line.kind == LineKind::Deleted;
            let number = if on_old_side {
                line.old_no
            } else {
                line.new_no
            };
            let Some(number) = number else {
                continue;
            };
            if !wanted.contains(&(on_old_side, number)) {
                continue;
            }
            let row = diff.rows().iter().position(|row| {
                matches!(*row, DiffRow::Line { hunk: h, line: l, .. } if h == hunk_at && l == line_at)
            });
            let row = row.or_else(|| {
                diff.fold_groups
                    .iter()
                    .position(|group| group.lines.contains(&(hunk_at, line_at)))
                    .and_then(|group| {
                        diff.rows().iter().position(
                            |row| matches!(*row, DiffRow::Fold { group: g, .. } if g == group),
                        )
                    })
            });
            if let Some(row) = row {
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
        let names: Vec<&str> = lens.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["apply", "price", "qty", "discount"]);
        assert_eq!(lens.symbols[0].reach, Reach::Diff, "the diff defines apply");
        assert_eq!(lens.symbols[1].reach, Reach::Function("apply".to_owned()));
        let uses = |symbol: usize| lens.uses.iter().filter(|u| u.symbol == symbol).count();
        let files: HashSet<&str> = lens
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
        assert_eq!(lens.focus, Some(3), "discount");
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
        assert_eq!(lens(&app).focus, Some(0));
        app.handle(key('*'));
        assert_eq!(lens(&app).focus, Some(1));
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
        assert_eq!(lens(&app).focus, Some(3), "from all names to the last");
        app.handle(key('#'));
        assert_eq!(lens(&app).focus, Some(2));
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
        assert_eq!(lens.refs.len(), 5, "one per line apply is used on");
        let selected = &lens.uses[lens.refs[lens.ref_cursor].use_index];
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
        assert_eq!(lens.symbols[0].name, "log_it");
        assert_eq!(lens.symbols[0].reach, Reach::Diff);
        let files: HashSet<&str> = lens
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
            Some("no names on this line")
        );
    }
}
