//! Per-forge adapters implementing [`crate::ci::CiProvider`].

mod forgejo;
mod github;
mod gitlab;

pub use forgejo::ForgejoProvider;
pub use github::{GitHubProvider, YamlCache};
pub use gitlab::GitLabProvider;
