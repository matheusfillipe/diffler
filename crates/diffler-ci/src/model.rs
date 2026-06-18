//! The provider-agnostic CI model. Adapters normalize each forge's runs, jobs,
//! dependency edges, and logs into these types; the host maps `RunDetail` onto a
//! `diffler_graph::Model` for rendering.

use time::OffsetDateTime;

/// A run/pipeline id as the provider spells it (a GitHub run database id, a
/// GitLab pipeline iid, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(pub String);

/// A job id within a run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(pub String);

/// Normalized job/run state, driving color and glyph. Maps 1:1 to the graph
/// component's `NodeStatus` at the host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Ok,
    Failed,
    Skipped,
    Neutral,
}

impl JobStatus {
    /// The more severe of two statuses, so one failing matrix leg dominates an
    /// aggregate (a run's status, a collapsed group).
    #[must_use]
    pub fn worse(self, other: Self) -> Self {
        let rank = |s: Self| match s {
            Self::Failed => 5,
            Self::Running => 4,
            Self::Queued => 3,
            Self::Skipped => 2,
            Self::Neutral => 1,
            Self::Ok => 0,
        };
        if rank(self) >= rank(other) {
            self
        } else {
            other
        }
    }
}

/// One run/pipeline as shown in the runs list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiRun {
    pub id: RunId,
    pub name: String,
    pub branch: String,
    pub commit: String,
    pub author: String,
    pub created: Option<OffsetDateTime>,
    pub status: JobStatus,
    pub url: Option<String>,
}

/// One job within a run, with its upstream dependencies (the DAG edges).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiJob {
    pub id: JobId,
    pub name: String,
    pub status: JobStatus,
    pub needs: Vec<JobId>,
}

/// A run plus its jobs — `jobs` + each job's `needs` is the dependency graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDetail {
    pub run: CiRun,
    pub jobs: Vec<CiJob>,
}

/// An incremental slice of a job log. `next_offset` is where the next poll
/// resumes; `done` is set once the job has finished and the log is complete.
/// This unifies streaming, polling, and one-shot-dump log sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk {
    pub text: String,
    pub next_offset: u64,
    pub done: bool,
}

/// What a provider can actually do, so the UI degrades honestly instead of
/// failing at runtime (hide the graph when `DagSource::None`, the follow toggle
/// when `LogMode::Dump`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub dag: DagSource,
    pub logs: LogMode,
}

/// Where a provider's dependency edges come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagSource {
    /// Edges are in the run/job API response.
    RunApi,
    /// Edges are only in the pipeline config file (parsed separately).
    ConfigFile,
    /// No dependency concept; render as a flat list.
    None,
}

/// How a provider delivers logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    /// A true follow/stream.
    Stream,
    /// Offset/range polling.
    Poll,
    /// Whole log available only once the job completes.
    Dump,
    /// No log access.
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worse_picks_the_more_severe_status() {
        assert_eq!(JobStatus::Ok.worse(JobStatus::Failed), JobStatus::Failed);
        assert_eq!(JobStatus::Running.worse(JobStatus::Ok), JobStatus::Running);
        assert_eq!(
            JobStatus::Queued.worse(JobStatus::Skipped),
            JobStatus::Queued
        );
        assert_eq!(JobStatus::Ok.worse(JobStatus::Ok), JobStatus::Ok);
    }
}
