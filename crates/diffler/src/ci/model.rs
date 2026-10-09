//! The provider-agnostic CI model every forge adapter normalizes into.

use time::OffsetDateTime;

/// A run or pipeline id as the provider spells it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(pub String);

/// Maps 1:1 to the graph component's `NodeStatus`.
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
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Failed => "×",
            Self::Running => "●",
            Self::Queued => "·",
            Self::Skipped => "–",
            Self::Neutral => "○",
        }
    }

    /// The more severe status, so one failing matrix leg marks the aggregate.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiRun {
    pub id: RunId,
    pub name: String,
    /// The triggering commit's subject, where the provider exposes one.
    pub title: String,
    pub branch: String,
    pub commit: String,
    pub author: String,
    pub created: Option<OffsetDateTime>,
    pub status: JobStatus,
    pub url: Option<String>,
    /// Set only when several remotes are aggregated.
    pub remote: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiJob {
    pub id: JobId,
    pub name: String,
    pub status: JobStatus,
    pub needs: Vec<JobId>,
    /// Time taken so far for a running job; `None` before it starts or where
    /// the forge reports no times.
    pub duration_secs: Option<i64>,
    /// The matrix legs when there was more than one; `status` and
    /// `duration_secs` stay the aggregate across them.
    pub legs: Vec<CiJobLeg>,
}

/// `name` holds the leg's matrix parameters alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiJobLeg {
    /// The run job this leg ran as, which its log is fetched by.
    pub id: JobId,
    pub name: String,
    pub status: JobStatus,
    pub duration_secs: Option<i64>,
}

/// `13s` or `1m03s`.
pub fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDetail {
    pub run: CiRun,
    pub jobs: Vec<CiJob>,
}

/// Forges expose no per-step log content, so the host buckets log lines into
/// steps by timestamp (`start_key` ≤ a line's timestamp).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStepMeta {
    pub name: String,
    pub status: JobStatus,
    /// [`ts_sort_key`] of the step's start, the lower bound of its log lines.
    pub start_key: u64,
    pub duration_secs: Option<i64>,
}

/// `next_offset` is where the next poll resumes; `done` means the log is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk {
    pub text: String,
    pub steps: Vec<LogStepMeta>,
    pub next_offset: u64,
    pub done: bool,
}

/// The first 14 digits of an ISO-8601 timestamp (`YYYYMMDDHHMMSS`), so a
/// fractional-second line key compares against a step key without parsing.
/// `0` when there are fewer.
#[must_use]
pub fn ts_sort_key(iso: &str) -> u64 {
    let digits: String = iso.chars().filter(char::is_ascii_digit).take(14).collect();
    if digits.len() == 14 {
        digits.parse().unwrap_or(0)
    } else {
        0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: Option<String>,
    pub base_ref: String,
    pub head_ref: String,
    /// The PR head commit at fetch time; the diff is `merge-base..head`.
    pub head_oid: String,
    pub author: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrComment {
    pub id: String,
    pub path: String,
    /// 1-based line on the side the comment anchors to; `None` for file-level.
    pub line: Option<u32>,
    /// First line of a multi-line comment's range.
    pub start_line: Option<u32>,
    /// Anchored to the new side (`true`) or the old side of the diff.
    pub new_side: bool,
    pub body: String,
    pub author: String,
    /// Forge id of the comment this replies to; `None` for thread roots.
    pub reply_to: Option<String>,
    /// What `resolve_pr_thread` takes; set on thread roots where the forge has one.
    pub thread_id: Option<String>,
    /// Roots only.
    pub resolved: bool,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub name: String,
    pub size_bytes: u64,
    /// Past its retention window: still listed, no longer downloadable.
    pub expired: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationLevel {
    Notice,
    Warning,
    Failure,
}

/// A `::warning`/`::error` workflow command or a check failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub level: AnnotationLevel,
    pub title: String,
    pub message: String,
    pub path: String,
    pub start_line: Option<u64>,
}

/// Shown below the DAG; empty where the provider exposes neither.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunExtras {
    pub artifacts: Vec<Artifact>,
    pub annotations: Vec<Annotation>,
}

/// The UI gates its affordances on these, so a missing feature hides rather than fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub dag: DagSource,
    pub logs: LogMode,
    /// Without it, a resolution stays local to the review session.
    pub resolve_threads: bool,
    /// Without it, a whole-file comment is held back from a submit.
    pub file_comments: bool,
}

/// Where a provider's dependency edges come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagSource {
    RunApi,
    /// The pipeline config file, parsed separately.
    ConfigFile,
    /// Rendered as a flat list.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    Stream,
    Poll,
    /// The whole log, once the job completes.
    Dump,
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
