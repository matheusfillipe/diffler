//! Forgejo/Codeberg adapter. Forgejo exposes a GitHub-shaped Actions REST API,
//! fetched with `curl` through the same `CommandRunner` seam the other adapters
//! use — a public repo needs no token; a PAT is read from the environment.
//! Job logs and the dependency DAG aren't wired yet; `Capabilities` says so.

use async_trait::async_trait;
use serde::Deserialize;

use crate::ci::error::{CiError, Result, parse_json};
use crate::ci::exec::CommandRunner;
use crate::ci::model::{
    CiJob, CiRun, JobId, JobStatus, LogChunk, PullRequest, RunDetail, RunExtras, RunId,
};
use crate::ci::provider::{ForgeProvider, ProviderKind};

pub struct ForgejoProvider {
    runner: Box<dyn CommandRunner>,
    /// `None` when no host could be resolved (no configured `[ci.forgejo]
    /// host` and no parseable remote); every call then fails closed instead
    /// of guessing a host to send the token to.
    host: Option<String>,
    /// `owner/name`.
    repo: String,
    token: Option<String>,
    branch: Option<String>,
}

impl ForgejoProvider {
    pub fn new(
        runner: Box<dyn CommandRunner>,
        host: Option<String>,
        repo: String,
        token: Option<String>,
        branch: Option<String>,
    ) -> Self {
        Self {
            runner,
            host,
            repo,
            token,
            branch,
        }
    }

    async fn get(&self, path: &str) -> Result<String> {
        let host = self
            .host
            .as_deref()
            .ok_or_else(|| CiError::NotFound("no Forgejo host configured".to_owned()))?;
        let mut args = vec![
            "-sS".to_owned(),
            "--fail".to_owned(),
            "--max-time".to_owned(),
            "20".to_owned(),
            "-H".to_owned(),
            "Accept: application/json".to_owned(),
        ];
        if let Some(token) = &self.token {
            args.push("-H".to_owned());
            args.push(format!("Authorization: token {token}"));
        }
        args.push(format!("https://{host}/api/v1/repos/{}/{path}", self.repo));
        // a failed exec embeds the argv in the error, which the status bar
        // renders — never let the token through
        self.runner.run("curl", &args).await.map_err(|err| {
            let Some(token) = &self.token else { return err };
            match err {
                CiError::Exec { cmd, message } => CiError::Exec {
                    cmd: cmd.replace(token.as_str(), "***"),
                    message: message.replace(token.as_str(), "***"),
                },
                other => other,
            }
        })
    }
}

#[async_trait]
impl ForgeProvider for ForgejoProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Forgejo
    }

    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>> {
        let body = self.get(&format!("actions/runs?limit={limit}")).await?;
        let resp: RunsResponse = parse_json("forgejo runs", &body)?;
        Ok(resp
            .workflow_runs
            .into_iter()
            .map(RunItem::into_run)
            .filter(|run| {
                self.branch
                    .as_deref()
                    .is_none_or(|branch| run.branch == branch)
            })
            .collect())
    }

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail> {
        let found = self
            .list_runs(50)
            .await?
            .into_iter()
            .find(|r| &r.id == run)
            .ok_or_else(|| CiError::NotFound(format!("run {}", run.0)))?;
        // no run-jobs endpoint on current Forgejo: this run's jobs are the
        // tasks sharing its run number
        let body = self.get("actions/tasks?limit=50").await?;
        let tasks: TasksResponse = parse_json("forgejo tasks", &body)?;
        let jobs: Vec<CiJob> = tasks
            .workflow_runs
            .iter()
            .filter(|t| t.run_number.map(|n| n.to_string()).as_deref() == Some(run.0.as_str()))
            .map(|t| CiJob {
                id: JobId(t.id.to_string()),
                name: t.name.clone(),
                status: map_status(&t.status, t.conclusion.as_deref()),
                needs: Vec::new(),
            })
            .collect();
        Ok(RunDetail { run: found, jobs })
    }

    async fn job_log(&self, _run: &RunId, _job: &JobId, _offset: u64) -> Result<LogChunk> {
        Err(CiError::Unsupported("forgejo job logs"))
    }

    async fn run_extras(&self, _run: &RunId) -> Result<RunExtras> {
        Ok(RunExtras::default())
    }

    async fn list_prs(&self) -> Result<Vec<PullRequest>> {
        let raw = self.get("pulls?state=open&limit=50").await?;
        let pulls: Vec<PullItem> = parse_json("pr list", &raw)?;
        Ok(pulls.into_iter().map(PullItem::into_pr).collect())
    }

    async fn current_pr(&self) -> Result<Option<PullRequest>> {
        let Some(branch) = &self.branch else {
            return Ok(None);
        };
        let raw = self.get("pulls?state=open&limit=50").await?;
        // a malformed response must propagate, same as `list_prs` — treating
        // it as "no PR" would look like a normal, PR-less branch
        let pulls: Vec<PullItem> = parse_json("pr list", &raw)?;
        Ok(pulls
            .into_iter()
            .find(|p| p.head.r#ref == *branch)
            .map(PullItem::into_pr))
    }
}

#[derive(Deserialize)]
struct PullItem {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    html_url: String,
    head: PullSide,
    base: PullSide,
    #[serde(default)]
    user: ForgejoUser,
}

