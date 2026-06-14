//! Session persistence: one file per review source under `.diffler/reviews/`,
//! atomically written, self-gitignored. The legacy single-session file
//! `.diffler/session.json` is read once and migrated to `reviews/working.json`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::session::Session;
use crate::source::ReviewSource;

const DIR: &str = ".diffler";
const REVIEWS: &str = "reviews";
const LEGACY_FILE: &str = "session.json";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("corrupt session file {0}: {1}")]
    Corrupt(PathBuf, serde_json::Error),
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OnDisk {
    version: u32,
    /// Self-describes the file for `load_all`; lookups go by filename (the
    /// source key), so the filename is authoritative. Absent in legacy files,
    /// where the source is implicitly the working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<ReviewSource>,
    #[serde(flatten)]
    session: Session,
}

fn reviews_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(DIR).join(REVIEWS)
}

fn source_path(repo_root: &Path, source: &ReviewSource) -> PathBuf {
    reviews_dir(repo_root).join(format!("{}.json", source.key()))
}

fn legacy_path(repo_root: &Path) -> PathBuf {
    repo_root.join(DIR).join(LEGACY_FILE)
}

fn read_session(path: &Path) -> Result<Option<Session>, StoreError> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let on_disk: OnDisk = serde_json::from_str(&raw)
                .map_err(|e| StoreError::Corrupt(path.to_path_buf(), e))?;
            Ok(Some(on_disk.session))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Load the session for one source. The working tree falls back to the legacy
/// single-session file when no per-source file exists yet; the file is moved on
/// the next [`save_source`].
pub fn load_source(repo_root: &Path, source: &ReviewSource) -> Result<Session, StoreError> {
    if let Some(session) = read_session(&source_path(repo_root, source))? {
        return Ok(session);
    }
    if matches!(source, ReviewSource::WorkingTree)
        && let Some(session) = read_session(&legacy_path(repo_root))?
    {
        return Ok(session);
    }
    Ok(Session::default())
}

/// Persist one source's session atomically (temp file then rename). Migrates
/// the working tree off the legacy file by removing it once the new file lands.
pub fn save_source(
    repo_root: &Path,
    source: &ReviewSource,
    session: &Session,
) -> Result<(), StoreError> {
    let dir = reviews_dir(repo_root);
    fs::create_dir_all(&dir)?;
    let gitignore = repo_root.join(DIR).join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n")?;
    }
    let on_disk = OnDisk {
        version: 1,
        source: Some(source.clone()),
        session: session.clone(),
    };
    let json = serde_json::to_string_pretty(&on_disk).map_err(std::io::Error::other)?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(json.as_bytes())?;
    tmp.persist(source_path(repo_root, source))
        .map_err(|e| StoreError::Io(e.error))?;
    if matches!(source, ReviewSource::WorkingTree) {
        let legacy = legacy_path(repo_root);
        if legacy.exists() {
            fs::remove_file(legacy)?;
        }
    }
    Ok(())
}

/// Every persisted review, for aggregating across sources (e.g. the MCP feed).
/// Ordered by key for deterministic output. A corrupt file fails the whole
/// call rather than silently vanishing.
pub fn load_all(repo_root: &Path) -> Result<Vec<(ReviewSource, Session)>, StoreError> {
    let mut reviews: Vec<(ReviewSource, Session)> = Vec::new();
    let dir = reviews_dir(repo_root);
    match fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path.extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                let raw = fs::read_to_string(&path)?;
                let on_disk: OnDisk =
                    serde_json::from_str(&raw).map_err(|e| StoreError::Corrupt(path.clone(), e))?;
                let source = on_disk.source.unwrap_or(ReviewSource::WorkingTree);
                reviews.push((source, on_disk.session));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    if !reviews
        .iter()
        .any(|(s, _)| matches!(s, ReviewSource::WorkingTree))
        && let Some(session) = read_session(&legacy_path(repo_root))?
    {
        reviews.push((ReviewSource::WorkingTree, session));
    }
    reviews.sort_by_key(|(source, _)| source.key());
    Ok(reviews)
}

/// Working-tree session, the common case.
pub fn load(repo_root: &Path) -> Result<Session, StoreError> {
    load_source(repo_root, &ReviewSource::WorkingTree)
}

/// Persist the working-tree session.
pub fn save(repo_root: &Path, session: &Session) -> Result<(), StoreError> {
    save_source(repo_root, &ReviewSource::WorkingTree, session)
}

