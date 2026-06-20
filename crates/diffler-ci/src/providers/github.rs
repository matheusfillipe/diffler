//! GitHub Actions adapter (CLI-only via `gh`). The dependency DAG comes from a
//! run's workflow YAML `jobs.<id>.needs` (the run API omits it); status overlays
//! from `gh run view`. Logs come from `gh run view --log` (step-delimited);
//! artifacts and annotations from the REST API via `gh api`.

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{CiError, Result};
use crate::exec::CommandRunner;
use crate::model::{
    Annotation, AnnotationLevel, Artifact, Capabilities, CiJob, CiRun, DagSource, JobId, JobStatus,
    LogChunk, LogMode, PullRequest, RunDetail, RunExtras, RunId,
};
use crate::provider::{CiProvider, ProviderKind};

/// Talks to GitHub Actions through `gh`. The runs list is scoped to the current
/// `branch` (across all of its workflows); each run's DAG comes from whichever
/// of the repo's `workflows` YAMLs matches that run's workflow name.
pub struct GitHubProvider {
    runner: Box<dyn CommandRunner>,
    /// Every `.github/workflows/*.yml` body, so a run's DAG is built from its own
    /// workflow (matched by the YAML `name:`), not a single guessed file.
    workflows: Vec<String>,
    /// The checked-out branch, scoping the runs list; `None` on detached HEAD.
    branch: Option<String>,
}

impl GitHubProvider {
    pub fn new(
        runner: Box<dyn CommandRunner>,
        workflows: Vec<String>,
        branch: Option<String>,
    ) -> Self {
        Self {
            runner,
            workflows,
            branch,
        }
    }

    /// `gh api <path>`; `{owner}`/`{repo}` in `path` resolve to the current repo.
    async fn api(&self, path: &str) -> Result<String> {
        self.runner
            .run("gh", &["api".to_owned(), path.to_owned()])
            .await
    }

