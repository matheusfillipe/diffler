//! The agent's `focus` call: take the human to a comment, a walkthrough or a
//! stretch of code, in whichever review holds it. Every step goes through
//! the same opens and seats the keys use, so the screen stack, the folds and
//! a PR's fetch behave exactly as they do for the human.

use diffler_core::model::FileStatus;
use diffler_core::source::ReviewSource;

use super::open::PrLookup;
use super::{Pane, ScrollAlign, Slide};
use crate::app::{App, Screen};
use crate::mcp::{FocusResponse, FocusTarget, FocusView, McpResponse};

/// Where the cursor goes once a review is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Spot {
    Comment {
        id: String,
        file: String,
        span: Option<(u32, u32)>,
    },
    Walkthrough,
    Code {
        file: String,
        span: Option<(u32, u32)>,
    },
}

/// A focus whose review is still fetching, retried once the fetch lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingFocus {
    source: ReviewSource,
    spot: Spot,
    note: Option<String>,
}

enum Destination {
    Review(ReviewSource, Spot),
    /// A file no review holds, shown as it is on disk.
    File {
        path: String,
        span: Option<(u32, u32)>,
    },
}

enum Opened {
    Ready,
    Waiting,
    Failed(String),
}

pub(crate) const TYPING_REFUSAL: &str = "the human is typing or answering a dialog, so nothing \
                                 moved; try again once they send";

const PARKED_DRAFT: &str = "the human has an unsent comment in the review on screen, so \
                            nothing moved; try again once they send it";

const REVIEW_FORMS: &str = "review takes working, commit:<rev>, range:<oldest>..<newest>, \
                            pr:<number>, against:<rev>, or a source from list_reviews";

impl App {
    pub(crate) fn agent_focus(&mut self, target: FocusTarget, note: Option<String>) -> McpResponse {
        if self.busy_typing() {
            return McpResponse::Error(TYPING_REFUSAL.to_owned());
        }
        let destination = match self.focus_destination(target) {
            Ok(destination) => destination,
            Err(err) => return McpResponse::Error(err),
        };
        let keep = match &destination {
            Destination::Review(source, _) => Some(source),
            Destination::File { .. } => None,
        };
        if self.would_drop_drafts(keep) {
            return McpResponse::Error(PARKED_DRAFT.to_owned());
        }
        let note = note.filter(|note| !note.trim().is_empty());
        self.pending_focus = None;
        match destination {
            Destination::File { path, span } => {
                self.unwind_screens();
                self.open_file(&path, span, false);
                self.focused(None, FocusView::File, Some(path), span)
            }
            Destination::Review(source, spot) => {
                let (file, span) = spot.place();
                match self.open_for_focus(&source, &spot) {
                    Opened::Ready => {
                        let view = self.seat_focus(&spot);
                        self.announce(note.as_deref());
                        self.focused(Some(&source), view, file, span)
                    }
                    Opened::Waiting => {
                        let response = self.focused(Some(&source), FocusView::Waiting, file, span);
                        self.pending_focus = Some(PendingFocus { source, spot, note });
                        response
                    }
                    Opened::Failed(err) => McpResponse::Error(err),
                }
            }
        }
    }

    /// Finish a focus whose review was fetching. A human who started typing
    /// meanwhile keeps their place.
    pub(crate) fn retry_focus(&mut self) {
        let Some(PendingFocus { source, spot, note }) = self.pending_focus.take() else {
            return;
        };
        if self.busy_typing() || self.would_drop_drafts(Some(&source)) {
            self.info(format!("agent: see {}, it is ready now", source.label()));
            return;
        }
        match self.open_for_focus(&source, &spot) {
            Opened::Ready => {
                self.seat_focus(&spot);
                self.announce(note.as_deref());
            }
            Opened::Waiting => self.pending_focus = Some(PendingFocus { source, spot, note }),
            Opened::Failed(err) => self.error(err),
        }
    }

    /// A comment the human parked by clicking away lives on the open view,
    /// so replacing that view would throw it away.
    fn would_drop_drafts(&self, keep: Option<&ReviewSource>) -> bool {
        self.diff
            .as_ref()
            .is_some_and(|diff| !diff.parked_drafts.is_empty() && keep != Some(&diff.source))
    }

