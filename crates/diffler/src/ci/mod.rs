//! CI and PR review across forges. Adapters implement [`ForgeProvider`] over
//! `gh`/`glab`/`curl` through the [`CommandRunner`] seam and never touch the terminal.

mod detect;
mod error;
mod exec;
mod model;
mod provider;
mod providers;

pub use detect::{Detected, detect};
pub use error::{CiError, Result};
#[cfg(test)]
pub(crate) use exec::test_support;
pub use exec::{CommandRunner, RealRunner};
pub use model::{
    Annotation, AnnotationLevel, Artifact, Capabilities, CiJob, CiJobLeg, CiRun, DagSource, JobId,
    JobStatus, LogChunk, LogMode, LogStepMeta, PrComment, PullRequest, RunDetail, RunExtras, RunId,
    fmt_duration, ts_sort_key,
};
pub use provider::{
    ForgeProvider, NewPrComment, NewPrReview, NewPullRequest, ProviderKind, ReviewVerdict,
    capabilities_for,
};
pub use providers::{EtagCache, ForgejoProvider, GitHubProvider, GitLabProvider, YamlCache};

use std::path::Path;

use crate::config::CiConfig;
use crate::graph::{Edge, Model, Node, NodeId, NodeStatus, RankDir};

/// A configured `host` overrides remote detection for a self-hosted instance.
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

/// Without the forge CLI we disable CI, so no poll errors.
pub fn provider_available(detected: &Detected) -> bool {
    let cli = match detected.kind {
        ProviderKind::GitHub => "gh",
        ProviderKind::GitLab => "glab",
        ProviderKind::Forgejo => "curl",
    };
    std::env::var_os("PATH").is_some_and(|path| on_path(cli, &path))
}

pub(crate) fn on_path(program: &str, path: &std::ffi::OsStr) -> bool {
    std::env::split_paths(path)
        .any(|dir| dir.join(program).is_file() || dir.join(format!("{program}.exe")).is_file())
}

/// `git@host:owner/repo.git`, `https://host/owner/repo`, or `ssh://git@host:port/owner/repo`.
fn parse_host(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.split(':').next().map(str::to_owned);
    }
    let authority = url.split("://").nth(1)?.split('/').next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_owned())
}

/// A Forgejo detection with no resolvable host passes through as `None`, so
/// the provider errors on every call and the token never reaches a guessed host.
pub fn build_provider(
    detected: &Detected,
    repo_root: &Path,
    branch: Option<&str>,
    remote_url: Option<&str>,
    yaml_cache: YamlCache,
    etags: EtagCache,
) -> Box<dyn ForgeProvider + Send> {
    match detected.kind {
        ProviderKind::GitHub => Box::new(GitHubProvider::new(
            Box::new(RealRunner),
            read_workflows(repo_root),
            branch.map(str::to_owned),
            yaml_cache,
            etags,
            remote_url.and_then(parse_owner_repo),
        )),
        ProviderKind::GitLab => Box::new(GitLabProvider::new(
            Box::new(RealRunner),
            detected.host.clone(),
            branch.map(str::to_owned),
        )),
        ProviderKind::Forgejo => Box::new(ForgejoProvider::new(
            Box::new(RealRunner),
            detected.host.clone(),
            remote_url.and_then(parse_owner_repo).unwrap_or_default(),
            forgejo_token(),
            branch.map(str::to_owned),
        )),
    }
}

/// GitHub and Forgejo Actions share this `conclusion` vocabulary. `None`
/// means no conclusion yet, and the caller reads its forge's own status strings.
pub(crate) fn map_conclusion(conclusion: Option<&str>) -> Option<JobStatus> {
    match conclusion? {
        "success" => Some(JobStatus::Ok),
        "failure" | "timed_out" | "startup_failure" => Some(JobStatus::Failed),
        "skipped" => Some(JobStatus::Skipped),
        "cancelled" => Some(JobStatus::Neutral),
        _ => None,
    }
}

