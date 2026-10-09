//! What a [`DiffRow`] points at, named by something that survives
//! [`DiffView::ensure_rows`] rebuilding the row list. The status screen's
//! counterpart is [`crate::app::status::CursorAnchor`].

use diffler_core::model::{DiffModel, HunkId};
use diffler_core::review::Review;
use diffler_core::session::Session;

use super::{DiffRow, DiffView};

/// The identity of the thing one [`DiffRow`] displays, in a form that
/// outlives its row index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowRef {
    Hunk(HunkId),
    /// A line by its file's path, its number on its side, and that side.
    Line {
        file: String,
        line: u32,
        on_old_side: bool,
    },
    /// A row of a stop's card, by the stop's position in the walkthrough's
    /// own order and the row's line within the card, since a revision
    /// reissues every stop's comment id.
    Stop {
        index: usize,
        line: usize,
    },
    /// A row of a comment's card, by the comment's id and the row's line
    /// within the card.
    Comment {
        id: String,
        line: usize,
    },
    /// One display line of the open composer; its header is line 0.
    Composer(usize),
    /// A row of the walkthrough summary's card, by its line within the card.
    Summary {
        line: usize,
    },
    /// A folded hunk's row, by its hunk key.
    Fold {
        key: String,
    },
}

/// The cursor, the visual selection anchor, and the banded span, each named
/// by the [`RowRef`] it sits on.
#[derive(Debug)]
pub(crate) struct RowPositions {
    cursor: Option<RowRef>,
    anchor: Option<RowRef>,
    referenced: Option<(RowRef, RowRef)>,
}

impl DiffView {
    /// Name what the cursor, the visual anchor, and the banded span sit on,
    /// while `self.rows` and `review` still agree. A caller swapping the model
    /// must capture before the swap, since a `RowRef` read from stale rows
    /// against the new model names the wrong thing.
    pub(crate) fn capture_positions(&self, review: &Review) -> RowPositions {
        RowPositions {
            cursor: self.row_ref(review, self.cursor),
            anchor: self.visual_anchor.and_then(|row| self.row_ref(review, row)),
            referenced: self.referenced.and_then(|(start, end)| {
                Some((self.row_ref(review, start)?, self.row_ref(review, end)?))
            }),
        }
    }

    /// Resolve `positions` (from [`Self::capture_positions`]) against the
    /// rows on screen. The cursor falls to the nearest surviving row; a
    /// selection anchor or band over something gone is dropped.
    pub(crate) fn restore_positions(&mut self, review: &Review, positions: RowPositions) {
        self.cursor = positions
            .cursor
            .and_then(|target| self.find_row(review, &target))
            .unwrap_or_else(|| self.cursor.min(self.rows.len().saturating_sub(1)));
        self.visual_anchor = positions
            .anchor
            .and_then(|target| self.find_row(review, &target));
        self.referenced = positions.referenced.and_then(|(start, end)| {
            Some((self.find_row(review, &start)?, self.find_row(review, &end)?))
        });
    }

    /// The identity of the thing row `row` displays, or `None` when `row` is
    /// out of range or the model lacks it.
    pub(crate) fn row_ref(&self, review: &Review, row: usize) -> Option<RowRef> {
        let session = review.session_for(&self.source);
        let model = self.model_for_rows(review);
        match *self.rows.get(row)? {
            DiffRow::Hunk { file, hunk } => Some(RowRef::Hunk(
                model.files.get(file)?.hunks.get(hunk)?.id.clone(),
            )),
            DiffRow::Line { file, hunk, line } => {
                let file = model.files.get(file)?;
                let diff_line = file.hunks.get(hunk)?.lines.get(line)?;
                let (line, on_old_side) = match diff_line.new_no {
                    Some(no) => (no, false),
                    None => (diff_line.old_no?, true),
                };
                Some(RowRef::Line {
                    file: file.path.clone(),
                    line,
                    on_old_side,
                })
            }
            DiffRow::Comment { comment, line, .. } => {
                let comment = session.comments.get(comment)?;
                match self.active_walkthrough(session).and_then(|walkthrough| {
                    walkthrough.stops.iter().position(|id| *id == comment.id)
                }) {
                    Some(index) => Some(RowRef::Stop { index, line }),
                    None => Some(RowRef::Comment {
                        id: comment.id.clone(),
                        line,
                    }),
                }
            }
            DiffRow::Composer { line } => Some(RowRef::Composer(line)),
            DiffRow::Summary { line } => Some(RowRef::Summary { line }),
            DiffRow::Fold { group, .. } => Some(RowRef::Fold {
                key: self.fold_groups.get(group)?.keys.first()?.clone(),
            }),
        }
    }