#[cfg(test)]
mod tests {
    use crate::session::Anchor;

    use super::*;

    fn anchor() -> Anchor {
        Anchor {
            file: "a.txt".into(),
            line: Some(1),
            line_end: None,
            on_old_side: false,
            hunk: None,
            line_text: None,
        }
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = load(dir.path()).expect("load");
        assert_eq!(s, Session::default());
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = Session::default();
        s.add_comment("mattf", anchor(), "hm");
        s.mark_viewed("a.txt", "hash-1");
        save(dir.path(), &s).expect("save");
        let back = load(dir.path()).expect("load");
        assert_eq!(s, back);
    }

    #[test]
    fn save_writes_gitignore() {
        let dir = tempfile::tempdir().expect("tempdir");
        save(dir.path(), &Session::default()).expect("save");
        let gi = std::fs::read_to_string(dir.path().join(".diffler/.gitignore")).expect("read");
        assert_eq!(gi, "*\n");
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".diffler/reviews")).expect("mkdir");
        std::fs::write(
            dir.path().join(".diffler/reviews/working.json"),
            "{not json",
        )
        .expect("write");
        assert!(matches!(load(dir.path()), Err(StoreError::Corrupt(..))));
    }

    #[test]
    fn sources_persist_independently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut work = Session::default();
        work.mark_viewed("a.txt", "h-work");
        let mut commit = Session::default();
        commit.mark_viewed("a.txt", "h-commit");

        save_source(dir.path(), &ReviewSource::WorkingTree, &work).expect("save work");
        save_source(dir.path(), &ReviewSource::commit("abc"), &commit).expect("save commit");

        assert_eq!(load(dir.path()).expect("load work"), work);
        assert_eq!(
            load_source(dir.path(), &ReviewSource::commit("abc")).expect("load commit"),
            commit
        );
        // a path means different things per source; no collision
        assert!(
            load_source(dir.path(), &ReviewSource::commit("abc"))
                .expect("load")
                .is_viewed("a.txt", "h-commit")
        );
        assert!(
            !load(dir.path())
                .expect("load")
                .is_viewed("a.txt", "h-commit")
        );
    }

    #[test]
    fn legacy_file_migrates_to_working_on_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".diffler")).expect("mkdir");
        let legacy = dir.path().join(".diffler/session.json");
        std::fs::write(
            &legacy,
            r#"{"version":1,"comments":[],"viewed":{"a.txt":"h-legacy"}}"#,
        )
        .expect("write legacy");

        // read falls back to the legacy file
        let loaded = load(dir.path()).expect("load");
        assert!(loaded.is_viewed("a.txt", "h-legacy"));

        // saving migrates: new file written, legacy removed
        save(dir.path(), &loaded).expect("save");
        assert!(!legacy.exists(), "legacy file removed after migration");
        assert!(dir.path().join(".diffler/reviews/working.json").exists());
        assert_eq!(load(dir.path()).expect("reload"), loaded);
    }

    #[test]
    fn load_all_returns_every_source_sorted_by_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        save_source(dir.path(), &ReviewSource::WorkingTree, &Session::default()).expect("w");
        save_source(
            dir.path(),
            &ReviewSource::commit("bbb"),
            &Session::default(),
        )
        .expect("c");
        save_source(
            dir.path(),
            &ReviewSource::commit("aaa"),
            &Session::default(),
        )
        .expect("c");

        let all = load_all(dir.path()).expect("load_all");
        let keys: Vec<String> = all.iter().map(|(s, _)| s.key()).collect();
        assert_eq!(keys, ["commit-aaa", "commit-bbb", "working"]);
    }

    #[test]
    fn load_all_includes_legacy_working_before_migration() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".diffler")).expect("mkdir");
        std::fs::write(
            dir.path().join(".diffler/session.json"),
            r#"{"version":1,"comments":[],"viewed":{"a.txt":"h"}}"#,
        )
        .expect("write legacy");
        save_source(
            dir.path(),
            &ReviewSource::commit("abc"),
            &Session::default(),
        )
        .expect("c");

        let all = load_all(dir.path()).expect("load_all");
        let keys: Vec<String> = all.iter().map(|(s, _)| s.key()).collect();
        assert_eq!(keys, ["commit-abc", "working"]);
    }
}
