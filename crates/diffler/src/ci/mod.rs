//! Host glue for `diffler-ci`: pick a provider for the repo, and map a normalized
//! `RunDetail` onto the `diffler_graph::Model` the graph screen renders. The
//! provider does the IO; this module is the composition seam between acquisition
//! (`diffler-ci`) and rendering (`diffler-graph`).

use std::path::Path;

use diffler_ci::{
    CiProvider, Detected, GitHubProvider, GitLabProvider, JobStatus, ProviderKind, RealRunner,
    RunDetail,
};
use diffler_graph::{Edge, Model, Node, NodeId, NodeStatus, RankDir};

/// Construct the provider for a detected forge. GitHub is scoped to the repo's
/// discovered workflow (its YAML supplies the DAG); GitLab targets the detected
/// host. `None` when the prerequisites are missing (e.g. no workflow file).
pub fn provider(detected: &Detected, repo_root: &Path) -> Option<Box<dyn CiProvider + Send>> {
    match detected.kind {
        ProviderKind::GitHub => {
            let workflow = crate::graph::discover_workflow(repo_root)?;
            let yaml = std::fs::read_to_string(&workflow).ok()?;
            let file = workflow.file_name()?.to_str()?.to_owned();
            Some(Box::new(GitHubProvider::new(
                Box::new(RealRunner),
                yaml,
                file,
            )))
        }
        ProviderKind::GitLab => Some(Box::new(GitLabProvider::new(
            Box::new(RealRunner),
            detected.host.clone(),
        ))),
    }
}

/// Map a run's jobs + dependency edges onto a graph model (top-down layered).
pub fn to_model(detail: &RunDetail) -> Model {
    let mut model = Model::new(RankDir::TopDown);
    model.nodes = detail
        .jobs
        .iter()
        .map(|job| Node {
            id: NodeId::new(job.id.0.clone()),
            label: job.name.clone(),
            status: node_status(job.status),
            group: None,
            foldable: None,
        })
        .collect();
    model.edges = detail
        .jobs
        .iter()
        .flat_map(|job| {
            let to = job.id.0.clone();
            job.needs.iter().map(move |dep| Edge {
                from: NodeId::new(dep.0.clone()),
                to: NodeId::new(to.clone()),
                label: None,
            })
        })
        .collect();
    model
}

fn node_status(status: JobStatus) -> NodeStatus {
    match status {
        JobStatus::Ok => NodeStatus::Ok,
        JobStatus::Failed => NodeStatus::Failed,
        JobStatus::Running => NodeStatus::Running,
        JobStatus::Queued => NodeStatus::Queued,
        JobStatus::Skipped => NodeStatus::Skipped,
        JobStatus::Neutral => NodeStatus::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffler_ci::{CiJob, CiRun, JobId, RunId};

    fn run() -> CiRun {
        CiRun {
            id: RunId("1".into()),
            name: "CI".into(),
            branch: "main".into(),
            commit: "abc".into(),
            author: String::new(),
            created: None,
            status: JobStatus::Running,
            url: None,
        }
    }

    #[test]
    fn maps_jobs_and_needs_to_nodes_and_edges() {
        let detail = RunDetail {
            run: run(),
            jobs: vec![
                CiJob {
                    id: JobId("lint".into()),
                    name: "lint".into(),
                    status: JobStatus::Ok,
                    needs: vec![],
                },
                CiJob {
                    id: JobId("test".into()),
                    name: "test".into(),
                    status: JobStatus::Running,
                    needs: vec![JobId("lint".into())],
                },
            ],
        };
        let model = to_model(&detail);
        let ids: Vec<&str> = model.nodes.iter().map(|n| n.id.0.as_str()).collect();
        assert_eq!(ids, ["lint", "test"]);
        assert_eq!(model.nodes[0].status, NodeStatus::Ok);
        let edges: Vec<(&str, &str)> = model
            .edges
            .iter()
            .map(|e| (e.from.0.as_str(), e.to.0.as_str()))
            .collect();
        assert_eq!(edges, [("lint", "test")]);
    }
}
