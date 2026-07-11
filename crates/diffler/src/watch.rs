//! Filesystem watcher: debounced notify events on the repo become
//! `AppEvent::RepoChanged`. Noise sources (the session store, lockfiles,
//! git's object database) are filtered out so staging or saving a comment
//! does not echo back as a repo change.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

const DEBOUNCE: Duration = Duration::from_millis(200);

/// Keeps the watcher alive and exposes its health: when notify reports
/// errors the app falls back to periodic polling.
pub struct WatcherHandle {
    pub healthy: Arc<AtomicBool>,
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

/// Watch the repository and send `RepoChanged` for relevant debounced
/// events. The recursive root watch already covers an in-tree `.git`; the
/// explicit HEAD/refs/index watches on the resolved `git_dir` cover linked
/// worktrees, whose gitdir lives outside the workdir.
pub fn spawn_watcher(
    repo_root: &Path,
    git_dir: &Path,
    tx: UnboundedSender<AppEvent>,
) -> notify::Result<WatcherHandle> {
    let healthy = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&healthy);
    let root = repo_root.to_path_buf();
    let metadata_dir = git_dir.to_path_buf();
    let mut debouncer =
        new_debouncer(
            DEBOUNCE,
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) => {
                    let hit = events.iter().any(|event| {
                        !matches!(event.kind, EventKind::Access(_))
                            && event
                                .paths
                                .iter()
                                .any(|path| relevant(path, &root, &metadata_dir))
                    });
                    if hit {
                        let _ = tx.send(AppEvent::RepoChanged);
                    }
                }
                Err(_) => flag.store(false, Ordering::Relaxed),
            },
        )?;
    debouncer.watch(repo_root, RecursiveMode::Recursive)?;
    for extra in ["HEAD", "refs", "index"] {
        let path = git_dir.join(extra);
        if !path.exists() {
            continue;
        }
        let mode = if path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        // duplicates of the root watch at worst; the debounced batch still
        // collapses into a single RepoChanged
        let _ = debouncer.watch(&path, mode);
    }
    Ok(WatcherHandle {
        healthy,
        _debouncer: debouncer,
    })
}

/// Whether an event path should trigger a refresh. Filters the session
/// store (`.diffler/`), git's own transient lockfiles (`*.lock` under the
/// gitdir flickers on every git write), and the object database (every
/// commit floods it). Project lockfiles (Cargo.lock, uv.lock, etc.) are
/// kept because they represent real working-tree changes worth reviewing.
fn relevant(path: &Path, repo_root: &Path, git_dir: &Path) -> bool {
    // git metadata, wherever the gitdir lives — in-tree `.git` or a linked
    // worktree's external dir under the main repo
    if let Ok(rel) = path.strip_prefix(git_dir) {
        return !(is_lockfile(path) || rel.starts_with("objects"));
    }
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    if rel.starts_with(".git") && is_lockfile(path) {
        return false;
    }
    !(rel.starts_with(".diffler") || rel.starts_with(".git/objects"))
}

