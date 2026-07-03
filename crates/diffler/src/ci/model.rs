//! The provider-agnostic CI model. Adapters normalize each forge's runs, jobs,
//! dependency edges, and logs into these types; the host maps `RunDetail` onto a
//! `crate::graph::Model` for rendering.

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
    /// A compact status glyph for list/section rows.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Failed => "✗",
            Self::Running => "●",
            Self::Queued => "·",
            Self::Skipped => "–",
            Self::Neutral => "○",
        }
    }

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
    /// The run's headline (the triggering commit's subject), if the provider
    /// exposes one.
    pub title: String,
    pub branch: String,
    pub commit: String,
    pub author: String,
    pub created: Option<OffsetDateTime>,
    pub status: JobStatus,
    pub url: Option<String>,
    /// The git remote this run came from, set when several remotes are
    /// aggregated; `None` for a single-remote repo.
    pub remote: Option<String>,
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

/// One step of a job, for grouping the log into the same collapsible units the
/// forge UI shows. The public API exposes no per-step log *content*, so the host
/// buckets log lines into steps by timestamp (`start_key` ≤ a line's timestamp).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStepMeta {
    pub name: String,
    pub status: JobStatus,
    /// [`ts_sort_key`] of the step's start, the lower bound of its log lines.
    pub start_key: u64,
    /// Wall-clock seconds the step ran, when both endpoints are known.
    pub duration_secs: Option<i64>,
}

/// An incremental slice of a job log. `next_offset` is where the next poll
/// resumes; `done` is set once the job has finished and the log is complete.
/// `steps` carries the job's step boundaries when the provider exposes them
/// (empty otherwise). This unifies streaming, polling, and one-shot-dump sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk {
    pub text: String,
    pub steps: Vec<LogStepMeta>,
    pub next_offset: u64,
    pub done: bool,
}

/// A coarse chronological sort key from an ISO-8601 timestamp: its first 14
/// digits (`YYYYMMDDHHMMSS`), so a fractional-second line key compares against a
/// second-resolution step key without parsing. `0` when there aren't 14 digits.
#[must_use]
pub fn ts_sort_key(iso: &str) -> u64 {
    let digits: String = iso.chars().filter(char::is_ascii_digit).take(14).collect();
    if digits.len() == 14 {
        digits.parse().unwrap_or(0)
    } else {
        0
    }
}

/// An ISO-8601 UTC timestamp (`2026-07-03T17:00:00Z`) as unix seconds, via
/// the days-from-civil algorithm; anything unparseable is `None`.
#[must_use]
pub fn iso_epoch(iso: &str) -> Option<u64> {
    let (date, rest) = iso.split_once('T')?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let time: String = rest.chars().take(8).collect();
    let mut hms = time.splitn(3, ':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = if shifted_year >= 0 {
        shifted_year
    } else {
        shifted_year - 399
    } / 400;
    let year_of_era = shifted_year - era * 400;
    let month_shifted = (month + 9) % 12;
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()
}

/// The pull/merge request for the checked-out branch, shown beside the runs so
/// the section reflects "the branch and PR I'm on", not just a workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: Option<String>,
    /// Branch the PR merges into.
    pub base_ref: String,
    /// The PR head commit at fetch time; the diff is `merge-base..head`.
    pub head_oid: String,
}

/// A line-anchored PR review comment from the forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrComment {
    pub id: String,
    pub path: String,
    /// 1-based line on the side the comment anchors to; `None` for file-level.
    pub line: Option<u32>,
    /// Anchored to the new side (`true`) or the old side of the diff.
    pub new_side: bool,
    pub body: String,
    pub author: String,
    /// Forge id of the comment this replies to; `None` for thread roots.
    pub reply_to: Option<String>,
    pub at: u64,
}

/// A build artifact a run produced, as listed on the run page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub name: String,
    pub size_bytes: u64,
    /// Past its retention window: still listed, no longer downloadable.
    pub expired: bool,
}

/// Severity of a run annotation, driving its glyph and color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationLevel {
    Notice,
    Warning,
    Failure,
}

/// One annotation a job emitted (a `::warning`/`::error` workflow command or a
/// check failure), tied to a file location when the provider gives one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub level: AnnotationLevel,
    pub title: String,
    pub message: String,
    pub path: String,
    pub start_line: Option<u64>,
}

/// A run's page extras: the artifacts it produced and the annotations its jobs
/// emitted. Shown below the DAG; empty for providers that don't expose them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunExtras {
    pub artifacts: Vec<Artifact>,
    pub annotations: Vec<Annotation>,
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
