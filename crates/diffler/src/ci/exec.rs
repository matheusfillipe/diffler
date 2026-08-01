//! The subprocess seam. Adapters shell to `gh`/`glab`/`curl` through
//! `CommandRunner` so tests can inject recorded output instead of running a live
//! CLI — this is what makes the adapters fully unit-testable. The runner is
//! async (tokio process) so adapter futures never block the executor.

use async_trait::async_trait;
use tokio::process::Command;

use crate::ci::error::{CiError, Result};

/// Runs a CLI and returns its stdout.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// `program` is a static name (e.g. `"gh"`) so a missing binary can be
    /// reported precisely; `args` is the full argument vector.
    async fn run(&self, program: &'static str, args: &[String]) -> Result<String>;
}

/// Spawns the real binary on `PATH`.
pub struct RealRunner;

#[async_trait]
impl CommandRunner for RealRunner {
    async fn run(&self, program: &'static str, args: &[String]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .await
            .map_err(|_| CiError::CliMissing(program))?;
        if !output.status.success() {
            return Err(CiError::Exec {
                cmd: format!("{program} {}", args.join(" ")),
                message: failure_message(&output.stdout, &output.stderr),
            });
        }
        String::from_utf8(output.stdout).map_err(|err| CiError::Parse {
            what: format!("{program} output"),
            message: err.to_string(),
        })
    }
}

/// Both streams of a failed run, joined. `curl --fail-with-body` explains the
/// exit code on stderr and carries the forge's rejection reason on stdout, so
/// dropping either loses why a write was refused.
fn failure_message(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
pub(crate) mod test_support {
    use async_trait::async_trait;

    use super::{CommandRunner, Result};

    /// A `CommandRunner` that returns canned stdout for the first registered key
    /// that appears as a substring of the joined command (e.g. `"run list"`,
    /// `"run view"`, `"--log"`, `"api graphql"`). Keys are tried in insertion
    /// order so the most specific can win.
    pub struct RecordingRunner {
        responses: Vec<(&'static str, String)>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingRunner {
        pub fn new(responses: &[(&'static str, &str)]) -> Self {
            Self {
                responses: responses
                    .iter()
                    .map(|(k, v)| (*k, (*v).to_owned()))
                    .collect(),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// Joined argv of every call, for asserting how often an endpoint
        /// was actually hit.
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().map(|c| c.clone()).unwrap_or_default()
        }
    }

    #[async_trait]
    impl CommandRunner for std::sync::Arc<RecordingRunner> {
        async fn run(&self, program: &'static str, args: &[String]) -> Result<String> {
            self.as_ref().run(program, args).await
        }
    }

    #[async_trait]
    impl CommandRunner for RecordingRunner {
        async fn run(&self, _program: &'static str, args: &[String]) -> Result<String> {
            let joined = args.join(" ");
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(joined.clone());
            }
            let hit = self
                .responses
                .iter()
                .find(|(key, _)| joined.contains(key))
                .map(|(_, value)| value.clone());
            Ok(hit.unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_keeps_the_response_body_beside_the_exit_reason() {
        let message = failure_message(
            br#"{"message":"approve your own pull is not allowed"}"#,
            b"curl: (22) The requested URL returned error: 422",
        );
        assert_eq!(
            message,
            r#"curl: (22) The requested URL returned error: 422: {"message":"approve your own pull is not allowed"}"#
        );
        assert_eq!(failure_message(b"", b"boom"), "boom");
        assert_eq!(failure_message(b"boom", b""), "boom");
    }

    #[tokio::test]
    async fn real_runner_reports_a_missing_binary() {
        let err = RealRunner
            .run("definitely-not-a-real-binary-xyzzy", &[])
            .await
            .unwrap_err();
        assert!(matches!(err, CiError::CliMissing(_)));
    }
}
