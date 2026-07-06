//! Provider-agnostic CI run/job/log acquisition, plus the host glue that picks a
//! provider for the repo and maps a normalized `RunDetail` onto the graph model.
//! Adapters (`providers/`) implement [`ForgeProvider`] over each forge (via
//! `gh`/`glab`/`curl` through the [`CommandRunner`] seam) and never touch the terminal.

mod detect;
mod error;
mod exec;
mod model;
mod provider;
mod providers;

pub use detect::{Detected, detect};
pub use error::{CiError, Result};
pub use exec::{CommandRunner, RealRunner};
pub use model::{
    Annotation, AnnotationLevel, Artifact, Capabilities, CiJob, CiRun, DagSource, JobId, JobStatus,
    LogChunk, LogMode, LogStepMeta, PrComment, PullRequest, RunDetail, RunExtras, RunId,
    ts_sort_key,
};
pub use provider::{ForgeProvider, NewPrComment, ProviderKind};
pub use providers::{ForgejoProvider, GitHubProvider, GitLabProvider, YamlCache};

use std::path::Path;

use crate::config::CiConfig;
use crate::graph::{Edge, Model, Node, NodeId, NodeStatus, RankDir};

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
        "forgejo" | "codeberg" => Some(ProviderKind::Forgejo),
        _ => None,
    };
    let host = remote_url.and_then(parse_host);
    let mut detected = detect(repo_root, host.as_deref(), forced)?;
    if detected.kind == ProviderKind::GitLab && config.gitlab.host.is_some() {
        detected.host.clone_from(&config.gitlab.host);
    }
    if detected.kind == ProviderKind::Forgejo {
        if config.forgejo.host.is_some() {
            detected.host.clone_from(&config.forgejo.host);
        } else if detected.host.is_none() {
            detected.host = host;
        }
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
        ProviderKind::Forgejo => "curl",
    };
    std::env::var_os("PATH").is_some_and(|path| on_path(cli, &path))
}

/// Whether `program` resolves in one of `path`'s directories (with a `.exe`
/// fallback on Windows). Split from [`provider_available`] for testability.
pub(crate) fn on_path(program: &str, path: &std::ffi::OsStr) -> bool {
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
pub fn build_provider(
    detected: &Detected,
    repo_root: &Path,
    branch: Option<&str>,
    remote_url: Option<&str>,
    yaml_cache: YamlCache,
) -> Box<dyn ForgeProvider + Send> {
    match detected.kind {
        ProviderKind::GitHub => Box::new(GitHubProvider::new(
            Box::new(RealRunner),
            read_workflows(repo_root),
            branch.map(str::to_owned),
            yaml_cache,
        )),
        ProviderKind::GitLab => Box::new(GitLabProvider::new(
            Box::new(RealRunner),
            detected.host.clone(),
        )),
        ProviderKind::Forgejo => Box::new(ForgejoProvider::new(
            Box::new(RealRunner),
            detected
                .host
                .clone()
                .unwrap_or_else(|| "codeberg.org".to_owned()),
            remote_url.and_then(parse_owner_repo).unwrap_or_default(),
            forgejo_token(),
            branch.map(str::to_owned),
        )),
    }
}

/// `owner/name` from a git remote URL: `git@host:owner/name.git` or
/// `https://host/owner/name(.git)`. Extra path segments are dropped.
fn parse_owner_repo(url: &str) -> Option<String> {
    let path = if let Some(rest) = url.strip_prefix("git@") {
        rest.split_once(':').map(|(_, p)| p)?
    } else {
        url.split("://").nth(1)?.split_once('/').map(|(_, p)| p)?
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/');
    let owner = segments.next()?;
    let name = segments.next()?;
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

/// A Forgejo PAT for private repos; public repos need none.
fn forgejo_token() -> Option<String> {
    std::env::var("FORGEJO_TOKEN")
        .or_else(|_| std::env::var("CODEBERG_TOKEN"))
        .ok()
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
    use super::{CiJob, CiRun, JobId, RunId};

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
            remote: None,
        }
    }

    #[test]
    fn forced_forgejo_targets_the_remote_host_not_codeberg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = CiConfig {
            provider: "forgejo".to_owned(),
            ..CiConfig::default()
        };
        let detected =
            detect_for_repo(dir.path(), Some("git@git.example.com:me/repo.git"), &config)
                .expect("detected");
        assert_eq!(detected.kind, ProviderKind::Forgejo);
        assert_eq!(detected.host.as_deref(), Some("git.example.com"));

        config.forgejo.host = Some("forge.corp.io".to_owned());
        let detected =
            detect_for_repo(dir.path(), Some("git@git.example.com:me/repo.git"), &config)
                .expect("detected");
        assert_eq!(detected.host.as_deref(), Some("forge.corp.io"));
    }

    #[test]
    fn parse_owner_repo_handles_common_url_shapes() {
        assert_eq!(
            parse_owner_repo("git@codeberg.org:mattf/diffler.git").as_deref(),
            Some("mattf/diffler")
        );
        assert_eq!(
            parse_owner_repo("https://codeberg.org/mattf/diffler").as_deref(),
            Some("mattf/diffler")
        );
        assert_eq!(
            parse_owner_repo("ssh://git@codeberg.org:2222/mattf/diffler.git").as_deref(),
            Some("mattf/diffler")
        );
        assert_eq!(parse_owner_repo("https://codeberg.org/"), None);
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
