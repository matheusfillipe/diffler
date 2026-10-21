//! A pull request's own page: its description and the comments and reviews on
//! the PR as a whole, which its diff has no line for. It sits between the PR
//! list and the PR's diff.

use diffler_core::highlight::Highlighter;

use super::pr::PrPost;
use super::{App, CiRequest, Flow, InputOp, Screen};
use crate::app::markdown::{self, MdSpan};
use crate::ci::{PrConversation, PrNote, PullRequest};
use crate::keymap::Action;

/// What a row belongs to, so a key acts on the whole card the cursor is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrItem {
    Files,
    Description,
    Note(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrLine {
    /// State, author, branches and labels.
    Meta,
    Files,
    Blank,
    Section {
        title: &'static str,
        count: Option<usize>,
    },
    /// A card's author row; `None` is the description, written by the PR's author.
    Head(Option<usize>),
    Body(Vec<MdSpan>),
    Loading,
    Failed(String),
    NoDescription,
    NoNotes,
}

pub(crate) struct PrPage {
    pub pr: PullRequest,
    pub conversation: Option<PrConversation>,
    pub error: Option<String>,
    pub cursor: usize,
    pub scroll: usize,
    pub viewport: u16,
    /// A comment the forge refused, so the next `c` starts from its text.
    pub unsent: Option<String>,
    lines: Vec<(Option<PrItem>, PrLine)>,
    lines_width: u16,
    stale: bool,
}

impl PrPage {
    fn new(pr: PullRequest) -> Self {
        Self {
            pr,
            conversation: None,
            error: None,
            cursor: 1,
            scroll: 0,
            viewport: 0,
            unsent: None,
            lines: Vec::new(),
            lines_width: 0,
            stale: true,
        }
    }

    pub(crate) fn number(&self) -> u64 {
        self.pr.number
    }

    /// The rows at `width`, rebuilt only when the width or the conversation changed.
    pub(crate) fn lines(
        &mut self,
        width: u16,
        highlighter: &Highlighter,
    ) -> &[(Option<PrItem>, PrLine)] {
        if self.stale || self.lines_width != width {
            self.lines = layout(
                self.conversation.as_ref(),
                self.error.as_deref(),
                width,
                highlighter,
            );
            self.lines_width = width;
            self.stale = false;
            self.cursor = self.cursor.min(self.lines.len().saturating_sub(1));
        }
        &self.lines
    }

    /// The rows [`Self::lines`] last built.
    pub(crate) fn cached_lines(&self) -> &[(Option<PrItem>, PrLine)] {
        &self.lines
    }

    /// The old rows stay until the next draw rebuilds them, so a key handled
    /// before that draw still finds the card under the cursor.
    fn invalidate(&mut self) {
        self.stale = true;
    }

    pub(crate) fn item_at_cursor(&self) -> Option<PrItem> {
        self.lines.get(self.cursor).and_then(|(item, _)| *item)
    }

    fn note_at_cursor(&self) -> Option<&PrNote> {
        let Some(PrItem::Note(index)) = self.item_at_cursor() else {
            return None;
        };
        self.conversation.as_ref()?.notes.get(index)
    }

    /// Each row's words, so `/` finds a comment by what it says or who wrote it.
    pub(crate) fn search_rows(&self) -> Vec<(usize, String)> {
        let notes = self.conversation.as_ref().map(|c| c.notes.as_slice());
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(at, (_, line))| {
                let text = match line {
                    PrLine::Body(spans) => spans.iter().map(|span| span.text.as_str()).collect(),
                    PrLine::Head(Some(index)) => notes?.get(*index)?.author.clone(),
                    PrLine::Head(None) => self.pr.author.clone(),
                    _ => return None,
                };
                Some((at, text))
            })
            .collect()
    }

    fn last_line(&self) -> usize {
        self.lines.len().saturating_sub(1)
    }

    /// The first row of the next card or the one before, from anywhere inside the cursor's own.
    fn step_item(&mut self, forward: bool) {
        let here = self.item_at_cursor();
        let card_start = |(item, line): &(Option<PrItem>, PrLine)| {
            item.is_some() && *item != here && matches!(line, PrLine::Head(_) | PrLine::Files)
        };
        if let Some(target) = super::step_to(&self.lines, self.cursor, forward, card_start) {
            self.cursor = target;
        }
    }
}

