//! Host glue for `diffler-ci`: pick a provider for the repo, and map a normalized
//! `RunDetail` onto the `diffler_graph::Model` the graph screen renders. The
//! provider does the IO; this module is the composition seam between acquisition
//! (`diffler-ci`) and rendering (`diffler-graph`).

use std::path::Path;

use diffler_ci::{
    CiProvider, Detected, GitHubProvider, GitLabProvider, JobStatus, ProviderKind, RealRunner,
    RunDetail, detect,
};
use diffler_graph::{Edge, Model, Node, NodeId, NodeStatus, RankDir};

use crate::config::CiConfig;

/// Detect the repo's CI provider: the configured `provider` (or auto), the
/// `origin` remote host (from `remote_url`), and config-file presence. A
/// configured GitLab `host` overrides remote detection for a self-hosted
/// instance.
pub fn detect_for_repo(
    repo_root: &Path,
    remote_url: Option<&str>,
    config: &CiConfig,
) -> Option<Detected> {
    let forced = match config.provider.as_str() {
        "github" => Some(ProviderKind::GitHub),
        "gitlab" => Some(ProviderKind::GitLab),
        _ => None,
    };
    let host = remote_url.and_then(parse_host);
    let mut detected = detect(repo_root, host.as_deref(), forced)?;
    if detected.kind == ProviderKind::GitLab && config.gitlab.host.is_some() {
        detected.host.clone_from(&config.gitlab.host);
    }
    Some(detected)
}

/// Whether the forge CLI a provider drives is installed, so detection can
/// disable CI (hide the section, stop polling) instead of erroring on every
/// poll when `gh`/`glab` isn't on the host.
pub fn provider_available(detected: &Detected) -> bool {
    let cli = match detected.kind {
        ProviderKind::GitHub => "gh",
        ProviderKind::GitLab => "glab",
    };
    std::env::var_os("PATH").is_some_and(|path| on_path(cli, &path))
}

/// Whether `program` resolves in one of `path`'s directories (with a `.exe`
/// fallback on Windows). Split from [`provider_available`] for testability.
fn on_path(program: &str, path: &std::ffi::OsStr) -> bool {
    std::env::split_paths(path)
        .any(|dir| dir.join(program).is_file() || dir.join(format!("{program}.exe")).is_file())
}

/// Pull the host out of a git remote URL: `git@host:owner/repo.git`,
/// `https://host/owner/repo`, or `ssh://git@host:port/owner/repo`.
fn parse_host(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.split(':').next().map(str::to_owned);
    }
    let authority = url.split("://").nth(1)?.split('/').next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_owned())
}

/// Construct the provider for a detected forge. GitLab targets the detected host;
/// GitHub scopes the runs list to `branch` and carries every workflow YAML so
/// each run's DAG is built from its own workflow.
pub fn provider(
    detected: &Detected,
    repo_root: &Path,
    branch: Option<&str>,
) -> Box<dyn CiProvider + Send> {
    match detected.kind {
        ProviderKind::GitHub => Box::new(GitHubProvider::new(
            Box::new(RealRunner),
            read_workflows(repo_root),
            branch.map(str::to_owned),
        )),
        ProviderKind::GitLab => Box::new(GitLabProvider::new(
            Box::new(RealRunner),
            detected.host.clone(),
        )),
    }
}

/// Every `.github/workflows/*.{yml,yaml}` body, for per-run DAG matching.
fn read_workflows(repo_root: &Path) -> Vec<String> {
    let dir = repo_root.join(".github/workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x == "yml" || x == "yaml")
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect()
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

    #[test]
    fn on_path_finds_files_in_listed_dirs_only() {
        let dir = std::ffi::OsString::from(env!("CARGO_MANIFEST_DIR"));
        assert!(on_path("Cargo.toml", &dir), "a file in the dir resolves");
        assert!(!on_path("definitely-not-a-binary-xyz", &dir));
    }

    fn run() -> CiRun {
        CiRun {
            id: RunId("1".into()),
            name: "CI".into(),
            title: String::new(),
            branch: "main".into(),
            commit: "abc".into(),
            author: String::new(),
            created: None,
            status: JobStatus::Running,
            url: None,
        }
    }

    #[test]
    fn parse_host_handles_scp_https_and_ssh_urls() {
        assert_eq!(
            parse_host("git@github.com:o/r.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            parse_host("https://gitlab.com/o/r").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(
            parse_host("ssh://git@git.example.com:2222/o/r.git").as_deref(),
            Some("git.example.com")
        );
        assert_eq!(parse_host("not a url"), None);
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
