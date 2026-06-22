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