    fn fold_row_leads_with(&self, row: &DiffRow, key: &str) -> bool {
        matches!(row, DiffRow::Fold { group, .. }
            if self.fold_groups.get(*group).and_then(|g| g.keys.first()).is_some_and(|k| k == key))
    }

    /// The fold row hiding line `line` of `file` on the given side.
    fn fold_row_hiding(
        &self,
        model: &DiffModel,
        file: &str,
        line: u32,
        on_old_side: bool,
    ) -> Option<usize> {
        self.rows.iter().position(|row| {
            let DiffRow::Fold { file: index, group } = *row else {
                return false;
            };
            let (Some(found), Some(fold)) = (model.files.get(index), self.fold_groups.get(group))
            else {
                return false;
            };
            found.path == file
                && fold.lines.iter().any(|&(hunk, at)| {
                    found
                        .hunks
                        .get(hunk)
                        .and_then(|hunk| hunk.lines.get(at))
                        .is_some_and(|diff_line| diff_line.number_on(on_old_side) == Some(line))
                })
        })
    }

    /// The row holding `target`, or `None` when the thing it names is gone.
    pub(crate) fn find_row(&self, review: &Review, target: &RowRef) -> Option<usize> {
        let session = review.session_for(&self.source);
        let model = self.model_for_rows(review);
        let header_of = |id: &HunkId| {
            self.rows.iter().position(|row| {
                let DiffRow::Hunk { file, hunk } = row else {
                    return false;
                };
                model
                    .files
                    .get(*file)
                    .and_then(|file| file.hunks.get(*hunk))
                    .is_some_and(|found| found.id == *id)
            })
        };
        let fold_of = |key: &str| {
            self.rows
                .iter()
                .position(|row| self.fold_row_leads_with(row, key))
        };
        // folding a hunk turns its header into its fold row and opening it
        // turns it back, so each name finds the other form of the same hunk
        match target {
            RowRef::Hunk(id) => header_of(id).or_else(|| fold_of(&id.0)),
            // a line a fold now hides is found as that fold's row, so closing
            // a fold under the cursor leaves the cursor on it
            RowRef::Line {
                file,
                line,
                on_old_side,
            } => self
                .rows
                .iter()
                .position(|row| {
                    let DiffRow::Line {
                        file: file_index,
                        hunk,
                        line: line_index,
                    } = row
                    else {
                        return false;
                    };
                    model
                        .files
                        .get(*file_index)
                        .filter(|found| found.path == *file)
                        .and_then(|found| found.hunks.get(*hunk))
                        .and_then(|hunk| hunk.lines.get(*line_index))
                        .is_some_and(|diff_line| diff_line.number_on(*on_old_side) == Some(*line))
                })
                .or_else(|| self.fold_row_hiding(&model, file, *line, *on_old_side)),
            RowRef::Stop { index, line } => {
                let id = self.active_walkthrough(session)?.stops.get(*index)?;
                let header = Self::find_comment_row(&self.rows, session, id)?;
                Some(Self::card_row(&self.rows, header, *line))
            }
            RowRef::Comment { id, line } => {
                let header = Self::find_comment_row(&self.rows, session, id)?;
                Some(Self::card_row(&self.rows, header, *line))
            }
            RowRef::Composer(line) => self
                .rows
                .iter()
                .position(|row| matches!(row, DiffRow::Composer { line: l } if l == line)),
            RowRef::Summary { line } => {
                let header = self
                    .rows
                    .iter()
                    .position(|row| matches!(row, DiffRow::Summary { line: 0 }))?;
                Some(Self::card_row(&self.rows, header, *line))
            }
            RowRef::Fold { key, .. } => fold_of(key).or_else(|| header_of(&HunkId(key.clone()))),
        }
    }

