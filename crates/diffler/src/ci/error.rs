use serde::de::DeserializeOwned;
use thiserror::Error;

/// The host shows these as a status-bar message and degrades the screen.
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

/// `what` names the payload in the status-bar message.
pub(super) fn parse_json<T: DeserializeOwned>(what: &str, raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|e| CiError::Parse {
        what: what.to_owned(),
        message: e.to_string(),
    })
}
