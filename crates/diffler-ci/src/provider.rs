//! The provider seam. One async trait every forge adapter implements; the host
//! holds a `Box<dyn CiProvider + Send>` chosen at runtime by detection.

use async_trait::async_trait;

use crate::error::Result;
use crate::model::{Capabilities, CiRun, JobId, LogChunk, RunDetail, RunId};

/// Which forge an adapter talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    GitHub,
    GitLab,
}

#[async_trait]
pub trait CiProvider: Send {
    fn kind(&self) -> ProviderKind;

    /// What this provider can do, so the UI degrades honestly.
    fn capabilities(&self) -> Capabilities;

    /// The most recent runs, newest first, capped at `limit`.
    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>>;

    /// A run's jobs and dependency edges.
    async fn run_detail(&self, run: &RunId) -> Result<RunDetail>;

    /// A slice of a job's log starting at `offset`. For `LogMode::Dump`
    /// providers the whole log returns once the job completes.
    async fn job_log(&self, run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk>;
}