fn layout(
    conversation: Option<&PrConversation>,
    error: Option<&str>,
    width: u16,
    highlighter: &Highlighter,
) -> Vec<(Option<PrItem>, PrLine)> {
    let mut lines = vec![
        (None, PrLine::Meta),
        (Some(PrItem::Files), PrLine::Files),
        (None, PrLine::Blank),
    ];
    let Some(conversation) = conversation else {
        lines.push((
            None,
            error.map_or(PrLine::Loading, |err| PrLine::Failed(err.to_owned())),
        ));
        return lines;
    };
    let budget = super::composer::card_budget(width);
    lines.push((
        None,
        PrLine::Section {
            title: "Description",
            count: None,
        },
    ));
    lines.push((Some(PrItem::Description), PrLine::Head(None)));
    if conversation.body.trim().is_empty() {
        lines.push((Some(PrItem::Description), PrLine::NoDescription));
    } else {
        push_body(
            &mut lines,
            PrItem::Description,
            &conversation.body,
            budget,
            highlighter,
        );
    }
    lines.push((None, PrLine::Blank));
    lines.push((
        None,
        PrLine::Section {
            title: "Conversation",
            count: Some(conversation.notes.len()),
        },
    ));
    if conversation.notes.is_empty() {
        lines.push((None, PrLine::NoNotes));
    }
    for (index, note) in conversation.notes.iter().enumerate() {
        let item = PrItem::Note(index);
        let budget = if note.reply_to.is_some() {
            budget.saturating_sub(super::REPLY_LANE).max(8)
        } else {
            budget
        };
        lines.push((Some(item), PrLine::Head(Some(index))));
        push_body(&mut lines, item, &note.body, budget, highlighter);
        lines.push((None, PrLine::Blank));
    }
    lines
}

fn push_body(
    lines: &mut Vec<(Option<PrItem>, PrLine)>,
    item: PrItem,
    body: &str,
    budget: usize,
    highlighter: &Highlighter,
) {
    for logical in markdown::parse(body, Some(highlighter), budget) {
        lines.extend(
            markdown::wrap(&logical, budget, budget)
                .into_iter()
                .map(|spans| (Some(item), PrLine::Body(spans))),
        );
    }
}

impl App {
    /// The PR's page, its conversation loading behind it. Enter there opens the diff.
    pub(crate) fn open_pr_page(&mut self, pr: PullRequest) {
        let number = pr.number;
        self.pr_pages.push(PrPage::new(pr));
        self.push_screen(Screen::Pr);
        self.pending_ci = Some(CiRequest::PrConversation(number));
    }

    pub(crate) fn pr_page(&self) -> Option<&PrPage> {
        self.pr_pages.last()
    }

    pub(crate) fn pr_page_mut(&mut self) -> Option<&mut PrPage> {
        self.pr_pages.last_mut()
    }

    pub(crate) fn on_pr_conversation(
        &mut self,
        number: u64,
        result: &Result<Box<PrConversation>, String>,
    ) -> Flow {
        let mut seen = false;
        for page in self.pr_pages.iter_mut().filter(|p| p.number() == number) {
            seen = true;
            match result {
                Ok(conversation) => {
                    page.pr = conversation.pr.clone();
                    page.conversation = Some((**conversation).clone());
                    page.error = None;
                }
                // a failed poll keeps what the page already shows
                Err(err) if page.conversation.is_none() => page.error = Some(err.clone()),
                Err(_) => {}
            }
            page.invalidate();
        }
        self.answer_pr_waiters(number, result);
        if seen {
            self.queue_avatars();
        }
        Flow::Continue
    }

