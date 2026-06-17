//! GitHub Actions source: builds a `diffler_graph::Model` from a workflow YAML's
//! `jobs.<id>.needs`, with live status from `gh run view`. The YAML parse +
//! status overlay are pure and unit-tested; only the `gh` calls do IO. This is
//! the host-side source — the graph component never does IO itself.

use std::path::Path;
use std::process::Command;

use color_eyre::eyre::{Context, Result, eyre};
use diffler_graph::{Edge, Model, Node, NodeId, NodeStatus, RankDir};
use serde::Deserialize;

/// One job's live state from `gh run view --json jobs`.
#[derive(Debug, Clone, Deserialize)]
pub struct JobStatus {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
}

/// Build the model from a workflow file and the run's job statuses. Pass an
/// empty slice for the statuses to render just the structure (all queued).
pub fn build_model(workflow_yaml: &str, jobs: &[JobStatus]) -> Result<Model> {
    let (job_ids, edges) = parse_workflow(workflow_yaml)?;
    let mut model = Model::new(RankDir::TopDown);
    model.nodes = job_ids
        .iter()
        .map(|(id, label)| Node {
            id: NodeId::new(id.clone()),
            label: label.clone(),
            status: status_for(label, id, jobs),
            group: None,
            foldable: None,
        })
        .collect();
    model.edges = edges
        .into_iter()
        .map(|(from, to)| Edge {
            from: NodeId::new(from),
            to: NodeId::new(to),
            label: None,
        })
        .collect();
    Ok(model)
}

/// A job's `(id, display label)`.
type JobDef = (String, String);
/// An edge's `(from id, to id)`.
type EdgeDef = (String, String);

/// Parse a workflow YAML into `(jobs, edges)`. Jobs keep their declaration
/// order; an edge runs from each `needs` entry to the job.
fn parse_workflow(yaml: &str) -> Result<(Vec<JobDef>, Vec<EdgeDef>)> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml).wrap_err("workflow YAML did not parse")?;
    let jobs = value
        .get("jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or_else(|| eyre!("workflow has no `jobs` mapping"))?;

    let mut job_ids = Vec::new();
    let mut edges = Vec::new();
    for (key, job) in jobs {
        let Some(id) = key.as_str() else { continue };
        let label = job
            .get("name")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or(id)
            .to_owned();
        job_ids.push((id.to_owned(), label));
        for need in needs_of(job) {
            edges.push((need, id.to_owned()));
        }
    }
    Ok((job_ids, edges))
}

/// `needs` is either a single job id or a list of them.
fn needs_of(job: &serde_yaml::Value) -> Vec<String> {
    match job.get("needs") {
        Some(serde_yaml::Value::String(one)) => vec![one.clone()],
        Some(serde_yaml::Value::Sequence(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Pick a node's status from the run jobs. A matrix job (`test`) expands into
/// several run jobs (`test (ubuntu-latest)` …); match by exact name/id or the
/// `name (` matrix prefix and take the worst status, so a single red leg shows.
fn status_for(label: &str, id: &str, jobs: &[JobStatus]) -> NodeStatus {
    jobs.iter()
        .filter(|j| {
            j.name == label
                || j.name == id
                || j.name.starts_with(&format!("{label} ("))
                || j.name.starts_with(&format!("{id} ("))
        })
        .map(|j| map_status(&j.status, j.conclusion.as_deref()))
        .reduce(worst)
        // no matching run job yet (or no run at all) reads as queued
        .unwrap_or(NodeStatus::Queued)
}

/// Severity order so a failing matrix leg dominates the aggregate.
fn worst(a: NodeStatus, b: NodeStatus) -> NodeStatus {
    let rank = |s: NodeStatus| match s {
        NodeStatus::Failed => 5,
        NodeStatus::Running => 4,
        NodeStatus::Queued => 3,
        NodeStatus::Skipped => 2,
        NodeStatus::Neutral => 1,
        NodeStatus::Ok => 0,
    };
    if rank(a) >= rank(b) { a } else { b }
}

fn map_status(status: &str, conclusion: Option<&str>) -> NodeStatus {
    match conclusion {
        Some("success") => NodeStatus::Ok,
        Some("failure" | "timed_out" | "startup_failure") => NodeStatus::Failed,
        Some("skipped") => NodeStatus::Skipped,
        Some("cancelled") => NodeStatus::Neutral,
        _ => match status {
            "in_progress" => NodeStatus::Running,
            "completed" => NodeStatus::Neutral,
            _ => NodeStatus::Queued,
        },
    }
}

/// The most recent run id for `workflow`, via `gh run list`.
pub fn latest_run(workflow: &Path) -> Result<String> {
    let file = workflow
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre!("workflow path has no file name"))?;
    let out = run_gh(&[
        "run",
        "list",
        "--workflow",
        file,
        "--limit",
        "1",
        "--json",
        "databaseId",
        "-q",
        ".[0].databaseId",
    ])?;
    let id = out.trim().to_owned();
    if id.is_empty() {
        return Err(eyre!("no runs found for {file}"));
    }
    Ok(id)
}

/// The `gh run view --json jobs` envelope.
#[derive(Deserialize)]
struct RunJobs {
    jobs: Vec<JobStatus>,
}

/// Fetch the run's job statuses via `gh run view --json jobs`.
pub fn fetch_jobs(run_id: &str) -> Result<Vec<JobStatus>> {
    let out = run_gh(&["run", "view", run_id, "--json", "jobs"])?;
    let runs: RunJobs = serde_json::from_str(&out).wrap_err("gh run view JSON did not parse")?;
    Ok(runs.jobs)
}

fn run_gh(args: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .wrap_err("could not run `gh` (is the GitHub CLI installed?)")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("gh {}: {}", args.join(" "), stderr.trim()));
    }
    String::from_utf8(output.stdout).wrap_err("gh output was not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn parses_jobs_and_needs_edges_in_order() {
        let model = build_model(WORKFLOW, &[]).expect("model");
        let ids: Vec<&str> = model.nodes.iter().map(|n| n.id.0.as_str()).collect();
        assert_eq!(ids, ["lint", "test", "publish"]);
        // publish uses its `name:` as the label
        assert_eq!(model.nodes[2].label, "Publish");
        let edges: Vec<(&str, &str)> = model
            .edges
            .iter()
            .map(|e| (e.from.0.as_str(), e.to.0.as_str()))
            .collect();
        assert_eq!(
            edges,
            [("lint", "test"), ("lint", "publish"), ("test", "publish")]
        );
    }

    #[test]
    fn overlays_live_status_with_matrix_aggregation() {
        let jobs = vec![
            JobStatus {
                name: "lint".into(),
                status: "completed".into(),
                conclusion: Some("success".into()),
            },
            // matrix legs of `test`: one running, so the node reads running
            JobStatus {
                name: "test (ubuntu-latest)".into(),
                status: "completed".into(),
                conclusion: Some("success".into()),
            },
            JobStatus {
                name: "test (windows-latest)".into(),
                status: "in_progress".into(),
                conclusion: None,
            },
        ];
        let model = build_model(WORKFLOW, &jobs).expect("model");
        assert_eq!(model.nodes[0].status, NodeStatus::Ok, "lint succeeded");
        assert_eq!(
            model.nodes[1].status,
            NodeStatus::Running,
            "a test leg still runs"
        );
        assert_eq!(
            model.nodes[2].status,
            NodeStatus::Queued,
            "publish not started"
        );
    }
}