    fn focused(
        &self,
        source: Option<&ReviewSource>,
        view: FocusView,
        file: Option<String>,
        span: Option<(u32, u32)>,
    ) -> McpResponse {
        McpResponse::Focused(FocusResponse {
            project: self.project_name(),
            review: source.map(ReviewSource::label),
            view,
            file,
            line: span.map(|(line, _)| line),
        })
    }

    fn announce(&mut self, note: Option<&str>) {
        self.info(format!("agent: {}", note.unwrap_or("look here")));
    }

    fn focus_destination(&self, target: FocusTarget) -> Result<Destination, String> {
        match target {
            FocusTarget::Id(id) => self.id_destination(&id),
            FocusTarget::Code {
                file,
                line,
                line_end,
                review,
            } => {
                let file = file.trim_start_matches("./").to_owned();
                let span = match line {
                    Some(0) => return Err("lines count from 1".to_owned()),
                    Some(line) => Some((line, line_end.unwrap_or(line).max(line))),
                    None => None,
                };
                let source = match review {
                    Some(raw) => Some(self.parse_review(&raw)?),
                    None => self.source_holding(&file),
                };
                match source {
                    Some(ReviewSource::Walkthrough { .. }) => {
                        Err("focus a walkthrough by passing its id".to_owned())
                    }
                    Some(source) => Ok(Destination::Review(source, Spot::Code { file, span })),
                    None if self.review.repo_root.join(&file).is_file() => {
                        Ok(Destination::File { path: file, span })
                    }
                    None => Err(format!("no file {file} in {}", self.project_name())),
                }
            }
        }
    }

    fn id_destination(&self, id: &str) -> Result<Destination, String> {
        let (source, session) = self
            .owner_of_id(id)
            .ok_or_else(|| format!("no comment or walkthrough has the id {id}"))?;
        if matches!(&source, ReviewSource::Walkthrough { id: own } if own == id) {
            return Ok(Destination::Review(source, Spot::Walkthrough));
        }
        let comment = session
            .comment(id)
            .ok_or_else(|| format!("no comment has the id {id}"))?;
        let spot = Spot::Comment {
            id: id.to_owned(),
            file: comment.anchor.file.clone(),
            span: comment.anchor.span(),
        };
        Ok(Destination::Review(source, spot))
    }

    /// The review on screen when it shows `file`, else the working tree when
    /// it changes it.
    fn source_holding(&self, file: &str) -> Option<ReviewSource> {
        let on_screen = self
            .diff
            .as_ref()
            .filter(|diff| !matches!(diff.source, ReviewSource::Walkthrough { .. }))
            .filter(|diff| {
                diff.model(&self.review)
                    .files
                    .iter()
                    .any(|entry| entry.path == file)
            })
            .map(|diff| diff.source.clone());
        on_screen.or_else(|| {
            self.review
                .model()
                .files
                .iter()
                .any(|entry| entry.path == file)
                .then_some(ReviewSource::WorkingTree)
        })
    }

    fn parse_review(&self, raw: &str) -> Result<ReviewSource, String> {
        let raw = raw.trim();
        if let Some(source) = self
            .review
            .all_reviews()
            .unwrap_or_default()
            .into_iter()
            .map(|(source, _)| source)
            .find(|source| source.key() == raw)
        {
            return Ok(source);
        }
        let resolve = |rev: &str| {
            self.review
                .vcs
                .resolve(rev)
                .map_err(|err| format!("cannot resolve {rev}: {err}"))
        };
        let (kind, rest) = raw.split_once(':').unwrap_or((raw, ""));
        match kind {
            "working" => Ok(ReviewSource::WorkingTree),
            "commit" if !rest.is_empty() => Ok(ReviewSource::commit(resolve(rest)?)),
            "range" => {
                let (oldest, newest) = rest.split_once("..").ok_or(REVIEW_FORMS)?;
                Ok(ReviewSource::range(resolve(oldest)?, resolve(newest)?))
            }
            "pr" => rest
                .trim_start_matches('#')
                .parse()
                .map(ReviewSource::pr)
                .map_err(|_| REVIEW_FORMS.to_owned()),
            "against" if !rest.is_empty() => {
                resolve(rest)?;
                Ok(ReviewSource::against(rest))
            }
            _ => Err(REVIEW_FORMS.to_owned()),
        }
    }

