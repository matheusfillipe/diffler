//! Forgejo/Codeberg adapter. Forgejo exposes a GitHub-shaped Actions REST API,
//! fetched with `curl` through the same `CommandRunner` seam the other adapters
//! use — a public repo needs no token; a PAT is read from the environment.
//! Job logs and the dependency DAG aren't wired yet; `Capabilities` says so.

use async_trait::async_trait;
use serde::Deserialize;

use crate::ci::error::{CiError, Result};
use crate::ci::exec::CommandRunner;
use crate::ci::model::{
    Capabilities, CiJob, CiRun, DagSource, JobId, JobStatus, LogChunk, LogMode, PullRequest,
    RunDetail, RunExtras, RunId,
};
use crate::ci::provider::{CiProvider, ProviderKind};

pub struct ForgejoProvider {
    runner: Box<dyn CommandRunner>,
    host: String,
    /// `owner/name`.
    repo: String,
    token: Option<String>,
}

impl ForgejoProvider {
    pub fn new(
        runner: Box<dyn CommandRunner>,
        host: String,
        repo: String,
        token: Option<String>,
    ) -> Self {
        Self {
            runner,
            host,
            repo,
            token,
        }
    }

    async fn get(&self, path: &str) -> Result<String> {
        let mut args = vec![
            "-s".to_owned(),
            "--max-time".to_owned(),
            "20".to_owned(),
            "-H".to_owned(),
            "Accept: application/json".to_owned(),
        ];
        if let Some(token) = &self.token {
            args.push("-H".to_owned());
            args.push(format!("Authorization: token {token}"));
        }
        args.push(format!(
            "https://{}/api/v1/repos/{}/{path}",
            self.host, self.repo
        ));
        self.runner.run("curl", &args).await
    }
}

#[async_trait]
impl CiProvider for ForgejoProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Forgejo
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            dag: DagSource::None,
            logs: LogMode::None,
        }
    }

    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>> {
        let body = self.get(&format!("actions/tasks?limit={limit}")).await?;
        let resp: TasksResponse = serde_json::from_str(&body).map_err(|e| CiError::Parse {
            what: "forgejo tasks".into(),
            message: e.to_string(),
        })?;
        Ok(resp
            .workflow_runs
            .into_iter()
            .map(WorkflowRun::into_run)
            .collect())
    }

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail> {
        let found = self
            .list_runs(50)
            .await?
            .into_iter()
            .find(|r| &r.id == run)
            .ok_or_else(|| CiError::NotFound(format!("run {}", run.0)))?;
        let job = CiJob {
            id: JobId(found.id.0.clone()),
            name: found.name.clone(),
            status: found.status,
            needs: Vec::new(),
        };
        Ok(RunDetail {
            run: found,
            jobs: vec![job],
        })
    }

    async fn job_log(&self, _run: &RunId, _job: &JobId, _offset: u64) -> Result<LogChunk> {
        Err(CiError::Unsupported("forgejo job logs"))
    }

    async fn run_extras(&self, _run: &RunId) -> Result<RunExtras> {
        Ok(RunExtras::default())
    }

    async fn current_pr(&self) -> Result<Option<PullRequest>> {
        Ok(None)
    }
}

#[derive(Deserialize)]
struct TasksResponse {
    #[serde(default)]
    workflow_runs: Vec<WorkflowRun>,
}

/// One run from `/actions/tasks` (GitHub `workflow_run`-shaped). Every field is
/// optional so a forge that omits one degrades to a blank, not a parse failure.
#[derive(Deserialize)]
struct WorkflowRun {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    display_title: String,
    #[serde(default)]
    head_branch: String,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default, alias = "html_url")]
    url: String,
    #[serde(default)]
    created_at: Option<String>,
}

impl WorkflowRun {
    fn into_run(self) -> CiRun {
        CiRun {
            id: RunId(self.id.to_string()),
            name: self.name,
            title: self.display_title,
            branch: self.head_branch,
            commit: self.head_sha,
            author: String::new(),
            created: self.created_at.as_deref().and_then(|ts| {
                time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok()
            }),
            status: map_status(&self.status, self.conclusion.as_deref()),
            url: (!self.url.is_empty()).then_some(self.url),
            remote: None,
        }
    }
}

fn map_status(status: &str, conclusion: Option<&str>) -> JobStatus {
    match conclusion {
        Some("success") => JobStatus::Ok,
        Some("failure" | "timed_out" | "startup_failure") => JobStatus::Failed,
        Some("skipped") => JobStatus::Skipped,
        Some("cancelled") => JobStatus::Neutral,
        _ => match status {
            "running" | "in_progress" => JobStatus::Running,
            "success" => JobStatus::Ok,
            "failure" => JobStatus::Failed,
            _ => JobStatus::Queued,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;

    #[tokio::test]
    async fn list_runs_parses_the_tasks_envelope() {
        let json = r#"{"total_count":1,"workflow_runs":[
            {"id":7,"name":"CI","display_title":"fix things","head_branch":"main",
             "head_sha":"abc1234","status":"completed","conclusion":"success",
             "html_url":"https://codeberg.org/mattf/diffler/actions/runs/7",
             "created_at":"2026-06-26T10:00:00Z"}]}"#;
        let runs = ForgejoProvider::new(
            Box::new(RecordingRunner::new(&[("actions/tasks", json)])),
            "codeberg.org".into(),
            "mattf/diffler".into(),
            None,
        )
        .list_runs(10)
        .await
        .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("7".into()));
        assert_eq!(runs[0].branch, "main");
        assert_eq!(runs[0].commit, "abc1234");
        assert_eq!(runs[0].status, JobStatus::Ok);
        assert!(runs[0].created.is_some());
    }
}
