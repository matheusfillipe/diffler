//! The walkthrough layout's own view: which slide the pane windows to, and
//! the row-narrowing that gets it there. Distinct from `app::walkthrough`,
//! which publishes a walkthrough and resolves its anchors; this module only
//! decides what the reader sees once one is open.

use std::borrow::Cow;
use std::collections::HashSet;

use diffler_core::model::{DiffModel, FileDiff};
use diffler_core::session::Session;
use diffler_core::walkthrough::Walkthrough;

use crate::app::composer::card_budget;
use crate::app::walkthrough::{CachedBody, blocks, body_hash, has_figure, summary_figure_key};

use super::{DiffRow, DiffView, RowCopy, blocks_of, summary_display};

/// What the walkthrough layout windows the pane to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Slide {
    /// A stop's own region, by index into the walkthrough's `stops`.
    Stop(usize),
    /// A comment outside every stop's region, shown as a slide of its own so
    /// reaching it never falls back to the whole file.
    AdHoc(String),
    /// The walkthrough's own summary: one card, no code rows, the sidebar's
    /// leading slide where the walkthrough has one.
    Summary,
}

#[cfg(test)]
thread_local! {
    static MERGE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times [`DiffView::model_with_context`] has actually cloned and
/// merged a model on this thread: a render's own count must not grow, or it
/// rebuilt what [`DiffView::ensure_rows`] already cached.
#[cfg(test)]
pub(crate) fn merge_count() -> usize {
    MERGE_COUNT.with(std::cell::Cell::get)
}

impl DiffView {
    /// `base`'s files, plus `context_files` appended after them, a path
    /// already in `base` never duplicated. A free-standing function (not a
    /// `&self` method) so a caller already holding disjoint field borrows
    /// (`commit_model`, `context_files`) can compose it without widening
    /// them into a borrow of the whole view, which would collide with the
    /// mutations `ensure_rows` makes right after reading the model. It clones
    /// `base`'s files whenever there is anything to append, so a caller on the
    /// render path wants the result cached rather than rebuilt per frame.
    pub(crate) fn model_with_context<'a>(
        base: &'a DiffModel,
        context_files: &'a [FileDiff],
    ) -> Cow<'a, DiffModel> {
        if context_files.is_empty() {
            return Cow::Borrowed(base);
        }
        #[cfg(test)]
        MERGE_COUNT.with(|count| count.set(count.get() + 1));
        let mut files = base.files.clone();
        for extra in context_files {
            if !files.iter().any(|file| file.path == extra.path) {
                files.push(extra.clone());
            }
        }
        Cow::Owned(DiffModel { files })
    }

    /// Parse the bodies that hold a figure, reusing what is already parsed for
    /// the same body at the same width. Bodies without one are left out, so an
    /// ordinary comment costs nothing here. The walkthrough's own summary is
    /// one more such body, cached under `summary_figure_key` rather than a
    /// comment id since it is not one.
    pub(super) fn ensure_figures(&mut self, session: &Session) {
        let width = card_budget(self.wrap_width);
        let mut live = HashSet::new();
        for comment in &session.comments {
            if !has_figure(&comment.body) {
                continue;
            }
            live.insert(comment.id.clone());
            let hash = body_hash(&comment.body, width);
            if self
                .figures
                .get(&comment.id)
                .is_some_and(|cached| cached.hash == hash)
            {
                continue;
            }
            self.figures.insert(
                comment.id.clone(),
                CachedBody {
                    hash,
                    blocks: blocks(&comment.body, width),
                },
            );
            self.figures_dirty = true;
        }
        let summary = self.active_walkthrough(session).and_then(|walkthrough| {
            let body = walkthrough.summary.as_ref()?;
            has_figure(body).then(|| (summary_figure_key(&walkthrough.id), body.clone()))
        });
        if let Some((key, body)) = summary {
            live.insert(key.clone());
            let hash = body_hash(&body, width);
            if self
                .figures
                .get(&key)
                .is_none_or(|cached| cached.hash != hash)
            {
                self.figures.insert(
                    key,
                    CachedBody {
                        hash,
                        blocks: blocks(&body, width),
                    },
                );
                self.figures_dirty = true;
            }
        }
        self.figures.retain(|id, _| live.contains(id));
    }

    /// Whether a figure has been parsed afresh since this was last asked, so
    /// the caller knows to read the files its nodes name.
    pub(crate) fn take_figures_dirty(&mut self) -> bool {
        std::mem::take(&mut self.figures_dirty)
    }

    /// The walkthrough this layout shows: the one this source's session is.
    /// Kept as a method (not a free function) so every call site reads the
    /// same way regardless of what backs it.
    #[allow(clippy::unused_self)]
    pub(crate) fn active_walkthrough<'a>(&self, session: &'a Session) -> Option<&'a Walkthrough> {
        session.walkthrough.as_ref()
    }

    /// The comment the slide on screen is built around, by index into the
    /// session's comments: the ad hoc comment when one is open, else the
    /// current stop. `Slide::Summary` has no comment behind it at all: its
    /// card is built by `summary_rows` instead.
    pub(crate) fn slide_primary(&self, session: &Session) -> Option<usize> {
        let walkthrough = self.active_walkthrough(session)?;
        let id: &str = match &self.slide {
            Some(Slide::AdHoc(id)) => id,
            Some(Slide::Stop(index)) => walkthrough.stops.get(*index)?,
            Some(Slide::Summary) => return None,
            None => walkthrough.stops.first()?,
        };
        session.comments.iter().position(|comment| comment.id == id)
    }

    /// Clamp a slide that no longer matches the session: a stop index past a
    /// walkthrough that has shrunk falls back to its last stop, or to nothing
    /// once it has none left, and an ad hoc comment that is gone falls back to
    /// nothing rather than windowing to a comment that no longer exists.
    pub(super) fn validate_slide(&mut self, session: &Session) {
        let Some(walkthrough) = self.active_walkthrough(session) else {
            self.slide = None;
            return;
        };
        match &self.slide {
            Some(Slide::Stop(index)) if *index >= walkthrough.stops.len() => {
                self.slide = walkthrough.stops.len().checked_sub(1).map(Slide::Stop);
            }
            Some(Slide::AdHoc(id)) if session.comment(id).is_none() => {
                self.slide = None;
            }
            _ => {}
        }
    }

    /// Narrow the raw rows down to the slide on screen: `Slide::Summary` shows
    /// the walkthrough's own summary card, built by `summary_rows`; every
    /// other slide narrows to its single primary comment's region. Where the
    /// review has no walkthrough at all, the layout falls back to the file
    /// tree, so a file list windowed to nothing is a blank pane.
    pub(super) fn window_slide(
        &self,
        model: &DiffModel,
        session: &Session,
        rows: Vec<DiffRow>,
        copy: Vec<RowCopy>,
    ) -> (Vec<DiffRow>, Vec<RowCopy>) {
        if self.active_walkthrough(session).is_none() {
            return (rows, copy);
        }
        if matches!(self.slide, Some(Slide::Summary)) {
            return self.summary_rows(session);
        }
        let Some(primary) = self.slide_primary(session) else {
            return (Vec::new(), Vec::new());
        };
        Self::slide_rows(model, session, self.selected, primary, rows, copy)
    }

    /// The walkthrough's own summary as the slide on screen: one card and no
    /// code rows at all, the way a stop with no anchored line already shows
    /// its own card alone.
    fn summary_rows(&self, session: &Session) -> (Vec<DiffRow>, Vec<RowCopy>) {
        let Some(summary) = self
            .active_walkthrough(session)
            .and_then(|walkthrough| walkthrough.summary.as_deref())
        else {
            return (Vec::new(), Vec::new());
        };
        let key = self
            .active_walkthrough(session)
            .map(|walkthrough| summary_figure_key(&walkthrough.id))
            .unwrap_or_default();
        let blocks = blocks_of(&self.figures, &key);
        let lines = summary_display(summary, self.wrap_width, None, blocks);
        let rows = (0..lines.len())
            .map(|line| DiffRow::Summary { line })
            .collect();
        let copy = lines
            .iter()
            .map(|line| super::rows::row_copy_for(line, "Summary", &key))
            .collect();
        (rows, copy)
    }

    /// Narrow `rows` (already built for `file_index`) down to `primary`'s
    /// slide: the lines its span covers, the hunk headers they sit under,
    /// every comment the region holds, and the open composer. Every other
    /// row is dropped, so a slide reads as its own view rather than the
    /// whole file scrolled to a bookmark. A primary with no line to sit on
    /// shows the cards alone. `copy` is filtered by the same mask as `rows`
    /// so the two never drift apart.
    fn slide_rows(
        model: &DiffModel,
        session: &Session,
        file_index: usize,
        primary: usize,
        rows: Vec<DiffRow>,
        copy: Vec<RowCopy>,
    ) -> (Vec<DiffRow>, Vec<RowCopy>) {
        let held: HashSet<usize> = crate::app::walkthrough::slide_comments(session, primary)
            .into_iter()
            .collect();
        let anchor = session.comments.get(primary).map(|c| &c.anchor);
        let span = anchor.and_then(diffler_core::session::Anchor::span);
        let on_old_side = anchor.is_some_and(|anchor| anchor.on_old_side);
        let file = model
            .files
            .get(file_index)
            .filter(|file| anchor.is_some_and(|anchor| anchor.file == file.path) && span.is_some());

        let mut keep = vec![false; rows.len()];
        let mark = |keep: &mut [bool], index: usize| {
            if let Some(slot) = keep.get_mut(index) {
                *slot = true;
            }
        };
        let mut kept_hunks = HashSet::new();
        for (index, row) in rows.iter().enumerate() {
            match row {
                DiffRow::Line { hunk, line, .. } => {
                    let inside = span.is_some_and(|(start, end)| {
                        file.and_then(|file| file.hunks.get(*hunk))
                            .and_then(|h| h.lines.get(*line))
                            .and_then(|dl| if on_old_side { dl.old_no } else { dl.new_no })
                            .is_some_and(|no| start <= no && no <= end)
                    });
                    if inside {
                        mark(&mut keep, index);
                        kept_hunks.insert(*hunk);
                    }
                }
                DiffRow::Comment { comment, .. } if held.contains(comment) => {
                    mark(&mut keep, index);
                }
                DiffRow::Composer { .. } => mark(&mut keep, index),
                DiffRow::Comment { .. } | DiffRow::Hunk { .. } | DiffRow::Summary { .. } => {}
            }
        }
        for (index, row) in rows.iter().enumerate() {
            if let DiffRow::Hunk { hunk, .. } = row
                && kept_hunks.contains(hunk)
            {
                mark(&mut keep, index);
            }
        }
        rows.into_iter()
            .zip(copy)
            .zip(keep)
            .filter_map(|(pair, kept)| kept.then_some(pair))
            .unzip()
    }
}

