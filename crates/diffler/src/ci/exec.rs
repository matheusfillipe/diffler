//! The subprocess seam: adapters shell out through `CommandRunner` so tests can
//! inject recorded output.

use async_trait::async_trait;
use tokio::process::Command;

use crate::ci::error::{CiError, Result};

#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// `program` is static so a missing binary can be named precisely.
    async fn run(&self, program: &'static str, args: &[String]) -> Result<String>;

    /// `gh api` exits 1 on a `304 Not Modified`, the successful answer to a
    /// conditional request, so its stdout still counts.
    async fn run_ignoring_status(&self, program: &'static str, args: &[String]) -> Result<String> {
        self.run(program, args).await
    }
}

pub struct RealRunner;

#[async_trait]
impl CommandRunner for RealRunner {
    async fn run(&self, program: &'static str, args: &[String]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .envs(crate::proc::NO_PROMPT)
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

    async fn run_ignoring_status(&self, program: &'static str, args: &[String]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .envs(crate::proc::NO_PROMPT)
            .output()
            .await
            .map_err(|_| CiError::CliMissing(program))?;
        String::from_utf8(output.stdout).map_err(|err| CiError::Parse {
            what: format!("{program} output"),
            message: err.to_string(),
        })
    }
}

/// `curl --fail-with-body` explains the exit on stderr and carries the forge's
/// rejection reason on stdout, so we keep both.
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

    use super::{CiError, CommandRunner, Result};

    /// Answers with the first key found as a substring of the joined command,
    /// tried in insertion order so the most specific key goes first.
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
                .map_or_else(String::new, |(_, value)| value.clone());
            // a response written as `ERR:<reason>` fails the call
            match hit.strip_prefix("ERR:") {
                Some(reason) => Err(CiError::Exec {
                    cmd: joined,
                    message: reason.to_owned(),
                }),
                None => Ok(hit),
            }
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
