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

impl PullRequest {
    /// `#number title`, how a list names the PR.
    #[must_use]
    pub fn headline(&self) -> String {
        format!("#{} {}", self.number, self.title)
    }
}

/// A PR as the PR list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPr {
    pub pr: PullRequest,
    pub summary: PrSummary,
}

impl From<PullRequest> for ListedPr {
    fn from(pr: PullRequest) -> Self {
        Self {
            pr,
            summary: PrSummary::default(),
        }
    }
}

/// What a forge's PR listing tells about each PR; a forge leaves out what its
/// listing lacks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrSummary {
    pub draft: bool,
    pub checks: Option<JobStatus>,
    /// The forge's overall review decision: approved or changes requested.
    pub review: Option<PrVerdict>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub comments: Option<u64>,
    pub labels: Vec<String>,
    /// Unix seconds; we store 0 when the forge gives no update time.
    pub updated: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Closed,
    Merged,
}

impl PrState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Draft => "draft",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }
}

/// What a review said about the PR as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrVerdict {
    Approved,
    ChangesRequested,
    Commented,
}

impl PrVerdict {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "requested changes",
            Self::Commented => "reviewed",
        }
    }
}

/// A comment on the PR as a whole, or a review's summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrNote {
    pub id: String,
    pub author: String,
    pub body: String,
    pub at: u64,
    /// Set on a review's summary.
    pub verdict: Option<PrVerdict>,
    /// The forge thread a reply posts into; only GitLab threads these.
    pub thread_id: Option<String>,
    /// The note this one answers inside its thread.
    pub reply_to: Option<String>,
}

/// Everything a PR says outside its diff, oldest note first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrConversation {
    pub pr: PullRequest,
    pub body: String,
    pub state: PrState,
    pub labels: Vec<String>,
    pub changed_files: Option<u64>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub notes: Vec<PrNote>,
    pub reviewers: Vec<PrReviewer>,
}

/// Someone asked to review the PR or who reviewed it; `verdict` is `None`
/// while their review is pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReviewer {
    pub login: String,
    pub verdict: Option<PrVerdict>,
}

/// Someone the forge lets us request a review from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerCandidate {
    pub login: String,
    /// The forge recommends them for this PR.
    pub suggested: bool,
}

/// Each reviewer's standing from their reviews and review requests, oldest
/// first; a `None` verdict is a request. A plain comment keeps an earlier
/// approval or change request, the way the forges count it, and a request made
/// after a review asks for a new one. The PR's author answering on their own
/// PR is no reviewer.
pub fn reviewers_from(
    author: &str,
    events: impl IntoIterator<Item = (String, Option<PrVerdict>)>,
) -> Vec<PrReviewer> {
    let mut reviewers: Vec<PrReviewer> = Vec::new();
    for (login, verdict) in events {
        if login.is_empty() || login == author {
            continue;
        }
        match reviewers.iter_mut().find(|r| r.login == login) {
            Some(known)
                if verdict == Some(PrVerdict::Commented)
                    && matches!(
                        known.verdict,
                        Some(PrVerdict::Approved | PrVerdict::ChangesRequested)
                    ) => {}
            Some(known) => known.verdict = verdict,
            None => reviewers.push(PrReviewer { login, verdict }),
        }
    }
    reviewers
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

    #[test]
    fn reviewers_keep_their_standing_and_a_new_request_reopens_it() {
        let events = [
            ("lead", Some(PrVerdict::Approved)),
            ("bot", Some(PrVerdict::Commented)),
            ("lead", Some(PrVerdict::Commented)),
            ("peer", Some(PrVerdict::ChangesRequested)),
            ("", Some(PrVerdict::Commented)),
            ("author", Some(PrVerdict::Commented)),
            ("peer", None),
            ("new", None),
        ]
        .map(|(login, verdict)| (login.to_owned(), verdict));
        let reviewers = reviewers_from("author", events);
        let standing: Vec<(&str, Option<PrVerdict>)> = reviewers
            .iter()
            .map(|r| (r.login.as_str(), r.verdict))
            .collect();
        assert_eq!(
            standing,
            [
                ("lead", Some(PrVerdict::Approved)),
                ("bot", Some(PrVerdict::Commented)),
                ("peer", None),
                ("new", None),
            ]
        );
    }
}