    pub(crate) fn dispatch_pr_page(&mut self, action: Action) {
        let Some(page) = self.pr_pages.last_mut() else {
            return;
        };
        let last = page.last_line();
        let viewport = page.viewport;
        let step = |full| super::page_step(viewport, full);
        match action {
            Action::MoveDown => page.cursor = (page.cursor + 1).min(last),
            Action::MoveUp => page.cursor = page.cursor.saturating_sub(1),
            Action::GoTop => page.cursor = 0,
            Action::GoBottom => page.cursor = last,
            Action::HalfPageDown => page.cursor = (page.cursor + step(false)).min(last),
            Action::FullPageDown => page.cursor = (page.cursor + step(true)).min(last),
            Action::HalfPageUp => page.cursor = page.cursor.saturating_sub(step(false)),
            Action::FullPageUp => page.cursor = page.cursor.saturating_sub(step(true)),
            Action::NextSection => page.step_item(true),
            Action::PrevSection => page.step_item(false),
            // a page an agent opened by number learns the head from its first load
            Action::Open | Action::OpenPrDiff if page.pr.head_oid.is_empty() => {
                self.info("wait for the pull request to load, then open its diff");
            }
            Action::Open | Action::OpenPrDiff => {
                let pr = page.pr.clone();
                self.open_pr_review_for(pr);
            }
            Action::Refresh => {
                self.pending_ci = Some(CiRequest::PrConversation(page.number()));
            }
            Action::Comment => self.compose_pr_note(None),
            Action::Reply => match page.note_at_cursor().cloned() {
                Some(note) => self.compose_pr_note(Some(note)),
                None => self.info("move to a comment to reply, or c to add one"),
            },
            Action::BranchCheckout => {
                let pr = page.pr.clone();
                self.checkout_pr(&pr);
            }
            Action::PrWorktree => {
                let pr = page.pr.clone();
                self.pr_worktree(&pr);
            }
            Action::CopyUrl => {
                let (number, url) = (page.number(), page.pr.url.clone());
                self.copy_or_report(url, &format!("#{number}"));
            }
            Action::OpenUrl => {
                let (number, url) = (page.number(), page.pr.url.clone());
                self.open_or_report(url, &format!("#{number}"));
            }
            _ => {}
        }
    }

    fn compose_pr_note(&mut self, reply_to: Option<PrNote>) {
        let Some(page) = self.pr_pages.last_mut() else {
            return;
        };
        let number = page.number();
        let draft = page.unsent.take().unwrap_or_default();
        let title = match &reply_to {
            Some(note) => format!("Reply to {} on #{number}", note.author),
            None => format!("Comment on #{number}"),
        };
        self.open_input(
            title,
            draft,
            InputOp::PrNote {
                number,
                reply_to: reply_to.map(Box::new),
            },
        );
    }

    pub(crate) fn queue_pr_note(&mut self, number: u64, body: String, reply_to: Option<PrNote>) {
        self.queue_pr_post(PrPost::Note {
            number,
            body,
            reply_to: reply_to.map(Box::new),
        });
        self.info(format!("posting your comment on #{number}…"));
    }

    pub(crate) fn on_pr_note_posted(
        &mut self,
        number: u64,
        body: &str,
        result: Result<(), String>,
    ) {
        match result {
            Ok(()) => {
                self.info(format!("commented on #{number}"));
                self.pending_ci = Some(CiRequest::PrConversation(number));
            }
            Err(err) => {
                if let Some(page) = self
                    .pr_pages
                    .iter_mut()
                    .rev()
                    .find(|p| p.number() == number)
                {
                    page.unsent = Some(body.to_owned());
                }
                self.error(format!(
                    "posting to #{number} failed: {err}; c to try again"
                ));
            }
        }
    }