#[derive(Deserialize, Default)]
struct ForgejoUser {
    #[serde(default)]
    login: String,
}

impl PullItem {
    fn into_pr(self) -> PullRequest {
        PullRequest {
            number: self.number,
            title: self.title,
            url: (!self.html_url.is_empty()).then_some(self.html_url),
            base_ref: self.base.r#ref,
            head_ref: self.head.r#ref,
            head_oid: self.head.sha,
            author: self.user.login,
        }
    }
}

#[derive(Deserialize)]
struct PullSide {
    #[serde(default)]
    r#ref: String,
    #[serde(default)]
    sha: String,
}

#[derive(Deserialize)]
struct RunsResponse {
    #[serde(default)]
    workflow_runs: Vec<RunItem>,
}

/// One run from `/actions/runs`. `index_in_repo` is the human run number the
/// tasks reference and the web URL uses; it becomes the `RunId`.
#[derive(Deserialize)]
struct RunItem {
    index_in_repo: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    workflow_id: String,
    #[serde(default)]
    prettyref: String,
    #[serde(default)]
    commit_sha: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    created: Option<String>,
}

impl RunItem {
    fn into_run(self) -> CiRun {
        CiRun {
            id: RunId(self.index_in_repo.to_string()),
            name: self.workflow_id,
            title: self.title,
            branch: self.prettyref,
            commit: self.commit_sha,
            author: String::new(),
            created: self.created.as_deref().and_then(|ts| {
                time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok()
            }),
            status: map_status(&self.status, None),
            url: (!self.html_url.is_empty()).then_some(self.html_url),
            remote: None,
        }
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
    run_number: Option<u64>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
}

fn map_status(status: &str, conclusion: Option<&str>) -> JobStatus {
    // Forgejo's Actions API mirrors GitHub's `conclusion` vocabulary
    // (`crate::ci::map_conclusion` covers both); only the in-progress/no-conclusion
    // status strings are forge-specific
    crate::ci::map_conclusion(conclusion).unwrap_or(match status {
        "running" | "in_progress" => JobStatus::Running,
        "success" => JobStatus::Ok,
        "failure" => JobStatus::Failed,
        _ => JobStatus::Queued,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;

    #[tokio::test]
    async fn list_runs_parses_the_tasks_envelope() {
        let json = r#"{"workflow_runs":[
            {"id":900,"index_in_repo":7,"workflow_id":"ci.yml","title":"fix things",
             "prettyref":"main","commit_sha":"abc1234","status":"success",
             "html_url":"https://codeberg.org/acme/widgets/actions/runs/7",
             "created":"2026-06-26T10:00:00Z"}]}"#;
        let runs = ForgejoProvider::new(
            Box::new(RecordingRunner::new(&[("actions/runs", json)])),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            None,
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

    #[tokio::test]
    async fn a_failed_call_never_leaks_the_token() {
        struct FailingRunner;
        #[async_trait::async_trait]
        impl crate::ci::exec::CommandRunner for FailingRunner {
            async fn run(&self, program: &'static str, args: &[String]) -> Result<String> {
                Err(CiError::Exec {
                    cmd: format!("{program} {}", args.join(" ")),
                    message: "curl: (22) The requested URL returned error: 401".into(),
                })
            }
        }
        let err = ForgejoProvider::new(
            Box::new(FailingRunner),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            Some("sekret-token".into()),
            None,
        )
        .list_runs(10)
        .await
        .expect_err("fails");
        let text = err.to_string();
        assert!(!text.contains("sekret-token"), "token redacted: {text}");
        assert!(text.contains("***"));
    }

    #[tokio::test]
    async fn list_runs_scopes_to_the_branch() {
        let json = r#"{"workflow_runs":[
            {"id":900,"index_in_repo":7,"workflow_id":"ci.yml","prettyref":"main","status":"success"},
            {"id":901,"index_in_repo":8,"workflow_id":"ci.yml","prettyref":"feat/x","status":"success"}]}"#;
        let runs = ForgejoProvider::new(
            Box::new(RecordingRunner::new(&[("actions/runs", json)])),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            None,
            Some("feat/x".into()),
        )
        .list_runs(10)
        .await
        .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("8".into()));
    }

    #[tokio::test]
    async fn no_host_fails_closed_without_ever_calling_curl() {
        let runner = std::sync::Arc::new(RecordingRunner::new(&[(
            "actions/runs",
            r#"{"workflow_runs":[]}"#,
        )]));
        let err = ForgejoProvider::new(
            Box::new(runner.clone()),
            None,
            "acme/widgets".into(),
            Some("sekret-token".into()),
            None,
        )
        .list_runs(10)
        .await
        .expect_err("no host to target");
        assert!(matches!(err, CiError::NotFound(_)));
        assert!(
            runner.calls().is_empty(),
            "an unresolved host must never reach curl, e.g. a hardcoded default"
        );
    }

    #[tokio::test]
    async fn current_pr_propagates_a_parse_failure_like_list_prs() {
        let err = ForgejoProvider::new(
            Box::new(RecordingRunner::new(&[("pulls", "not json")])),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            None,
            Some("feat/x".into()),
        )
        .current_pr()
        .await
        .expect_err("malformed body must not silently read as \"no PR\"");
        assert!(matches!(err, CiError::Parse { .. }));
    }
}