fn is_lockfile(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `relevant` for a plain repo layout: gitdir at `<root>/.git`.
    fn relevant_in(root: &str, rel: &str) -> bool {
        let root = Path::new(root);
        relevant(&root.join(rel), root, &root.join(".git"))
    }

    #[test]
    fn source_and_git_metadata_paths_are_relevant() {
        assert!(relevant_in("/repo", "src/lib.rs"));
        assert!(relevant_in("/repo", "README.md"));
        assert!(relevant_in("/repo", ".git/HEAD"));
        assert!(relevant_in("/repo", ".git/index"));
        assert!(relevant_in("/repo", ".git/refs/heads/main"));
    }

    #[test]
    fn session_store_paths_are_filtered() {
        assert!(!relevant_in("/repo", ".diffler/session.json"));
        assert!(!relevant_in("/repo", ".diffler/config.toml"));
    }

    #[test]
    fn git_objects_are_filtered() {
        assert!(!relevant_in("/repo", ".git/objects/ab/cdef0123"));
        assert!(!relevant_in("/repo", ".git/objects/pack/pack-1.idx"));
    }

    #[test]
    fn git_lockfiles_under_git_dir_are_filtered() {
        assert!(!relevant_in("/repo", ".git/index.lock"));
        assert!(!relevant_in("/repo", ".git/MERGE_HEAD.lock"));
    }

    #[test]
    fn project_lockfiles_are_relevant() {
        // Cargo.lock, uv.lock and friends are real working-tree changes
        assert!(relevant_in("/repo", "Cargo.lock"));
        assert!(relevant_in("/repo", "uv.lock"));
        assert!(relevant_in("/repo", "Cargo.lock.bak"));
    }

    #[test]
    fn a_path_outside_the_root_stays_relevant() {
        // strip_prefix failing must not panic nor misclassify
        assert!(relevant(
            Path::new("/elsewhere/src/a.rs"),
            Path::new("/repo"),
            Path::new("/repo/.git"),
        ));
    }

    #[test]
    fn external_gitdir_metadata_events_pass_through() {
        // a linked worktree's gitdir lives outside the workdir; HEAD/index
        // changes there must still refresh the review
        let root = Path::new("/checkouts/wt");
        let gitdir = Path::new("/main/.git/worktrees/wt");
        assert!(relevant(&gitdir.join("HEAD"), root, gitdir));
        assert!(relevant(&gitdir.join("index"), root, gitdir));
        assert!(relevant(&gitdir.join("refs/heads/topic"), root, gitdir));
    }

    #[test]
    fn external_gitdir_lockfiles_and_objects_are_filtered() {
        let root = Path::new("/checkouts/wt");
        let gitdir = Path::new("/main/.git/worktrees/wt");
        assert!(!relevant(&gitdir.join("index.lock"), root, gitdir));
        assert!(!relevant(&gitdir.join("HEAD.lock"), root, gitdir));
        assert!(!relevant(&gitdir.join("objects/ab/cdef"), root, gitdir));
    }

    #[test]
    fn spawn_watcher_reports_healthy_on_a_real_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = spawn_watcher(dir.path(), &dir.path().join(".git"), tx).expect("watcher");
        assert!(handle.healthy.load(Ordering::Relaxed));
    }

    // End-to-end against the real notify backend: a relevant write must
    // surface as RepoChanged, an irrelevant one (under .diffler) must not.
    // macOS FSEvents reports canonicalized paths, so the watched root is
    // canonicalized too — otherwise `relevant`'s prefix-stripping silently
    // falls back to treating every event as relevant (see lsp_client.rs for
    // the same canonicalization need against a real OS-level watcher).
    #[tokio::test]
    async fn spawn_watcher_emits_repo_changed_for_relevant_writes_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        let git_dir = root.join(".git");
        std::fs::create_dir_all(&git_dir).expect("gitdir");
        std::fs::create_dir_all(root.join(".diffler")).expect("diffler dir");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _handle = spawn_watcher(&root, &git_dir, tx).expect("watcher");

        // an irrelevant write must not produce an event within a window well
        // past the debounce interval
        std::fs::write(root.join(".diffler/session.json"), "{}").expect("write");
        let irrelevant = tokio::time::timeout(Duration::from_millis(800), rx.recv()).await;
        assert!(
            irrelevant.is_err(),
            "a write under .diffler must not surface as RepoChanged, got {irrelevant:?}"
        );

        // a relevant write must arrive, retried across a generous timeout so
        // OS-level notify latency doesn't make this flaky on slower CI runners
        std::fs::write(root.join("src.rs"), "fn main() {}").expect("write");
        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("RepoChanged within timeout")
            .expect("channel stays open");
        assert!(matches!(event, AppEvent::RepoChanged));
    }
}
