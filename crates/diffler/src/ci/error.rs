use serde::de::DeserializeOwned;
use thiserror::Error;

/// Failures from acquiring CI data. Adapters surface these; the host maps them
/// to a status-bar message and degrades (no runs / no DAG / no logs) rather than
/// crashing.
#[derive(Debug, Error)]
pub enum CiError {
    #[error("the `{0}` CLI is not installed or not on PATH")]
    CliMissing(&'static str),
    #[error("`{cmd}` failed: {message}")]
    Exec { cmd: String, message: String },
    #[error("parsing {what}: {message}")]
    Parse { what: String, message: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unsupported by this provider: {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, CiError>;

/// Deserialize a forge response, wrapping a failure as [`CiError::Parse`] with
/// `what` describing the payload for the status-bar message. Every adapter
/// parses forge JSON through this instead of repeating the `map_err`.
pub fn parse_json<T: DeserializeOwned>(what: &str, raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|e| CiError::Parse {
        what: what.to_owned(),
        message: e.to_string(),
    })
}