    /// The row `line` lines into the card whose header sits at `header`, or
    /// the card's last row when the card is shorter.
    fn card_row(rows: &[DiffRow], header: usize, line: usize) -> usize {
        let Some(card) = rows.get(header) else {
            return header;
        };
        rows.iter()
            .enumerate()
            .skip(header)
            .take_while(|(_, row)| same_card(card, row))
            .take(line + 1)
            .last()
            .map_or(header, |(index, _)| index)
    }

    /// The header row (line 0) of the comment carrying `id`, if it is on
    /// screen.
    fn find_comment_row(rows: &[DiffRow], session: &Session, id: &str) -> Option<usize> {
        rows.iter().position(|row| {
            matches!(row, DiffRow::Comment { comment, line: 0, .. }
                if session.comments.get(*comment).is_some_and(|c| c.id == id))
        })
    }
}

/// Whether two rows belong to the same card: one comment's, or the summary's.
fn same_card(a: &DiffRow, b: &DiffRow) -> bool {
    match (a, b) {
        (DiffRow::Comment { comment: left, .. }, DiffRow::Comment { comment: right, .. }) => {
            left == right
        }
        (DiffRow::Summary { .. }, DiffRow::Summary { .. }) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::session::Anchor;

    use super::*;
    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::test_support::{key, seat_walkthrough, set_walkthrough_summary, two_hunk_fixture};

    fn unanchored(file: &str) -> Anchor {
        Anchor {
            file: file.to_owned(),
            line: None,
            line_end: None,
            on_old_side: false,
            line_text: None,
        }
    }

    fn first_row(app: &App, matches: impl Fn(&DiffRow) -> bool) -> usize {
        app.diff
            .as_ref()
            .expect("diff")
            .rows()
            .iter()
            .position(matches)
            .expect("a matching row")
    }

    #[test]
    fn row_ref_names_every_diff_row_variant_by_something_stable() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        let comment_id = app
            .review
            .session
            .add_comment(unanchored("data.txt"), "reviewer", "why?")
            .id
            .clone();
        app.open_working_tree_file("data.txt");

        let hunk_row = first_row(&app, |row| matches!(row, DiffRow::Hunk { .. }));
        let line_row = first_row(&app, |row| matches!(row, DiffRow::Line { .. }));
        let comment_row = first_row(&app, |row| matches!(row, DiffRow::Comment { line: 0, .. }));
        app.diff.as_mut().expect("diff").cursor = line_row;
        app.handle(key('c'));
        assert!(app.composer_open(), "composer stayed open");
        let composer_row = first_row(&app, |row| matches!(row, DiffRow::Composer { .. }));

        let diff = app.diff.as_ref().expect("diff");
        let model = diff.model(&app.review);
        let file = &model.files[0];

        assert_eq!(
            diff.row_ref(&app.review, hunk_row),
            Some(RowRef::Hunk(file.hunks[0].id.clone()))
        );
        let DiffRow::Line { hunk, line, .. } = diff.rows()[line_row] else {
            panic!("line_row is a DiffRow::Line");
        };
        let diff_line = &file.hunks[hunk].lines[line];
        let expected_line = RowRef::Line {
            file: file.path.clone(),
            line: diff_line.new_no.or(diff_line.old_no).expect("a number"),
            on_old_side: diff_line.new_no.is_none(),
        };
        assert_eq!(diff.row_ref(&app.review, line_row), Some(expected_line));
        assert_eq!(
            diff.row_ref(&app.review, comment_row),
            Some(RowRef::Comment {
                id: comment_id,
                line: 0
            })
        );
        let DiffRow::Composer {
            line: composer_line,
        } = diff.rows()[composer_row]
        else {
            panic!("composer_row is a DiffRow::Composer");
        };
        assert_eq!(
            diff.row_ref(&app.review, composer_row),
            Some(RowRef::Composer(composer_line))
        );
    }

    #[test]
    fn row_ref_names_a_walkthrough_stop_and_its_summary() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        seat_walkthrough(
            &mut app,
            "how it works",
            &[("first stop", Some("data.txt"), "look here")],
        );
        set_walkthrough_summary(&mut app, "w1", "the shape of it");
        app.open_walkthrough("w1", crate::app::diff::Slide::Summary);

        let summary_row = first_row(&app, |row| matches!(row, DiffRow::Summary { line: 0 }));
        assert_eq!(
            app.diff
                .as_ref()
                .expect("diff")
                .row_ref(&app.review, summary_row),
            Some(RowRef::Summary { line: 0 })
        );

        app.diff.as_mut().expect("diff").slide = Some(crate::app::diff::Slide::Stop(0));
        app.diff.as_mut().expect("diff").mark_rows_dirty();
        app.diff.as_mut().expect("diff").ensure_rows(&app.review);
        let stop_row = first_row(&app, |row| matches!(row, DiffRow::Comment { line: 0, .. }));
        assert_eq!(
            app.diff
                .as_ref()
                .expect("diff")
                .row_ref(&app.review, stop_row),
            Some(RowRef::Stop { index: 0, line: 0 })
        );
    }

    #[test]
    fn round_trip_through_row_ref_and_find_row_returns_the_same_row() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.review
            .session
            .add_comment(unanchored("data.txt"), "reviewer", "why?");
        app.open_working_tree_diff(None);

        let diff = app.diff.as_ref().expect("diff");
        for row in 0..diff.rows().len() {
            let Some(target) = diff.row_ref(&app.review, row) else {
                continue;
            };
            assert_eq!(
                diff.find_row(&app.review, &target),
                Some(row),
                "row {row} round-trips through {target:?}"
            );
        }
    }

    /// A `RowRef` still finds the row it named after an unrelated comment
    /// lands above it and pushes every row below down.
    #[test]
    fn round_trip_still_finds_the_row_after_a_comment_lands_above_it() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);

        let diff = app.diff.as_ref().expect("diff");
        let hunk_row = first_row(&app, |row| matches!(row, DiffRow::Hunk { .. }));
        let target = diff.row_ref(&app.review, hunk_row).expect("a hunk row");
        let original = diff.rows()[hunk_row];

        app.review
            .session
            .add_comment(unanchored("data.txt"), "reviewer", "look here first");
        let diff = app.diff.as_mut().expect("diff");
        diff.mark_rows_dirty();
        diff.ensure_rows(&app.review);

        let diff = app.diff.as_ref().expect("diff");
        let new_row = diff
            .find_row(&app.review, &target)
            .expect("the hunk row is still there");
        assert_ne!(new_row, hunk_row, "the new comment pushed it down");
        assert_eq!(
            diff.rows()[new_row],
            original,
            "and it's still the same row"
        );
    }

    #[test]
    fn find_row_returns_none_once_the_comment_is_gone() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        let comment_id = app
            .review
            .session
            .add_comment(unanchored("data.txt"), "reviewer", "why?")
            .id
            .clone();
        app.open_working_tree_diff(None);
        let target = RowRef::Comment {
            id: comment_id.clone(),
            line: 0,
        };
        assert!(
            app.diff
                .as_ref()
                .expect("diff")
                .find_row(&app.review, &target)
                .is_some(),
            "the comment starts out on screen"
        );

        app.review.session.comments.retain(|c| c.id != comment_id);
        let diff = app.diff.as_mut().expect("diff");
        diff.mark_rows_dirty();
        diff.ensure_rows(&app.review);

        assert_eq!(
            app.diff
                .as_ref()
                .expect("diff")
                .find_row(&app.review, &target),
            None
        );
    }
}
