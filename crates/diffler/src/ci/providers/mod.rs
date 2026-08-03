//! Per-forge adapters implementing [`crate::ci::ForgeProvider`].

mod forgejo;
mod github;
mod gitlab;

pub use forgejo::ForgejoProvider;
pub use github::{EtagCache, GitHubProvider, YamlCache};
pub use gitlab::GitLabProvider;
