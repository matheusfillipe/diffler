//! Per-forge adapters implementing [`crate::CiProvider`].

mod github;

pub use github::GitHubProvider;
