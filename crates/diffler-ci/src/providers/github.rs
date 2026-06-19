//! GitHub Actions adapter (CLI-only via `gh`). The dependency DAG comes from the
//! workflow YAML's `jobs.<id>.needs` (GitHub's run API omits it); live status
//! overlays from `gh run view`. Logs are a whole-job dump (`gh run view --log`).

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{CiError, Result};
use crate::exec::CommandRunner;
use crate::model::{
    Capabilities, CiJob, CiRun, DagSource, JobId, JobStatus, LogChunk, LogMode, RunDetail, RunId,
};
use crate::provider::{CiProvider, ProviderKind};

/// Talks to GitHub Actions through `gh`, scoped to one workflow file (its YAML
/// supplies the DAG structure).
pub struct GitHubProvider {
    runner: Box<dyn CommandRunner>,
    workflow_yaml: String,
    workflow_file: String,
}

impl GitHubProvider {
    pub fn new(
        runner: Box<dyn CommandRunner>,
        workflow_yaml: String,
        workflow_file: String,
    ) -> Self {
        Self {
            runner,
            workflow_yaml,
            workflow_file,
        }
    }
}

#[async_trait]
impl CiProvider for GitHubProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitHub
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            dag: DagSource::ConfigFile,
            logs: LogMode::Dump,
        }
    }

    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>> {
        let args = [
            "run",
            "list",
            "--workflow",
            &self.workflow_file,
            "-L",
            &limit.to_string(),
            "--json",
            "databaseId,displayTitle,headBranch,headSha,status,conclusion,workflowName,createdAt,url",
        ]
        .map(str::to_owned);
        let out = self.runner.run("gh", &args).await?;
        let raw: Vec<RunListItem> = serde_json::from_str(&out).map_err(|e| CiError::Parse {
            what: "gh run list".into(),
            message: e.to_string(),
        })?;
        Ok(raw.into_iter().map(RunListItem::into_run).collect())
    }

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail> {
        let args = [
            "run",
            "view",
            &run.0,
            "--json",
            "jobs,headBranch,headSha,status,conclusion,workflowName,createdAt,url",
        ]
        .map(str::to_owned);
        let out = self.runner.run("gh", &args).await?;
        let view: RunView = serde_json::from_str(&out).map_err(|e| CiError::Parse {
            what: "gh run view".into(),
            message: e.to_string(),
        })?;

        let specs = parse_workflow(&self.workflow_yaml)?;
        let jobs = specs
            .iter()
            .map(|spec| CiJob {
                id: JobId(spec.id.clone()),
                name: spec.label.clone(),
                status: aggregate_status(&spec.id, &spec.label, &view.jobs),
                needs: spec.needs.iter().cloned().map(JobId).collect(),
            })
            .collect();
        Ok(RunDetail {
            run: view.into_run(run.clone()),
            jobs,
        })
    }

    async fn job_log(&self, run: &RunId, job: &JobId, _offset: u64) -> Result<LogChunk> {
        // resolve the run-job database id for this workflow job (matrix jobs
        // expand into several; the first matching leg is shown)
        let specs = parse_workflow(&self.workflow_yaml)?;
        let label = specs
            .iter()
            .find(|s| s.id == job.0)
            .map_or(job.0.clone(), |s| s.label.clone());

        let view_args = ["run", "view", &run.0, "--json", "jobs"].map(str::to_owned);
        let out = self.runner.run("gh", &view_args).await?;
        let view: JobList = serde_json::from_str(&out).map_err(|e| CiError::Parse {
            what: "gh run view".into(),
            message: e.to_string(),
        })?;
        let db_id = view
            .jobs
            .iter()
            .find(|j| job_matches(&j.name, &job.0, &label))
            .map(|j| j.database_id.to_string())
            .ok_or_else(|| CiError::NotFound(format!("job {} in run {}", job.0, run.0)))?;

        let log_args = ["run", "view", &run.0, "--log", "--job", &db_id].map(str::to_owned);
        let text = self.runner.run("gh", &log_args).await?;
        let next_offset = text.len() as u64;
        Ok(LogChunk {
            text,
            next_offset,
            done: true,
        })
    }
}