    /// Open `source` the way its key would, unless it is already the review
    /// on screen.
    fn open_for_focus(&mut self, source: &ReviewSource, spot: &Spot) -> Opened {
        if self
            .diff
            .as_ref()
            .is_some_and(|diff| diff.source == *source)
        {
            while self.screen() != Screen::Diff && self.screens.len() > 1 {
                self.pop_screen();
            }
            return Opened::Ready;
        }
        self.unwind_screens();
        // we read the open's own error from the status line, so we clear
        // whatever an earlier action left there
        self.message = None;
        match source {
            ReviewSource::WorkingTree => self.open_working_tree_diff(None),
            ReviewSource::Commit { oid } => self.open_commit_diff(oid),
            ReviewSource::Range { oldest, newest } => self.open_range_diff(oldest, newest),
            ReviewSource::Against { rev } => self.open_against_diff(rev),
            ReviewSource::Pr { number } => {
                if let Some(waiting) = self.open_pr_for_focus(*number) {
                    return waiting;
                }
            }
            ReviewSource::Walkthrough { id } => {
                let slide = match spot {
                    Spot::Comment { id: comment, .. } => Slide::AdHoc(comment.clone()),
                    Spot::Walkthrough | Spot::Code { .. } => self.opening_slide(id),
                };
                self.open_walkthrough(id, slide);
                // the focus retries the whole open itself, so we keep one
                // pending slot for it
                if self.pending_walkthrough_open.take().is_some() {
                    return if self.pending_pr_open.is_some() || self.status.prs_in_flight {
                        Opened::Waiting
                    } else {
                        self.open_failure(source)
                    };
                }
            }
        }
        let fetching = matches!(
            (source, &self.pending_pr_open),
            (ReviewSource::Pr { number }, Some(pr)) if pr.number == *number
        );
        if self
            .diff
            .as_ref()
            .is_some_and(|diff| diff.source == *source)
        {
            Opened::Ready
        } else if fetching {
            Opened::Waiting
        } else {
            self.open_failure(source)
        }
    }

    fn open_failure(&self, source: &ReviewSource) -> Opened {
        Opened::Failed(self.message.as_ref().map_or_else(
            || format!("cannot open {}", source.label()),
            |message| message.text.clone(),
        ))
    }

    /// `None` once the open has run; otherwise how the focus has to wait.
    fn open_pr_for_focus(&mut self, number: u64) -> Option<Opened> {
        if let Some((base, head)) = self.pr_ranges.get(&number).cloned() {
            self.open_pr_diff(number, &base, &head);
            return None;
        }
        match self.find_pr(number) {
            PrLookup::Known(pr) => {
                self.open_pr_review_for(pr);
                None
            }
            PrLookup::NotOpen => Some(Opened::Failed(format!(
                "PR #{number} is not among this repo's open pull requests"
            ))),
            PrLookup::NoForge => Some(Opened::Failed(
                "no forge detected for this repo, so diffler cannot open a PR".to_owned(),
            )),
            PrLookup::Loading => Some(Opened::Waiting),
        }
    }

    /// Pop back to the screen diffler started on, so repeated jumps never
    /// pile screens up and one `q` returns the human to it.
    fn unwind_screens(&mut self) {
        while self.screens.len() > 1 {
            self.pop_screen();
        }
    }

    fn seat_focus(&mut self, spot: &Spot) -> FocusView {
        let view = match spot {
            Spot::Comment { id, file, span } => {
                let in_walkthrough = self
                    .diff
                    .as_ref()
                    .is_some_and(|diff| matches!(diff.source, ReviewSource::Walkthrough { .. }));
                // a walkthrough reseats its slide once the anchors it reads
                // land, so a card not on screen yet is still on its way
                if self.focus_comment(id) || in_walkthrough {
                    FocusView::Diff
                } else {
                    self.open_file(file, *span, false);
                    FocusView::File
                }
            }
            // we seated its first slide when we opened the walkthrough
            Spot::Walkthrough => FocusView::Diff,
            Spot::Code { file, span } => self.seat_code(file, *span),
        };
        if let (FocusView::Diff, Some(diff)) = (view, self.diff.as_mut()) {
            diff.focus = Pane::Diff;
            diff.scroll_align = Some(ScrollAlign::Center);
        }
        view
    }

