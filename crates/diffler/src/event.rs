//! Terminal events and a periodic tick on one channel. Every decision lives in
//! `App::handle`, which is what the tests drive.

use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream, KeyEvent, MouseEvent};
use futures_util::StreamExt as _;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::mcp::McpRequest;

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize,
    /// DEC mode 1004. A terminal without focus reporting never sends it, so
    /// the app starts out focused.
    Focus(bool),
    Tick,
    RepoChanged,
    Enriched(Box<crate::app::enrich::EnrichOutcome>),
    FileLoaded {
        result: Box<Result<crate::app::file::FileView, String>>,
        /// Rows the request pointed at, 1-based and inclusive.
        span: Option<(u32, u32)>,
        reload: bool,
        /// The request this answers; a stale one is dropped on arrival.
        token: u64,
    },
    /// A path is absent when the repo's attributes say nothing about it.
    DeclaredKinds {
        kinds: std::collections::HashMap<String, diffler_core::classify::Kind>,
        /// The request this answers; a stale one is dropped on arrival.
        token: u64,
    },
    RepoStats {
        stats: Box<diffler_core::stats::RepoStats>,
        /// The request this answers; a stale one is dropped on arrival.
        token: u64,
    },
    /// The files the walkthrough's anchors name.
    WalkthroughAnchors {
        contents: std::collections::HashMap<String, String>,
        /// The pinned revision no longer resolves, so every file came from the worktree.
        pin_broken: bool,
        /// The request this answers; a stale one is dropped on arrival.
        token: u64,
    },
    RefreshDone(Box<Result<diffler_core::review::Refreshed, String>>),
    /// The re-diff a live algorithm switch queued.
    RediffDone {
        result: Box<Result<diffler_core::review::Refreshed, String>>,
        request: crate::app::RediffRequest,
    },
    Lens {
        token: u64,
        lens: Box<crate::app::Lens>,
    },
    ImagePreview {
        token: u64,
        preview: Box<crate::app::image::ImagePreview>,
    },
    /// Routed through the channel so the app stays the single owner of review state.
    Mcp(McpRequest),
    /// `wait_for_feedback` started a poll that ends by `until` at the latest.
    McpWaiting {
        until: Instant,
    },
    /// A shelled-out network git op (`app::GitOp`) finished.
    GitDone {
        label: String,
        ok: bool,
        output: String,
    },
    CiRuns(Vec<crate::ci::CiRun>),
    PrComments {
        number: u64,
        comments: Vec<crate::ci::PrComment>,
        /// So the same poll that syncs comments notices a force-push; `None`
        /// when the lookup failed.
        pr: Option<crate::ci::PullRequest>,
    },
    PrPosted {
        post: Box<crate::app::pr::PrPost>,
        result: Result<Option<crate::ci::PrComment>, String>,
    },
    CiPrs(Vec<crate::ci::PullRequest>),
    CiPr(Option<crate::ci::PullRequest>),
    PrCreated(Box<Result<crate::ci::PullRequest, String>>),
    CiRunDetail(crate::ci::RunDetail),
    CiExtras(crate::ci::RunExtras),
    CiLog {
        text: String,
        steps: Vec<crate::ci::LogStepMeta>,
        next_offset: u64,
        done: bool,
    },
    CiError(String),
    /// Kept apart from `CiError` so the in-flight guard frees only the PR-list slot.
    CiPrsError(String),
    Quit,
}

pub(crate) const TICK: Duration = Duration::from_millis(250);

pub fn spawn_event_loop(tx: UnboundedSender<AppEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let event = tokio::select! {
                _ = tick.tick() => Some(AppEvent::Tick),
                event = events.next() => match event {
                    Some(Ok(Event::Key(key))) => Some(AppEvent::Key(key)),
                    Some(Ok(Event::Mouse(mouse))) => Some(AppEvent::Mouse(mouse)),
                    Some(Ok(Event::Resize(_, _))) => Some(AppEvent::Resize),
                    Some(Ok(Event::FocusGained)) => Some(AppEvent::Focus(true)),
                    Some(Ok(Event::FocusLost)) => Some(AppEvent::Focus(false)),
                    Some(Ok(_)) => None,
                    Some(Err(_)) | None => {
                        let _ = tx.send(AppEvent::Quit);
                        return;
                    }
                },
            };
            if let Some(event) = event
                && tx.send(event).is_err()
            {
                return;
            }
        }
    })
}
