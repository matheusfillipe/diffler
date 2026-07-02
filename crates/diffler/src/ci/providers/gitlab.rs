//! GitLab CI adapter (CLI-only via `glab api`, REST). The dependency graph is
//! derived from pipeline stages (jobs in a stage depend on all jobs in the
//! previous stage) — GitLab's default pipeline view. The exact `needs` DAG is a
//! GraphQL refinement left for later. Logs poll the job trace by offset.

use async_trait::async_trait;
use serde::Deserialize;

use crate::ci::error::{CiError, Result};
use crate::ci::exec::CommandRunner;
use crate::ci::model::{
    Capabilities, CiJob, CiRun, DagSource, JobId, JobStatus, LogChunk, LogMode, PullRequest,
    RunDetail, RunExtras, RunId,
};
use crate::ci::provider::{CiProvider, ProviderKind};

/// Talks to GitLab CI through `glab api`. `glab` resolves the project from the
/// repo via the `:fullpath` placeholder; an explicit `host` targets a
/// self-hosted instance.
pub struct GitLabProvider {
    runner: Box<dyn CommandRunner>,
    host: Option<String>,
}

impl GitLabProvider {
    pub fn new(runner: Box<dyn CommandRunner>, host: Option<String>) -> Self {
        Self { runner, host }
    }

    /// `glab api <path>`, with `--hostname` when a self-hosted host is set.
    async fn api(&self, path: &str) -> Result<String> {
        let mut args = vec!["api".to_owned()];
        if let Some(host) = &self.host {
            args.push("--hostname".to_owned());
            args.push(host.clone());
        }
        args.push(path.to_owned());
        self.runner.run("glab", &args).await
    }
}

#[async_trait]
impl CiProvider for GitLabProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitLab
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            dag: DagSource::RunApi,
            logs: LogMode::Poll,
        }
    }

    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>> {
        let path = format!("projects/:fullpath/pipelines?per_page={limit}");
        let out = self.api(&path).await?;
        let raw: Vec<PipelineItem> = serde_json::from_str(&out).map_err(|e| CiError::Parse {
            what: "glab pipelines".into(),
            message: e.to_string(),
        })?;
        Ok(raw.into_iter().map(PipelineItem::into_run).collect())
    }

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail> {
        let meta = self
            .api(&format!("projects/:fullpath/pipelines/{}", run.0))
            .await?;
        let pipeline: PipelineItem = serde_json::from_str(&meta).map_err(|e| CiError::Parse {
            what: "glab pipeline".into(),
            message: e.to_string(),
        })?;
        let jobs_out = self
            .api(&format!("projects/:fullpath/pipelines/{}/jobs", run.0))
            .await?;
        let raw: Vec<JobItem> = serde_json::from_str(&jobs_out).map_err(|e| CiError::Parse {
            what: "glab jobs".into(),
            message: e.to_string(),
        })?;
        Ok(RunDetail {
            run: pipeline.into_run(),
            jobs: jobs_with_stage_edges(&raw),
        })
    }

    async fn job_log(&self, _run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk> {
        let trace = self
            .api(&format!("projects/:fullpath/jobs/{}/trace", job.0))
            .await?;
        let done = self
            .api(&format!("projects/:fullpath/jobs/{}", job.0))
            .await
            .ok()
            .and_then(|raw| serde_json::from_str::<JobState>(&raw).ok())
            .is_some_and(|job| {
                matches!(
                    map_status(&job.status),
                    JobStatus::Ok | JobStatus::Failed | JobStatus::Skipped | JobStatus::Neutral
                )
            });
        // resume from the saved offset, clamped to the end and floored to a char
        // boundary, so a multibyte split or a shrunk/replaced trace yields the
        // correct tail (or empty) instead of re-emitting the whole trace
        let mut start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(trace.len());
        while start > 0 && !trace.is_char_boundary(start) {
            start -= 1;
        }
        Ok(LogChunk {
            next_offset: trace.len() as u64,
            text: trace[start..].to_owned(),
            steps: Vec::new(),
            done,
        })
    }

    /// GitLab exposes neither run artifacts nor annotations through this adapter.
    async fn run_extras(&self, _run: &RunId) -> Result<RunExtras> {
        Ok(RunExtras::default())
    }

    /// Merge-request resolution isn't wired for the GitLab adapter yet.
    async fn current_pr(&self) -> Result<Option<PullRequest>> {
        Ok(None)
    }
}

/// Order jobs into stages (by first appearance) and link each job to every job
/// in the previous stage — GitLab's stage-sequenced pipeline graph.
fn jobs_with_stage_edges(raw: &[JobItem]) -> Vec<CiJob> {
    let mut stage_order: Vec<String> = Vec::new();
    for job in raw {
        if !stage_order.contains(&job.stage) {
            stage_order.push(job.stage.clone());
        }
    }
    let ids_in = |stage: &str| -> Vec<JobId> {
        raw.iter()
            .filter(|j| j.stage == stage)
            .map(|j| JobId(j.id.to_string()))
            .collect()
    };
    raw.iter()
        .map(|job| {
            let stage_idx = stage_order.iter().position(|s| *s == job.stage);
            let needs = stage_idx
                .and_then(|i| i.checked_sub(1))
                .and_then(|prev| stage_order.get(prev))
                .map(|stage| ids_in(stage))
                .unwrap_or_default();
            CiJob {
                id: JobId(job.id.to_string()),
                name: job.name.clone(),
                status: map_status(&job.status),
                needs,
            }
        })
        .collect()
}