    /// The cursor on `file`'s line `span` starts on, the span banded; a line
    /// outside the diff's hunks opens the file view there.
    fn seat_code(&mut self, file: &str, span: Option<(u32, u32)>) -> FocusView {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return FocusView::Diff;
        };
        let model = diff.model_for_rows(review);
        let Some(index) = model.files.iter().position(|entry| entry.path == file) else {
            self.open_file(file, span, false);
            return FocusView::File;
        };
        // a deleted file has only old-side lines to name
        let deleted = model
            .files
            .get(index)
            .is_some_and(|entry| entry.status == FileStatus::Deleted);
        let Some((line, end)) = span else {
            diff.select(index, review);
            diff.reveal_selected(review);
            diff.cursor = 0;
            diff.referenced = None;
            return FocusView::Diff;
        };
        if diff.seat_line(review, file, deleted, line).is_none() {
            self.open_file(file, span, false);
            return FocusView::File;
        }
        diff.referenced = diff.span_rows(review, index, line, end);
        FocusView::Diff
    }
}

impl Spot {
    fn place(&self) -> (Option<String>, Option<(u32, u32)>) {
        match self {
            Self::Comment { file, span, .. } | Self::Code { file, span } => {
                (Some(file.clone()), *span)
            }
            Self::Walkthrough => (None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::session::Anchor;

    use super::*;
    use crate::app::diff::DiffRow;
    use crate::config::{FileLayout, LoadedConfig};
    use crate::mcp::{McpRequestKind, StopParams};
    use crate::test_support::{Fixture, standard_fixture, two_hunk_fixture};

    fn focus(app: &mut App, target: FocusTarget) -> FocusResponse {
        match app.handle_mcp(McpRequestKind::Focus {
            target,
            note: Some("this one".to_owned()),
        }) {
            McpResponse::Focused(focused) => focused,
            other => panic!("the focus was refused: {other:?}"),
        }
    }

    fn code(file: &str, lines: Option<(u32, u32)>, review: Option<&str>) -> FocusTarget {
        FocusTarget::Code {
            file: file.to_owned(),
            line: lines.map(|(line, _)| line),
            line_end: lines.map(|(_, end)| end),
            review: review.map(str::to_owned),
        }
    }

    fn cursor_line(app: &App) -> Option<(String, Option<u32>)> {
        app.diff_cursor_file_line()
    }

    #[test]
    fn a_comment_in_a_past_commit_opens_that_commit_on_its_card() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let source = ReviewSource::commit(app.status.recent[0].oid.clone());
        app.review.ensure_source(&source).expect("source");
        let id = app
            .review
            .session_for_mut(&source)
            .add_comment(
                Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("    41".to_owned()),
                },
                "reviewer",
                "why 41?",
            )
            .id
            .clone();
        app.open_working_tree_diff(None);
        app.open_file("notes.txt", None, false);

        let focused = focus(&mut app, FocusTarget::Id(id));

        assert_eq!(focused.view, FocusView::Diff);
        assert_eq!(focused.review, Some(source.label()));
        assert_eq!(
            app.screens,
            [Screen::Status, Screen::Diff],
            "one q goes back"
        );
        let diff = app.diff.as_ref().expect("a diff");
        assert_eq!(diff.source, source);
        assert!(matches!(
            diff.rows().get(diff.cursor),
            Some(DiffRow::Comment { .. })
        ));
        assert_eq!(
            app.message.as_ref().map(|message| message.text.as_str()),
            Some("agent: this one")
        );
    }

    #[test]
    fn a_line_range_lands_the_cursor_on_its_first_line_and_bands_it() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());

        let focused = focus(&mut app, code("data.txt", Some((18, 20)), None));