    async fn artifacts(&self, run: &RunId) -> Result<Vec<Artifact>> {
        let raw = self
            .api(&format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{}/artifacts",
                run.0
            ))
            .await?;
        let list: ArtifactList = serde_json::from_str(&raw).map_err(|e| CiError::Parse {
            what: "gh api artifacts".into(),
            message: e.to_string(),
        })?;
        Ok(list.artifacts.into_iter().map(ArtifactItem::into).collect())
    }

    async fn annotations(&self, run: &RunId) -> Result<Vec<Annotation>> {
        let raw = self
            .api(&format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{}/jobs",
                run.0
            ))
            .await?;
        let jobs: JobsApi = serde_json::from_str(&raw).map_err(|e| CiError::Parse {
            what: "gh api jobs".into(),
            message: e.to_string(),
        })?;
        let mut annotations = Vec::new();
        for job in jobs.jobs {
            // one job's annotations 404ing (a GC'd check run) or rate-limiting
            // must not drop every other job's — skip it and keep going
            let Ok(raw) = self
                .api(&format!("{}/annotations", job.check_run_url))
                .await
            else {
                continue;
            };
            let Ok(items) = serde_json::from_str::<Vec<AnnotationItem>>(&raw) else {
                continue;
            };
            annotations.extend(items.into_iter().map(AnnotationItem::into));
        }
        Ok(annotations)
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
        let mut args = vec!["run".to_owned(), "list".to_owned()];
        if let Some(branch) = &self.branch {
            args.push("--branch".to_owned());
            args.push(branch.clone());
        }
        args.extend(
            [
                "-L",
                &limit.to_string(),
                "--json",
                "databaseId,displayTitle,headBranch,headSha,status,conclusion,workflowName,createdAt,url",
            ]
            .map(str::to_owned),
        );
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
            "jobs,displayTitle,headBranch,headSha,status,conclusion,workflowName,createdAt,url",
        ]
        .map(str::to_owned);
        let out = self.runner.run("gh", &args).await?;
        let view: RunView = serde_json::from_str(&out).map_err(|e| CiError::Parse {
            what: "gh run view".into(),
            message: e.to_string(),
        })?;

        // build the DAG from the run's own workflow, matched by the YAML `name:`
        // against the run's `workflowName`; an unmatched run falls back to flat
        let specs = self
            .workflows
            .iter()
            .find(|yaml| workflow_name(yaml).as_deref() == Some(view.workflow_name.as_str()))
            .and_then(|yaml| parse_workflow(yaml).ok())
            .unwrap_or_default();
        let jobs = if specs.is_empty() {
            // no workflow file: a flat, edgeless node per run job
            view.jobs
                .iter()
                .map(|j| CiJob {
                    id: JobId(j.name.clone()),
                    name: j.name.clone(),
                    status: map_status(&j.status, j.conclusion.as_deref()),
                    needs: Vec::new(),
                })
                .collect()
        } else {
            specs
                .iter()
                .map(|spec| CiJob {
                    id: JobId(spec.id.clone()),
                    name: spec.label.clone(),
                    status: aggregate_status(&spec.id, &spec.label, &view.jobs),
                    needs: spec.needs.iter().cloned().map(JobId).collect(),
                })
                .collect()
        };
        Ok(RunDetail {
            run: view.into_run(run.clone()),
            jobs,
        })
    }

    async fn job_log(&self, run: &RunId, job: &JobId, _offset: u64) -> Result<LogChunk> {
        // resolve the run-job database id for this job (matrix jobs expand into
        // several legs; the first matching leg is shown)
        let view_args = ["run", "view", &run.0, "--json", "jobs"].map(str::to_owned);
        let out = self.runner.run("gh", &view_args).await?;
        let view: JobList = serde_json::from_str(&out).map_err(|e| CiError::Parse {
            what: "gh run view".into(),
            message: e.to_string(),
        })?;
        let db_id = view
            .jobs
            .iter()
            .find(|j| j.name == job.0 || job_matches(&j.name, &job.0, &job.0))
            .map(|j| j.database_id)
            .ok_or_else(|| CiError::NotFound(format!("job {} in run {}", job.0, run.0)))?;

        // `--log` prefixes each line with `<job>\t<step>\t<timestamp>`, so the
        // host can group the log into collapsible steps
        let log_args =
            ["run", "view", &run.0, "--log", "--job", &db_id.to_string()].map(str::to_owned);
        let text = self.runner.run("gh", &log_args).await?;
        let next_offset = text.len() as u64;
        Ok(LogChunk {
            text,
            next_offset,
            done: true,
        })
    }

    async fn run_extras(&self, run: &RunId) -> Result<RunExtras> {
        // the extras panel is auxiliary: a forge hiccup degrades a section to
        // empty rather than failing the graph page (and, since the host re-polls
        // extras only while they're absent, rather than re-fetching forever)
        Ok(RunExtras {
            artifacts: self.artifacts(run).await.unwrap_or_default(),
            annotations: self.annotations(run).await.unwrap_or_default(),
        })
    }

    async fn current_pr(&self) -> Result<Option<PullRequest>> {
        let Some(branch) = &self.branch else {
            return Ok(None);
        };
        // `gh pr view` exits non-zero when the branch has no PR; that's a normal
        // state, not an error, so a failed call resolves to "no PR"
        let args = ["pr", "view", branch, "--json", "number,title,url"].map(str::to_owned);
        let Ok(raw) = self.runner.run("gh", &args).await else {
            return Ok(None);
        };
        let Ok(pr) = serde_json::from_str::<PrView>(&raw) else {
            return Ok(None);
        };
        Ok(Some(PullRequest {
            number: pr.number,
            title: pr.title,
            url: (!pr.url.is_empty()).then_some(pr.url),
        }))
    }
}

/// A workflow job's structure from the YAML.
struct JobSpec {
    id: String,
    label: String,
    needs: Vec<String>,
}