fn map_status(status: &str) -> JobStatus {
    match status {
        "success" => JobStatus::Ok,
        "failed" => JobStatus::Failed,
        "running" => JobStatus::Running,
        "canceled" | "canceling" => JobStatus::Neutral,
        "skipped" | "manual" => JobStatus::Skipped,
        _ => JobStatus::Queued,
    }
}

fn parse_created(raw: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

#[derive(Deserialize)]
struct PipelineItem {
    id: u64,
    status: String,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    sha: Option<String>,
    source: Option<String>,
    created_at: Option<String>,
    web_url: Option<String>,
}

impl PipelineItem {
    fn into_run(self) -> CiRun {
        CiRun {
            id: RunId(self.id.to_string()),
            name: self.source.unwrap_or_else(|| "pipeline".to_owned()),
            title: String::new(),
            branch: self.git_ref.unwrap_or_default(),
            commit: self.sha.unwrap_or_default(),
            author: String::new(),
            created: self.created_at.as_deref().and_then(parse_created),
            status: map_status(&self.status),
            url: self.web_url,
            remote: None,
        }
    }
}

#[derive(Deserialize)]
struct JobItem {
    id: u64,
    name: String,
    stage: String,
    status: String,
}

#[derive(Deserialize)]
struct JobState {
    status: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;

    fn provider(responses: &[(&'static str, &str)]) -> GitLabProvider {
        GitLabProvider::new(Box::new(RecordingRunner::new(responses)), None)
    }

    #[tokio::test]
    async fn list_runs_parses_pipelines() {
        let json = r#"[
          {"id":100,"status":"running","ref":"main","sha":"deadbeef","source":"push",
           "created_at":"2026-06-18T10:00:00Z","web_url":"https://gl/p/100"}
        ]"#;
        let runs = provider(&[("pipelines", json)])
            .list_runs(10)
            .await
            .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("100".into()));
        assert_eq!(runs[0].branch, "main");
        assert_eq!(runs[0].status, JobStatus::Running);
    }

    #[tokio::test]
    async fn run_detail_links_stages_into_a_dag() {
        let pipeline = r#"{"id":100,"status":"running","ref":"main","sha":"d","source":"push",
          "created_at":"2026-06-18T10:00:00Z","web_url":"u"}"#;
        let jobs = r#"[
          {"id":1,"name":"build","stage":"build","status":"success"},
          {"id":2,"name":"test","stage":"test","status":"running"},
          {"id":3,"name":"lint","stage":"test","status":"success"}
        ]"#;
        // "pipelines/100/jobs" must be matched before "pipelines/100"
        let detail = provider(&[("/jobs", jobs), ("pipelines/100", pipeline)])
            .run_detail(&RunId("100".into()))
            .await
            .expect("detail");
        assert_eq!(detail.jobs.len(), 3);
        // build is in the first stage → no upstream
        assert!(detail.jobs[0].needs.is_empty());
        // test-stage jobs depend on the build-stage job
        assert_eq!(detail.jobs[1].needs, vec![JobId("1".into())]);
        assert_eq!(detail.jobs[2].needs, vec![JobId("1".into())]);
        assert_eq!(detail.jobs[1].status, JobStatus::Running);
    }

    #[tokio::test]
    async fn job_log_marks_a_finished_job_done() {
        let chunk = provider(&[
            ("/trace", "all good"),
            ("jobs/7", r#"{"status":"success"}"#),
        ])
        .job_log(&RunId("1".into()), &JobId("7".into()), 0)
        .await
        .expect("log");
        assert!(chunk.done, "finished job stops the poll loop");

        let chunk = provider(&[("/trace", "..."), ("jobs/7", r#"{"status":"running"}"#)])
            .job_log(&RunId("1".into()), &JobId("7".into()), 0)
            .await
            .expect("log");
        assert!(!chunk.done);
    }

    #[tokio::test]
    async fn job_log_returns_the_tail_from_offset() {
        let chunk = provider(&[("/trace", "hello world")])
            .job_log(&RunId("100".into()), &JobId("1".into()), 6)
            .await
            .expect("log");
        assert_eq!(chunk.text, "world");
        assert_eq!(chunk.next_offset, 11);
    }

    #[tokio::test]
    async fn job_log_past_the_end_yields_empty_not_a_duplicate() {
        // a shrunk/replaced trace (offset > len) must not re-emit the whole trace
        let chunk = provider(&[("/trace", "short")])
            .job_log(&RunId("100".into()), &JobId("1".into()), 999)
            .await
            .expect("log");
        assert_eq!(chunk.text, "");
        assert_eq!(chunk.next_offset, 5);
    }
}