        assert_eq!((focused.view, focused.line), (FocusView::Diff, Some(18)));
        assert_eq!(cursor_line(&app), Some(("data.txt".to_owned(), Some(18))));
        let diff = app.diff.as_ref().expect("a diff");
        assert_eq!(diff.source, ReviewSource::WorkingTree);
        assert_eq!(diff.focus, Pane::Diff);
        let (first, last) = diff.referenced.expect("the range is banded");
        assert_eq!(last - first, 3, "three lines and the one line 20 replaced");
    }

    #[test]
    fn a_line_the_diff_leaves_out_opens_the_file_there() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());

        let focused = focus(&mut app, code("data.txt", Some((10, 10)), None));

        assert_eq!(focused.view, FocusView::File);
        let open = app.pending_file.as_ref().expect("the file view loads");
        assert_eq!(
            (open.path.as_str(), open.span),
            ("data.txt", Some((10, 10)))
        );
    }

    #[test]
    fn a_file_no_review_changes_opens_as_it_is_on_disk() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());

        let focused = focus(&mut app, code("notes.txt", Some((1, 1)), None));

        assert_eq!((focused.view, focused.review), (FocusView::File, None));
        assert!(app.pending_file.is_some());
    }

    #[test]
    fn a_named_commit_opens_that_commits_diff() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());

        let focused = focus(
            &mut app,
            code("src/lib.rs", Some((2, 2)), Some("commit:HEAD")),
        );

        assert_eq!(focused.view, FocusView::Diff);
        let head = app.review.vcs.resolve("HEAD").expect("head");
        assert_eq!(
            app.diff.as_ref().map(|diff| &diff.source),
            Some(&ReviewSource::commit(head))
        );
        assert_eq!(cursor_line(&app), Some(("src/lib.rs".to_owned(), Some(2))));
    }

    #[test]
    fn a_review_it_cannot_read_names_the_forms_it_takes() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let response = app.handle_mcp(McpRequestKind::Focus {
            target: code("src/lib.rs", None, Some("yesterday")),
            note: None,
        });
        assert!(
            matches!(&response, McpResponse::Error(err) if err.contains("commit:<rev>")),
            "{response:?}"
        );
        assert_eq!(app.screen(), Screen::Status, "nothing moved");
    }

    #[test]
    fn nothing_moves_while_the_human_types() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_file_picker();
        let response = app.handle_mcp(McpRequestKind::Focus {
            target: code("src/lib.rs", None, None),
            note: None,
        });
        assert!(matches!(response, McpResponse::Error(_)));
        assert!(app.diff.is_none());
    }

    #[test]
    fn a_walkthrough_id_opens_it_in_its_own_layout() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "the answer".to_owned(),
                stops: vec![StopParams {
                    id: None,
                    title: "Bump it".to_owned(),
                    anchor: Some("src/lib.rs#answer".to_owned()),
                    body: "- We return 42.".to_owned(),
                    notes: None,
                }],
                skipped: None,
                summary: None,
            })
        else {
            panic!("published");
        };

        focus(&mut app, FocusTarget::Id(published.id.clone()));

        let diff = app.diff.as_ref().expect("a diff");
        assert_eq!(diff.source, ReviewSource::walkthrough(published.id));
        assert_eq!(diff.layout, FileLayout::Walkthrough);
    }

    #[test]
    fn a_pr_the_list_has_not_named_yet_waits_for_it_then_lands() {
        let fixture = Fixture::new();
        fixture.write("base.rs", "pub fn base() {}\n");
        fixture.commit_all("base");
        fixture.branch("feature");
        fixture.checkout("feature");
        fixture.write("pr_only.rs", "pub fn only_in_pr() -> u32 {\n    7\n}\n");
        fixture.commit_all("add pr_only.rs");
        fixture.checkout("main");
        std::fs::remove_file(fixture.root.join("pr_only.rs")).expect("remove");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.ci_remotes = vec![crate::app::CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }];

        let focused = focus(&mut app, code("pr_only.rs", Some((2, 2)), Some("pr:7")));
        assert_eq!(focused.view, FocusView::Waiting);
        assert!(matches!(app.pending_ci, Some(crate::app::CiRequest::Prs)));

        let head_oid = app.review.vcs.resolve("feature").expect("head");
        app.on_prs_event(vec![crate::ci::PullRequest {
            number: 7,
            title: "add pr_only.rs".into(),
            url: None,
            base_ref: "main".into(),
            head_ref: "feature".into(),
            head_oid,
            author: "reviewer".into(),
        }]);

        assert_eq!(
            app.diff.as_ref().map(|diff| &diff.source),
            Some(&ReviewSource::pr(7))
        );
        assert_eq!(cursor_line(&app), Some(("pr_only.rs".to_owned(), Some(2))));
        assert!(app.pending_focus.is_none());
    }

    fn pr_fixture() -> Fixture {
        let fixture = Fixture::new();
        fixture.write("base.rs", "pub fn base() {}\n");
        fixture.commit_all("base");
        fixture.branch("feature");
        fixture.checkout("feature");
        fixture.write("pr_only.rs", "pub fn only_in_pr() -> u32 {\n    7\n}\n");
        fixture.commit_all("add pr_only.rs");
        fixture.checkout("main");
        std::fs::remove_file(fixture.root.join("pr_only.rs")).expect("remove");
        fixture
    }

    fn with_forge(app: &mut App) {
        app.ci_remotes = vec![crate::app::CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }];
    }

    fn publish(app: &mut App, anchor: &str) -> String {
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "a walk".to_owned(),
                stops: vec![StopParams {
                    id: None,
                    title: "Look".to_owned(),
                    anchor: Some(anchor.to_owned()),
                    body: "- We look here.".to_owned(),
                    notes: None,
                }],
                skipped: None,
                summary: None,
            })
        else {
            panic!("published");
        };
        published.id
    }

    #[test]
    fn a_failed_pr_list_forgets_the_focus_waiting_on_it() {
        let fixture = pr_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        with_forge(&mut app);
        focus(&mut app, code("pr_only.rs", None, Some("pr:7")));
        assert!(app.pending_focus.is_some());

        app.on_ci_prs_error("gh: not logged in".to_owned());

        assert!(
            app.pending_focus.is_none(),
            "a later list must not yank the view"
        );
    }

    #[test]
    fn a_walkthrough_about_a_pr_no_longer_open_is_refused() {
        let fixture = pr_fixture();
        let id = {
            let mut app = App::new(fixture.review(), LoadedConfig::default());
            let base = app.review.vcs.resolve("main").expect("base");
            let head = app.review.vcs.resolve("feature").expect("head");
            app.open_pr_diff(7, &base, &head);
            publish(&mut app, "pr_only.rs#only_in_pr")
        };
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        with_forge(&mut app);
        app.on_prs_event(Vec::new());

        let response = app.handle_mcp(McpRequestKind::Focus {
            target: FocusTarget::Id(id),
            note: None,
        });

        assert!(matches!(response, McpResponse::Error(_)), "{response:?}");
        assert!(app.pending_focus.is_none());
        assert!(app.pending_walkthrough_open.is_none());
    }

    #[test]
    fn a_stop_outside_the_diff_opens_its_slide() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let id = publish(&mut app, "notes.txt:1");
        let McpResponse::Walkthrough(Some(walkthrough)) =
            app.handle_mcp(McpRequestKind::GetWalkthrough {
                id: Some(id.clone()),
            })
        else {
            panic!("the walkthrough");
        };
        let stop = walkthrough.stops[0].id.clone();

        let focused = focus(&mut app, FocusTarget::Id(stop));

        assert_eq!(focused.view, FocusView::Diff);
        assert_eq!(app.screen(), Screen::Diff);
        assert!(app.pending_file.is_none(), "no file view over the slide");
        assert_eq!(
            app.diff.as_ref().map(|diff| &diff.source),
            Some(&ReviewSource::walkthrough(id))
        );
    }

    #[test]
    fn a_line_outside_the_hunks_never_lands_on_a_deleted_line_of_that_number() {
        let fixture = Fixture::new();
        let lines: Vec<String> = (1..=30).map(|n| format!("line {n}")).collect();
        fixture.write("data.txt", &(lines.join("\n") + "\n"));
        fixture.commit_all("base");
        let mut edited: Vec<String> = (1..=5).map(|n| format!("new {n}")).collect();
        edited.extend(lines.iter().filter(|line| *line != "line 25").cloned());
        fixture.write("data.txt", &(edited.join("\n") + "\n"));
        let mut app = App::new(fixture.review(), LoadedConfig::default());

        let focused = focus(&mut app, code("data.txt", Some((25, 25)), None));

        assert_eq!(focused.view, FocusView::File, "new line 25 is old line 20");
    }

    #[test]
    fn a_parked_draft_keeps_the_human_in_its_review() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        focus(&mut app, code("src/lib.rs", Some((2, 2)), None));
        app.comment_at_cursor();
        let diff = app.diff.as_mut().expect("a diff");
        let mut draft = diff.composer.take().expect("a composer");
        draft.buffer = "half a thought".to_owned();
        diff.parked_drafts.push(draft);

        let response = app.handle_mcp(McpRequestKind::Focus {
            target: code("src/lib.rs", None, Some("commit:HEAD")),
            note: None,
        });
        assert!(matches!(response, McpResponse::Error(_)), "{response:?}");
        assert_eq!(
            app.diff.as_ref().map(|diff| diff.parked_drafts.len()),
            Some(1)
        );

        let same_review = focus(&mut app, code("todo.md", None, None));
        assert_eq!(
            same_review.view,
            FocusView::Diff,
            "moving inside it is fine"
        );
    }
}
