//! The subprocess seam. Adapters shell to provider CLIs (`gh`, `glab`) through
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
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        String::from_utf8(output.stdout).map_err(|err| CiError::Parse {
            what: format!("{program} output"),
            message: err.to_string(),
        })
    }
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
    }

    impl RecordingRunner {
        pub fn new(responses: &[(&'static str, &str)]) -> Self {
            Self {
                responses: responses
                    .iter()
                    .map(|(k, v)| (*k, (*v).to_owned()))
                    .collect(),
            }
        }
    }

    #[async_trait]
    impl CommandRunner for RecordingRunner {
        async fn run(&self, _program: &'static str, args: &[String]) -> Result<String> {
            let joined = args.join(" ");
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

    #[tokio::test]
    async fn real_runner_reports_a_missing_binary() {
        let err = RealRunner
            .run("definitely-not-a-real-binary-xyzzy", &[])
            .await
            .unwrap_err();
        assert!(matches!(err, CiError::CliMissing(_)));
    }
}
