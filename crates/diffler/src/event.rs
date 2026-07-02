//! Terminal event pump: crossterm's async stream plus a periodic tick,
//! multiplexed onto one channel. Kept thin — all decisions live in
//! `App::handle`, which is what the tests drive.

use std::time::Duration;

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
    Tick,
    /// Debounced filesystem change from the watcher (`watch` module).
    RepoChanged,
    /// A background enrichment (emphasis/highlight/scope) finished.
    Enriched(Box<crate::app::enrich::EnrichOutcome>),
    /// An off-thread repo refresh finished (status + working diff), or failed.
    RefreshDone(
        Box<
            Result<
                (
                    diffler_core::vcs::StatusModel,
                    diffler_core::model::DiffModel,
                ),
                String,
            >,
        >,
    ),
    /// Agent tool call routed through the event channel so the app stays
    /// the single owner of the review state (`mcp` module).
    Mcp(McpRequest),
    /// A shelled-out network git op finished (`app::GitOp`). The result returns
    /// as an event so the run loop keeps drawing while the process runs.
    GitDone {
        label: String,
        ok: bool,
        output: String,
    },
    /// Branch-scoped CI run list from a provider poll.
    CiRuns(Vec<crate::ci::CiRun>),
    /// The checked-out branch's PR, fetched once per branch (not every poll).
    CiPr(Option<crate::ci::PullRequest>),
    /// A run's jobs + dependency DAG, mapped onto the graph view.
    CiRunDetail(crate::ci::RunDetail),
    /// A run's artifacts + annotations for the graph page's extras panel.
    CiExtras(crate::ci::RunExtras),
    /// An incremental job-log slice from a provider poll, with the job's step
    /// boundaries (empty when the provider exposes none).
    CiLog {
        text: String,
        steps: Vec<crate::ci::LogStepMeta>,
        next_offset: u64,
        done: bool,
    },
    /// A CI provider call failed; surfaced as a status-bar message.
    CiError(String),
    Quit,
}

const TICK: Duration = Duration::from_millis(250);

/// Forward terminal events and ticks into `tx` until the terminal stream
/// ends or the receiver is dropped.
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
