//! Per-forge adapters implementing [`crate::CiProvider`].

mod github;
mod gitlab;

pub use github::GitHubProvider;
pub use gitlab::GitLabProvider;