/// A workflow job's structure from the YAML.
struct JobSpec {
    id: String,
    label: String,
    needs: Vec<String>,
}

/// Parse `jobs.<id>` into specs, preserving declaration order; `needs` is a
/// scalar or a sequence of upstream job ids.
fn parse_workflow(yaml: &str) -> Result<Vec<JobSpec>> {
    let value: serde_norway::Value = serde_norway::from_str(yaml).map_err(|e| CiError::Parse {
        what: "workflow YAML".into(),
        message: e.to_string(),
    })?;
    let jobs = value
        .get("jobs")
        .and_then(serde_norway::Value::as_mapping)
        .ok_or_else(|| CiError::Parse {
            what: "workflow YAML".into(),
            message: "no `jobs` mapping".into(),
        })?;

    let mut specs = Vec::new();
    for (key, job) in jobs {
        let Some(id) = key.as_str() else { continue };
        let label = job
            .get("name")
            .and_then(serde_norway::Value::as_str)
            .unwrap_or(id)
            .to_owned();
        specs.push(JobSpec {
            id: id.to_owned(),
            label,
            needs: needs_of(job),
        });
    }
    Ok(specs)
}

fn needs_of(job: &serde_norway::Value) -> Vec<String> {
    match job.get("needs") {
        Some(serde_norway::Value::String(one)) => vec![one.clone()],
        Some(serde_norway::Value::Sequence(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// A matrix job (`test`) expands into several run jobs (`test (ubuntu-latest)`).
/// Match by exact name/id or the `name (` matrix prefix.
fn job_matches(run_job_name: &str, id: &str, label: &str) -> bool {
    run_job_name == label
        || run_job_name == id
        || run_job_name.starts_with(&format!("{label} ("))
        || run_job_name.starts_with(&format!("{id} ("))
}

/// Worst status across a job's matching run jobs, so one red leg shows red.
fn aggregate_status(id: &str, label: &str, jobs: &[RunJob]) -> JobStatus {
    jobs.iter()
        .filter(|j| job_matches(&j.name, id, label))
        .map(|j| map_status(&j.status, j.conclusion.as_deref()))
        .reduce(JobStatus::worse)
        .unwrap_or(JobStatus::Queued)
}

fn map_status(status: &str, conclusion: Option<&str>) -> JobStatus {
    match conclusion {
        Some("success") => JobStatus::Ok,
        Some("failure" | "timed_out" | "startup_failure") => JobStatus::Failed,
        Some("skipped") => JobStatus::Skipped,
        Some("cancelled") => JobStatus::Neutral,
        _ => match status {
            "in_progress" => JobStatus::Running,
            "completed" => JobStatus::Neutral,
            _ => JobStatus::Queued,
        },
    }
}

fn parse_created(raw: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

#[derive(Deserialize)]
struct RunListItem {
    #[serde(rename = "databaseId")]
    database_id: u64,
    #[serde(rename = "displayTitle")]
    display_title: String,
    #[serde(rename = "headBranch")]
    head_branch: String,
    #[serde(rename = "headSha")]
    head_sha: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "workflowName")]
    workflow_name: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    url: String,
}

impl RunListItem {
    fn into_run(self) -> CiRun {
        let name = if self.workflow_name.is_empty() {
            self.display_title
        } else {
            self.workflow_name
        };
        CiRun {
            id: RunId(self.database_id.to_string()),
            name,
            branch: self.head_branch,
            commit: self.head_sha,
            author: String::new(),
            created: parse_created(&self.created_at),
            status: map_status(&self.status, self.conclusion.as_deref()),
            url: Some(self.url),
        }
    }
}

/// `gh run view --json jobs` returns only the jobs array, so logs parse this
/// narrow shape rather than the full [`RunView`] (which needs the meta fields).
#[derive(Deserialize)]
struct JobList {
    jobs: Vec<RunJob>,
}

#[derive(Deserialize)]
struct RunView {
    jobs: Vec<RunJob>,
    #[serde(rename = "headBranch")]
    head_branch: String,
    #[serde(rename = "headSha")]
    head_sha: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "workflowName")]
    workflow_name: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    url: String,
}

impl RunView {
    fn into_run(self, id: RunId) -> CiRun {
        CiRun {
            id,
            name: self.workflow_name,
            branch: self.head_branch,
            commit: self.head_sha,
            author: String::new(),
            created: parse_created(&self.created_at),
            status: map_status(&self.status, self.conclusion.as_deref()),
            url: Some(self.url),
        }
    }
}

#[derive(Deserialize)]
struct RunJob {
    #[serde(rename = "databaseId")]
    database_id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::RecordingRunner;

    const WORKFLOW: &str = r"
name: CI
on: push
jobs:
  lint:
    runs-on: ubuntu-latest
  test:
    needs: lint
    runs-on: ubuntu-latest
  publish:
    name: Publish
    needs: [lint, test]
    runs-on: ubuntu-latest
";

    fn provider(responses: &[(&'static str, &str)]) -> GitHubProvider {
        GitHubProvider::new(
            Box::new(RecordingRunner::new(responses)),
            WORKFLOW.to_owned(),
            "ci.yml".to_owned(),
        )
    }

    #[tokio::test]
    async fn list_runs_parses_gh_json() {
        let json = r#"[
          {"databaseId":42,"displayTitle":"fix things","headBranch":"main","headSha":"abc1234",
           "status":"completed","conclusion":"success","workflowName":"CI",
           "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/42"}
        ]"#;
        let runs = provider(&[("run list", json)])
            .list_runs(10)
            .await
            .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("42".into()));
        assert_eq!(runs[0].name, "CI");
        assert_eq!(runs[0].branch, "main");
        assert_eq!(runs[0].status, JobStatus::Ok);
        assert!(runs[0].created.is_some());
    }

    #[tokio::test]
    async fn run_detail_builds_dag_with_matrix_aggregation() {
        let view = r#"{
          "headBranch":"main","headSha":"abc","status":"in_progress","conclusion":null,
          "workflowName":"CI","createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/42",
          "jobs":[
            {"databaseId":1,"name":"lint","status":"completed","conclusion":"success"},
            {"databaseId":2,"name":"test (ubuntu-latest)","status":"completed","conclusion":"success"},
            {"databaseId":3,"name":"test (windows-latest)","status":"in_progress","conclusion":null}
          ]
        }"#;
        let detail = provider(&[("run view", view)])
            .run_detail(&RunId("42".into()))
            .await
            .expect("detail");
        let ids: Vec<&str> = detail.jobs.iter().map(|j| j.id.0.as_str()).collect();
        assert_eq!(ids, ["lint", "test", "publish"]);
        assert_eq!(detail.jobs[2].name, "Publish");
        assert_eq!(
            detail.jobs[2].needs,
            vec![JobId("lint".into()), JobId("test".into())]
        );
        assert_eq!(detail.jobs[0].status, JobStatus::Ok, "lint succeeded");
        assert_eq!(detail.jobs[1].status, JobStatus::Running, "a test leg runs");
        assert_eq!(
            detail.jobs[2].status,
            JobStatus::Queued,
            "publish not started"
        );
    }

    #[tokio::test]
    async fn job_log_resolves_the_run_job_and_dumps() {
        // `gh run view --json jobs` returns only the jobs array — parsing must
        // not require the run meta fields (headBranch, …)
        let view = r#"{"jobs":[{"databaseId":7,"name":"lint","status":"completed","conclusion":"success"}]}"#;
        let chunk = provider(&[("--log", "line one\nline two\n"), ("run view", view)])
            .job_log(&RunId("42".into()), &JobId("lint".into()), 0)
            .await
            .expect("log");
        assert!(chunk.text.contains("line one"));
        assert!(chunk.done);
        assert_eq!(chunk.next_offset, chunk.text.len() as u64);
    }

    #[tokio::test]
    async fn capabilities_are_config_dag_and_dump_logs() {
        let caps = provider(&[]).capabilities();
        assert_eq!(caps.dag, DagSource::ConfigFile);
        assert_eq!(caps.logs, LogMode::Dump);
    }
}