    /// Every forge login the open PR pages show, for their avatars.
    pub(crate) fn pr_page_logins(&self) -> Vec<String> {
        let mut logins: Vec<String> = self
            .pr_pages
            .iter()
            .filter_map(|page| page.conversation.as_ref())
            .flat_map(|conversation| {
                std::iter::once(conversation.pr.author.clone())
                    .chain(conversation.notes.iter().map(|note| note.author.clone()))
            })
            .filter(|login| !login.is_empty())
            .collect();
        logins.sort();
        logins.dedup();
        logins
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ci::PrVerdict;
    use crate::config::LoadedConfig;
    use crate::mcp::{McpRequestKind, McpResponse};
    use crate::test_support::standard_fixture;

    pub(crate) fn pr(number: u64) -> PullRequest {
        PullRequest {
            number,
            title: "Add widgets".to_owned(),
            url: Some(format!("https://github.com/acme/widgets/pull/{number}")),
            base_ref: "main".to_owned(),
            head_ref: "feat/widgets".to_owned(),
            head_oid: "0".repeat(40),
            author: "author".to_owned(),
        }
    }

    pub(crate) fn note(id: &str, author: &str, body: &str) -> PrNote {
        PrNote {
            id: id.to_owned(),
            author: author.to_owned(),
            body: body.to_owned(),
            at: 0,
            verdict: None,
            thread_id: None,
            reply_to: None,
        }
    }

    pub(crate) fn conversation(number: u64) -> PrConversation {
        let mut approved = note("r1", "lead", "ship it");
        approved.verdict = Some(PrVerdict::Approved);
        PrConversation {
            pr: pr(number),
            body: "Adds **widgets**.\n\nSecond paragraph.".to_owned(),
            state: crate::ci::PrState::Open,
            labels: vec!["backend".to_owned()],
            changed_files: Some(2),
            additions: Some(52),
            deletions: Some(3),
            notes: vec![note("c1", "reviewer", "why this way?"), approved],
        }
    }

    fn app_on_page() -> (crate::test_support::Fixture, App) {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_pr_page(pr(7));
        app.on_pr_conversation(7, &Ok(Box::new(conversation(7))));
        let highlighter = std::sync::Arc::clone(&app.highlighter);
        if let Some(page) = app.pr_page_mut() {
            page.lines(80, &highlighter);
        }
        (fixture, app)
    }

    #[test]
    fn the_cursor_starts_on_the_files_row_so_enter_opens_the_diff() {
        let (_fixture, app) = app_on_page();
        assert_eq!(
            app.pr_page().and_then(PrPage::item_at_cursor),
            Some(PrItem::Files)
        );
    }

    #[test]
    fn brackets_step_card_by_card() {
        let (_fixture, mut app) = app_on_page();
        let item = |app: &App| app.pr_page().and_then(PrPage::item_at_cursor);
        app.dispatch_pr_page(Action::NextSection);
        assert_eq!(item(&app), Some(PrItem::Description));
        app.dispatch_pr_page(Action::NextSection);
        assert_eq!(item(&app), Some(PrItem::Note(0)));
        app.dispatch_pr_page(Action::NextSection);
        assert_eq!(item(&app), Some(PrItem::Note(1)));
        app.dispatch_pr_page(Action::MoveDown);
        app.dispatch_pr_page(Action::PrevSection);
        assert_eq!(
            item(&app),
            Some(PrItem::Note(0)),
            "from inside a card back to the one before"
        );
    }

    #[test]
    fn reply_quotes_the_note_under_the_cursor() {
        let (_fixture, mut app) = app_on_page();
        app.dispatch_pr_page(Action::NextSection);
        app.dispatch_pr_page(Action::NextSection);
        app.dispatch_pr_page(Action::Reply);
        let Some(super::super::Modal::Input {
            title, on_submit, ..
        }) = &app.modal
        else {
            panic!("the input opens");
        };
        assert_eq!(title, "Reply to reviewer on #7");
        assert!(matches!(
            on_submit,
            InputOp::PrNote { number: 7, reply_to: Some(note) } if note.id == "c1"
        ));
    }

    #[test]
    fn a_refused_comment_comes_back_on_the_next_c() {
        let (_fixture, mut app) = app_on_page();
        app.on_pr_note_posted(7, "my text", Err("forbidden".to_owned()));
        app.dispatch_pr_page(Action::Comment);
        assert!(matches!(
            &app.modal,
            Some(super::super::Modal::Input { buffer, .. }) if buffer == "my text"
        ));
    }

    fn on_github(app: &mut App) {
        app.ci_remotes = vec![crate::app::CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }];
    }

