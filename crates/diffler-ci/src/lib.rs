//! Provider-agnostic CI run/job/log acquisition for diffler.
//!
//! Adapters (`providers/`) implement [`CiProvider`] over each forge — CLI-only
//! today (`gh`, `glab`) via the [`CommandRunner`] seam — and normalize results
//! into [`model`] types. The host maps [`RunDetail`] onto a `diffler_graph::Model`
//! and drives polling; this crate does no rendering and holds no credentials.

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
    LogChunk, LogMode, LogStepMeta, PullRequest, RunDetail, RunExtras, RunId, ts_sort_key,
};
pub use provider::{CiProvider, ProviderKind};
pub use providers::{GitHubProvider, GitLabProvider};