/// `owner/name` from a remote URL; extra path segments are dropped.
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

/// A job with matrix legs becomes a foldable root with one member node per
/// leg; edges always target the root, since `needs` resolves per job.
pub fn to_model(detail: &RunDetail) -> Model {
    let mut model = Model::new(RankDir::LeftRight);
    for job in &detail.jobs {
        model.nodes.push(Node {
            id: NodeId::new(job.id.0.clone()),
            label: job_label(&job.name, job.duration_secs),
            status: node_status(job.status),
            group: None,
            foldable: (!job.legs.is_empty()).then(|| job.id.0.clone()),
            subgraph: None,
            decision: false,
        });
        for leg in &job.legs {
            model.nodes.push(Node {
                id: NodeId::new(leg.id.0.clone()),
                label: job_label(&leg.name, leg.duration_secs),
                status: node_status(leg.status),
                group: Some(job.id.0.clone()),
                foldable: None,
                subgraph: None,
                decision: false,
            });
        }
    }
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

fn job_label(name: &str, duration_secs: Option<i64>) -> String {
    match duration_secs {
        Some(secs) => format!("{name}  {}", fmt_duration(secs)),
        None => name.to_owned(),
    }
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
    use super::{CiJob, CiJobLeg, CiRun, JobId, RunId};

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

    #[tokio::test]
    async fn forced_forgejo_with_no_resolvable_host_fails_closed() {
        // no remote, no config host: build_provider must not fall back to a
        // hardcoded default host (that would risk sending the token elsewhere)
        let dir = tempfile::tempdir().expect("tempdir");
        let config = CiConfig {
            provider: "forgejo".to_owned(),
            ..CiConfig::default()
        };
        let detected = detect_for_repo(dir.path(), None, &config).expect("detected");
        assert_eq!(detected.host, None);

        let provider = build_provider(
            &detected,
            dir.path(),
            None,
            None,
            YamlCache::default(),
            EtagCache::default(),
        );
        let err = provider.list_runs(1).await.expect_err("no host to target");
        assert!(matches!(err, CiError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn map_conclusion_covers_the_shared_actions_vocabulary() {
        assert_eq!(map_conclusion(Some("success")), Some(JobStatus::Ok));
        assert_eq!(map_conclusion(Some("failure")), Some(JobStatus::Failed));
        assert_eq!(map_conclusion(Some("timed_out")), Some(JobStatus::Failed));
        assert_eq!(map_conclusion(Some("skipped")), Some(JobStatus::Skipped));
        assert_eq!(map_conclusion(Some("cancelled")), Some(JobStatus::Neutral));
        assert_eq!(
            map_conclusion(None),
            None,
            "no conclusion yet: not classified"
        );
        assert_eq!(
            map_conclusion(Some("neutral")),
            None,
            "an unrecognized value falls back to the forge's own status strings"
        );
    }

    #[test]
    fn parse_owner_repo_handles_common_url_shapes() {
        assert_eq!(
            parse_owner_repo("git@codeberg.org:acme/widgets.git").as_deref(),
            Some("acme/widgets")
        );
        assert_eq!(
            parse_owner_repo("https://codeberg.org/acme/widgets").as_deref(),
            Some("acme/widgets")
        );
        assert_eq!(
            parse_owner_repo("ssh://git@codeberg.org:2222/acme/widgets.git").as_deref(),
            Some("acme/widgets")
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
                    duration_secs: None,
                    needs: vec![],
                    legs: vec![],
                },
                CiJob {
                    id: JobId("test".into()),
                    name: "test".into(),
                    status: JobStatus::Running,
                    duration_secs: None,
                    needs: vec![JobId("lint".into())],
                    legs: vec![],
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

    #[test]
    fn a_node_wears_the_time_its_job_spent() {
        let detail = RunDetail {
            run: run(),
            jobs: vec![
                CiJob {
                    id: JobId("lint".into()),
                    name: "lint".into(),
                    status: JobStatus::Ok,
                    duration_secs: Some(73),
                    needs: vec![],
                    legs: vec![],
                },
                CiJob {
                    id: JobId("queued".into()),
                    name: "queued".into(),
                    status: JobStatus::Queued,
                    duration_secs: None,
                    needs: vec![],
                    legs: vec![],
                },
            ],
        };
        let model = to_model(&detail);
        assert_eq!(model.nodes[0].label, "lint  1m13s");
        assert_eq!(
            model.nodes[1].label, "queued",
            "a job that has not started says nothing about time"
        );
    }

    #[test]
    fn a_matrix_job_becomes_a_foldable_root_with_one_member_per_leg() {
        let detail = RunDetail {
            run: run(),
            jobs: vec![
                CiJob {
                    id: JobId("build".into()),
                    name: "build".into(),
                    status: JobStatus::Failed,
                    duration_secs: Some(90),
                    needs: vec![],
                    legs: vec![
                        CiJobLeg {
                            id: JobId("build (Dockerfile.cuda, -cuda)".into()),
                            name: "Dockerfile.cuda, -cuda".into(),
                            status: JobStatus::Ok,
                            duration_secs: Some(60),
                        },
                        CiJobLeg {
                            id: JobId("build (Dockerfile.gpu)".into()),
                            name: "Dockerfile.gpu".into(),
                            status: JobStatus::Failed,
                            duration_secs: Some(90),
                        },
                    ],
                },
                CiJob {
                    id: JobId("publish".into()),
                    name: "publish".into(),
                    status: JobStatus::Queued,
                    duration_secs: None,
                    needs: vec![JobId("build".into())],
                    legs: vec![],
                },
            ],
        };
        let model = to_model(&detail);

        let ids: Vec<&str> = model.nodes.iter().map(|n| n.id.0.as_str()).collect();
        assert_eq!(
            ids,
            [
                "build",
                "build (Dockerfile.cuda, -cuda)",
                "build (Dockerfile.gpu)",
                "publish"
            ],
            "a leg is keyed by the run job it ran as, so its log is found"
        );

        let root = &model.nodes[0];
        assert_eq!(
            root.foldable.as_deref(),
            Some("build"),
            "the job is its group's root"
        );
        assert_eq!(root.group, None, "a root is not itself a member");

        let leg0 = &model.nodes[1];
        assert_eq!(leg0.group.as_deref(), Some("build"));
        assert_eq!(leg0.foldable, None);
        assert_eq!(
            leg0.label, "Dockerfile.cuda, -cuda  1m00s",
            "a leg reads as its own parameters, not the job name repeated"
        );

        let leg1 = &model.nodes[2];
        assert_eq!(leg1.label, "Dockerfile.gpu  1m30s");
        assert_eq!(leg1.status, NodeStatus::Failed);

        let publish = &model.nodes[3];
        assert_eq!(publish.group, None, "a single-leg job stays a plain node");
        assert_eq!(publish.foldable, None);

        let edges: Vec<(&str, &str)> = model
            .edges
            .iter()
            .map(|e| (e.from.0.as_str(), e.to.0.as_str()))
            .collect();
        assert_eq!(
            edges,
            [("build", "publish")],
            "the needs edge lands on the root, never a leg"
        );

        let collapsed = model.collapse(&std::collections::HashSet::from(["build".to_owned()]));
        assert_eq!(
            collapsed.nodes.len(),
            2,
            "the legs fold away, the root and publish remain"
        );
        let root = collapsed
            .nodes
            .iter()
            .find(|n| n.id.0 == "build")
            .expect("root stays");
        assert_eq!(
            root.status,
            NodeStatus::Failed,
            "a failing leg still reads through the fold"
        );
        assert!(
            root.label.contains('▸'),
            "the collapsed root marks its fold state: {}",
            root.label
        );
    }
}