    fn ask(app: &mut App, kind: McpRequestKind) -> tokio::sync::oneshot::Receiver<McpResponse> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        app.handle(crate::event::AppEvent::Mcp(crate::mcp::McpRequest {
            kind,
            project: None,
            reply,
        }));
        answer
    }

    #[test]
    fn get_pr_answers_once_the_forge_does() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        on_github(&mut app);
        let mut answer = ask(&mut app, McpRequestKind::GetPr { number: Some(7) });
        assert!(
            answer.try_recv().is_err(),
            "nothing until the forge answers"
        );
        assert!(matches!(app.pending_ci, Some(CiRequest::PrConversation(7))));
        app.handle(crate::event::AppEvent::PrConversation {
            number: 7,
            result: Ok(Box::new(conversation(7))),
        });
        let Ok(McpResponse::Pr(info)) = answer.try_recv() else {
            panic!("the waiting call gets the conversation");
        };
        assert_eq!(info.body, "Adds **widgets**.\n\nSecond paragraph.");
        let verdicts: Vec<Option<&str>> =
            info.comments.iter().map(|c| c.verdict.as_deref()).collect();
        assert_eq!(verdicts, [None, Some("approved")]);
    }

    #[test]
    fn get_pr_reads_the_page_on_screen_without_a_fetch() {
        let (_fixture, mut app) = app_on_page();
        on_github(&mut app);
        app.pending_ci = None;
        let mut answer = ask(&mut app, McpRequestKind::GetPr { number: None });
        assert!(matches!(answer.try_recv(), Ok(McpResponse::Pr(info)) if info.number == 7));
        assert!(app.pending_ci.is_none());
    }

    #[test]
    fn focus_opens_a_pr_page_the_list_never_loaded() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        on_github(&mut app);
        let response = app.handle_mcp(McpRequestKind::Focus {
            target: crate::mcp::FocusTarget::Pr(42),
            note: None,
        });
        assert!(matches!(
            response,
            McpResponse::Focused(ref focused) if focused.view == crate::mcp::FocusView::Pr
        ));
        assert_eq!(app.screen(), Screen::Pr);
        assert_eq!(app.pr_page().map(PrPage::number), Some(42));
        app.dispatch_pr_page(Action::Open);
        assert_eq!(
            app.screen(),
            Screen::Pr,
            "no head yet, so the diff waits for the load"
        );
        assert!(
            app.pending_git.is_none(),
            "no fetch for a head we do not know"
        );
        assert!(app.pending_pr_open.is_none());
    }

    #[test]
    fn enter_in_the_pr_list_opens_the_page_and_loads_its_conversation() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        on_github(&mut app);
        app.prs = vec![pr(7)];
        app.push_screen(Screen::Prs);
        app.dispatch_prs(Action::Open);
        assert_eq!(app.screen(), Screen::Pr);
        assert!(matches!(app.pending_ci, Some(CiRequest::PrConversation(7))));
    }

    #[test]
    fn leaving_the_page_drops_it() {
        let (_fixture, mut app) = app_on_page();
        app.dispatch(Action::Back);
        assert!(app.pr_pages.is_empty());
        assert_ne!(app.screen(), Screen::Pr);
    }
}
