//! Picks the CI provider for a repo from the remote host, then from which CI
//! config files exist. A configured provider wins over both.

use std::path::Path;

use crate::ci::provider::ProviderKind;

/// `host` is set only for a self-hosted forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub kind: ProviderKind,
    pub host: Option<String>,
}

/// `forced` is the configured provider kind.
pub fn detect(
    repo_root: &Path,
    remote_host: Option<&str>,
    forced: Option<ProviderKind>,
) -> Option<Detected> {
    let has_gh = repo_root.join(".github/workflows").is_dir();
    let has_gitlab = repo_root.join(".gitlab-ci.yml").is_file();
    classify(remote_host, has_gh, has_gitlab, forced)
}

fn classify(
    remote_host: Option<&str>,
    has_gh_workflows: bool,
    has_gitlab_ci: bool,
    forced: Option<ProviderKind>,
) -> Option<Detected> {
    let self_hosted = |host: Option<&str>| {
        host.filter(|h| !h.eq_ignore_ascii_case("gitlab.com"))
            .map(str::to_owned)
    };

    // a forced kind takes its host from `[ci.<provider>] host`
    if let Some(kind) = forced {
        return Some(Detected { kind, host: None });
    }

    if let Some(host) = remote_host {
        if host.eq_ignore_ascii_case("github.com") {
            return Some(Detected {
                kind: ProviderKind::GitHub,
                host: None,
            });
        }
        if host.eq_ignore_ascii_case("gitlab.com") || host.to_ascii_lowercase().contains("gitlab") {
            return Some(Detected {
                kind: ProviderKind::GitLab,
                host: self_hosted(Some(host)),
            });
        }
        if host.eq_ignore_ascii_case("codeberg.org")
            || host.to_ascii_lowercase().contains("forgejo")
        {
            return Some(Detected {
                kind: ProviderKind::Forgejo,
                host: Some(host.to_owned()),
            });
        }
    }

    // unknown host: fall back to config-file presence
    if has_gh_workflows {
        return Some(Detected {
            kind: ProviderKind::GitHub,
            host: None,
        });
    }
    if has_gitlab_ci {
        return Some(Detected {
            kind: ProviderKind::GitLab,
            host: self_hosted(remote_host),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_dot_com_is_github() {
        assert_eq!(
            classify(Some("github.com"), false, false, None),
            Some(Detected {
                kind: ProviderKind::GitHub,
                host: None
            })
        );
    }

    #[test]
    fn codeberg_is_forgejo_and_carries_its_host() {
        assert_eq!(
            classify(Some("codeberg.org"), false, false, None),
            Some(Detected {
                kind: ProviderKind::Forgejo,
                host: Some("codeberg.org".to_owned())
            })
        );
    }

    #[test]
    fn gitlab_dot_com_carries_no_host() {
        assert_eq!(
            classify(Some("gitlab.com"), false, false, None),
            Some(Detected {
                kind: ProviderKind::GitLab,
                host: None
            })
        );
    }

    #[test]
    fn self_hosted_gitlab_host_with_config_file() {
        assert_eq!(
            classify(Some("git.example.com"), false, true, None),
            Some(Detected {
                kind: ProviderKind::GitLab,
                host: Some("git.example.com".into())
            })
        );
    }

    #[test]
    fn unknown_host_falls_back_to_workflows_dir() {
        assert_eq!(
            classify(Some("git.example.com"), true, false, None),
            Some(Detected {
                kind: ProviderKind::GitHub,
                host: None
            })
        );
    }

    #[test]
    fn no_signals_is_none() {
        assert_eq!(classify(None, false, false, None), None);
    }

    #[test]
    fn forced_overrides_detection() {
        assert_eq!(
            classify(Some("github.com"), true, false, Some(ProviderKind::GitLab)),
            Some(Detected {
                kind: ProviderKind::GitLab,
                host: None
            })
        );
    }
}
