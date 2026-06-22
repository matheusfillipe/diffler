//! Per-forge adapters implementing [`crate::ci::CiProvider`].

mod github;
mod gitlab;

pub use github::GitHubProvider;
pub use gitlab::GitLabProvider;