/// The workflow's display `name:` (what `gh run list` reports as `workflowName`),
/// used to match a run to the YAML that defines its DAG.
fn workflow_name(yaml: &str) -> Option<String> {
    let value: serde_norway::Value = serde_norway::from_str(yaml).ok()?;
    value
        .get("name")
        .and_then(serde_norway::Value::as_str)
        .map(str::to_owned)
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
            self.display_title.clone()
        } else {
            self.workflow_name
        };
        CiRun {
            id: RunId(self.database_id.to_string()),
            name,
            title: self.display_title,
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

impl RunView {
    fn into_run(self, id: RunId) -> CiRun {
        CiRun {
            id,
            name: self.workflow_name,
            title: self.display_title,
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

#[derive(Deserialize)]
struct ArtifactList {
    artifacts: Vec<ArtifactItem>,
}

#[derive(Deserialize)]
struct ArtifactItem {
    name: String,
    #[serde(rename = "size_in_bytes")]
    size_in_bytes: u64,
    expired: bool,
}

impl From<ArtifactItem> for Artifact {
    fn from(item: ArtifactItem) -> Self {
        Artifact {
            name: item.name,
            size_bytes: item.size_in_bytes,
            expired: item.expired,
        }
    }
}

/// The REST jobs response (`actions/runs/{id}/jobs`), which — unlike
/// `gh run view --json jobs` — carries each job's `check_run_url`, the handle
/// the annotations endpoint hangs off.
#[derive(Deserialize)]
struct JobsApi {
    jobs: Vec<JobApi>,
}

#[derive(Deserialize)]
struct PrView {
    number: u64,
    title: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct JobApi {
    check_run_url: String,
}

#[derive(Deserialize)]
struct AnnotationItem {
    annotation_level: Option<String>,
    title: Option<String>,
    message: Option<String>,
    path: Option<String>,
    start_line: Option<u64>,
}

impl From<AnnotationItem> for Annotation {
    fn from(item: AnnotationItem) -> Self {
        let level = match item.annotation_level.as_deref() {
            Some("failure") => AnnotationLevel::Failure,
            Some("warning") => AnnotationLevel::Warning,
            _ => AnnotationLevel::Notice,
        };
        Annotation {
            level,
            title: item.title.unwrap_or_default(),
            message: item.message.unwrap_or_default(),
            path: item.path.unwrap_or_default(),
            start_line: item.start_line,
        }
    }
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

    const RELEASE_WORKFLOW: &str = r"
name: Release
on: push
jobs:
  publish:
    runs-on: ubuntu-latest
";

    fn provider(responses: &[(&'static str, &str)]) -> GitHubProvider {
        GitHubProvider::new(
            Box::new(RecordingRunner::new(responses)),
            vec![WORKFLOW.to_owned()],
            None,
        )
    }

    #[tokio::test]
    async fn list_runs_scopes_to_the_branch() {
        // the response only matches if `--branch feat/x` was sent
        let json = r#"[{"databaseId":1,"displayTitle":"x","headBranch":"feat/x","headSha":"a",
            "status":"completed","conclusion":"success","workflowName":"CI",
            "createdAt":"2026-06-18T10:00:00Z","url":"u"}]"#;
        let runs = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[("list --branch feat/x", json)])),
            vec![WORKFLOW.to_owned()],
            Some("feat/x".to_owned()),
        )
        .list_runs(10)
        .await
        .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].branch, "feat/x");
    }

    #[tokio::test]
    async fn run_detail_builds_the_dag_from_the_runs_own_workflow() {
        // the run is a `Release` run; its DAG must come from the Release YAML
        // (one `publish` job), not the CI YAML (lint/test/publish)
        let view = r#"{
          "displayTitle":"cut","headBranch":"main","headSha":"abc","status":"completed",
          "conclusion":"success","workflowName":"Release",
          "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/9",
          "jobs":[{"databaseId":1,"name":"publish","status":"completed","conclusion":"success"}]
        }"#;
        let detail = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[("run view", view)])),
            vec![WORKFLOW.to_owned(), RELEASE_WORKFLOW.to_owned()],
            None,
        )
        .run_detail(&RunId("9".into()))
        .await
        .expect("detail");
        let ids: Vec<&str> = detail.jobs.iter().map(|j| j.id.0.as_str()).collect();
        assert_eq!(ids, ["publish"], "matched the Release workflow, not CI");
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
          "displayTitle":"fix things","headBranch":"main","headSha":"abc","status":"in_progress",
          "conclusion":null,"workflowName":"CI",
          "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/42",
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
    async fn run_extras_collects_artifacts_and_annotations() {
        let artifacts = r#"{"artifacts":[
            {"name":"coverage","size_in_bytes":2048,"expired":false},
            {"name":"old-logs","size_in_bytes":10,"expired":true}
        ]}"#;
        let jobs = r#"{"jobs":[
            {"check_run_url":"https://api.github.com/repos/o/r/check-runs/99"}
        ]}"#;
        let annotations = r#"[
            {"annotation_level":"warning","title":"clippy","message":"unused import",
             "path":"src/lib.rs","start_line":12},
            {"annotation_level":"failure","title":"test","message":"assert failed",
             "path":"src/x.rs","start_line":null}
        ]"#;
        let extras = provider(&[
            ("artifacts", artifacts),
            ("/jobs", jobs),
            ("annotations", annotations),
        ])
        .run_extras(&RunId("42".into()))
        .await
        .expect("extras");
        assert_eq!(extras.artifacts.len(), 2);
        assert_eq!(extras.artifacts[0].name, "coverage");
        assert!(extras.artifacts[1].expired);
        assert_eq!(extras.annotations.len(), 2);
        assert_eq!(extras.annotations[0].level, AnnotationLevel::Warning);
        assert_eq!(extras.annotations[0].start_line, Some(12));
        assert_eq!(extras.annotations[1].level, AnnotationLevel::Failure);
    }

    #[tokio::test]
    async fn run_extras_degrades_to_artifacts_when_annotations_fail() {
        // the jobs list is fetchable but its one job's annotations call has no
        // recorded response (the mock errors) — artifacts must survive
        let artifacts =
            r#"{"artifacts":[{"name":"coverage","size_in_bytes":2048,"expired":false}]}"#;
        let jobs =
            r#"{"jobs":[{"check_run_url":"https://api.github.com/repos/o/r/check-runs/99"}]}"#;
        let extras = provider(&[("artifacts", artifacts), ("/jobs", jobs)])
            .run_extras(&RunId("42".into()))
            .await
            .expect("extras never errors");
        assert_eq!(extras.artifacts.len(), 1);
        assert!(extras.annotations.is_empty(), "failed job is skipped");
    }

    #[tokio::test]
    async fn current_pr_parses_the_branch_pr() {
        let json = r#"{"number":28,"title":"Inline CI runs","url":"https://gh/pull/28"}"#;
        let pr = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[("pr view feat/x", json)])),
            vec![],
            Some("feat/x".to_owned()),
        )
        .current_pr()
        .await
        .expect("pr call");
        let pr = pr.expect("a pr");
        assert_eq!(pr.number, 28);
        assert_eq!(pr.url.as_deref(), Some("https://gh/pull/28"));
    }

    #[tokio::test]
    async fn current_pr_is_none_without_a_branch() {
        let pr = provider(&[]).current_pr().await.expect("pr call");
        assert!(pr.is_none(), "no branch → no PR, no gh call");
    }

    #[tokio::test]
    async fn capabilities_are_config_dag_and_dump_logs() {
        let caps = provider(&[]).capabilities();
        assert_eq!(caps.dag, DagSource::ConfigFile);
        assert_eq!(caps.logs, LogMode::Dump);
    }
}