/// The model file a stop's comment is anchored to, when the diff carries it.
/// A stop pointing outside the diff addresses no file.
pub(super) fn stop_file_index(model: &DiffModel, session: &Session, id: &str) -> Option<usize> {
    let path = &session.comment(id)?.anchor.file;
    model.files.iter().position(|file| file.path == *path)
}

/// Rows the sidebar reserves before a stop's own row: 1 when the walkthrough
/// carries a summary (its own leading row), 0 otherwise.
pub(crate) fn stop_row_offset(walkthrough: &Walkthrough) -> usize {
    usize::from(walkthrough.summary.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_file(path: &str) -> FileDiff {
        FileDiff {
            path: path.to_owned(),
            old_path: None,
            status: diffler_core::model::FileStatus::Modified,
            binary: false,
            old_text: None,
            new_text: None,
            hunks: Vec::new(),
            hashes: diffler_core::model::HashCache::default(),
        }
    }

    #[test]
    fn model_with_context_appends_missing_files_and_skips_a_path_already_in_the_diff() {
        let base = DiffModel {
            files: vec![stub_file("src/lib.rs")],
        };
        let extra = [stub_file("src/lib.rs"), stub_file("notes.txt")];
        let merged = DiffView::model_with_context(&base, &extra);
        let paths: Vec<&str> = merged.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["src/lib.rs", "notes.txt"]);
    }

    #[test]
    fn model_with_context_borrows_the_base_when_there_is_nothing_to_append() {
        let base = DiffModel {
            files: vec![stub_file("src/lib.rs")],
        };
        let merged = DiffView::model_with_context(&base, &[]);
        assert!(matches!(merged, Cow::Borrowed(_)));
    }
}
