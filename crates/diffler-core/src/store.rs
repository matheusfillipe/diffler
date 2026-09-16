//! Session persistence: one file per review source under `.diffler/reviews/`,
//! atomically written, self-gitignored. The legacy single-session file
//! `.diffler/session.json` is read once and migrated to `reviews/working.json`.
//! A file written before a walkthrough was a source of its own carries it
//! embedded (`walkthroughs`, or the older singular `walkthrough`); reading
//! any such file splits each one out into its own `walkthrough-<id>.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::session::Session;
use crate::source::ReviewSource;
use crate::walkthrough::Walkthrough;

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

/// Every review [`load_all`] found, plus the path of any file it had to skip
/// because it would not parse.
pub type LoadedReviews = (Vec<(ReviewSource, Session)>, Vec<PathBuf>);

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

/// The pre-source shape of one embedded walkthrough. `comments` was the
/// owned-id list (every stop and the notes hanging off them); splitting reads
/// it to find what else to move, then drops the field for good.
#[derive(Debug, serde::Deserialize)]
struct LegacyWalkthrough {
    id: String,
    title: String,
    author: String,
    at: u64,
    #[serde(default)]
    stops: Vec<String>,
    #[serde(default)]
    comments: Vec<String>,
    #[serde(default)]
    skipped: Option<String>,
}

/// The embedded walkthrough(s) a pre-source review file may carry: several
/// under `walkthroughs`, or one under the older singular `walkthrough`. A
/// permissive read alongside the real [`OnDisk`] parse, so an unknown key
/// never fails the load.
#[derive(Debug, Default, serde::Deserialize)]
struct LegacyEmbedded {
    #[serde(default)]
    walkthroughs: Vec<LegacyWalkthrough>,
    #[serde(default, rename = "walkthrough")]
    singular: Option<LegacyWalkthrough>,
}

impl LegacyEmbedded {
    fn into_list(self) -> Vec<LegacyWalkthrough> {
        if self.walkthroughs.is_empty() {
            self.singular.into_iter().collect()
        } else {
            self.walkthroughs
        }
    }
}

/// Split every walkthrough `raw` carries embedded out of `session` into its
/// own `walkthrough-<id>.json`: its stops, and every comment anchored inside
/// one of those stops' own regions (a human reply included, since a
/// walkthrough is now its own world), move with it; the rest of `session`
/// keeps what is left. Returns whether anything moved, so the caller knows
/// whether the origin file needs resaving.
fn split_embedded_walkthroughs(
    repo_root: &Path,
    raw: &str,
    session: &mut Session,
) -> Result<bool, StoreError> {
    let legacy: Vec<LegacyWalkthrough> = serde_json::from_str::<LegacyEmbedded>(raw)
        .unwrap_or_default()
        .into_list();
    if legacy.is_empty() {
        return Ok(false);
    }
    for walkthrough in legacy {
        let mut owned: BTreeSet<String> = walkthrough.comments.iter().cloned().collect();
        owned.extend(walkthrough.stops.iter().cloned());
        for stop_id in &walkthrough.stops {
            let Some(region) = session
                .comments
                .iter()
                .find(|c| c.id == *stop_id)
                .map(|c| c.anchor.clone())
            else {
                continue;
            };
            owned.extend(
                session
                    .comments
                    .iter()
                    .filter(|c| crate::walkthrough::region_contains(&region, &c.anchor))
                    .map(|c| c.id.clone()),
            );
        }
        let mut moved = Vec::new();
        session.comments.retain(|c| {
            if owned.contains(&c.id) {
                moved.push(c.clone());
                false
            } else {
                true
            }
        });
        let seen: BTreeSet<String> = session
            .seen_stops
            .iter()
            .filter(|id| walkthrough.stops.contains(id))
            .cloned()
            .collect();
        session
            .seen_stops
            .retain(|id| !walkthrough.stops.contains(id));
        let split = Session {
            comments: moved,
            viewed: BTreeMap::new(),
            walkthrough: Some(Walkthrough {
                id: walkthrough.id.clone(),
                title: walkthrough.title,
                author: walkthrough.author,
                at: walkthrough.at,
                stops: walkthrough.stops,
                skipped: walkthrough.skipped,
                summary: None,
                // a walkthrough this old predates the field entirely, so its
                // anchors resolve against the live worktree
                rev: None,
            }),
            seen_stops: seen,
        };
        save_source(
            repo_root,
            &ReviewSource::Walkthrough { id: walkthrough.id },
            &split,
        )?;
    }
    Ok(true)
}

/// Read one file's session and its own declared source (`WorkingTree` for a
/// file predating that field). A walkthrough's own file never carries an
/// embedded one, so splitting only ever runs for every other source.
fn read_session(
    repo_root: &Path,
    path: &Path,
) -> Result<Option<(ReviewSource, Session)>, StoreError> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let on_disk: OnDisk = serde_json::from_str(&raw)
                .map_err(|e| StoreError::Corrupt(path.to_path_buf(), e))?;
            let origin = on_disk.source.unwrap_or(ReviewSource::WorkingTree);
            let mut session = on_disk.session;
            if !matches!(origin, ReviewSource::Walkthrough { .. })
                && split_embedded_walkthroughs(repo_root, &raw, &mut session)?
            {
                save_source(repo_root, &origin, &session)?;
            }
            Ok(Some((origin, session)))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Load the session for one source. The working tree falls back to the legacy
/// single-session file when no per-source file exists yet; the file is moved on
/// the next [`save_source`].
pub fn load_source(repo_root: &Path, source: &ReviewSource) -> Result<Session, StoreError> {
    if let Some((_, session)) = read_session(repo_root, &source_path(repo_root, source))? {
        return Ok(session);
    }
    if matches!(source, ReviewSource::WorkingTree)
        && let Some((_, session)) = read_session(repo_root, &legacy_path(repo_root))?
    {
        return Ok(session);
    }
    Ok(Session::default())
}

/// Remove a source's review file entirely, e.g. deleting a walkthrough
/// deletes its `walkthrough-<id>.json`. A missing file is not an error.
pub fn delete_source(repo_root: &Path, source: &ReviewSource) -> Result<(), StoreError> {
    match fs::remove_file(source_path(repo_root, source)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
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

/// Every persisted review, for aggregating across sources (e.g. the MCP feed),
/// plus the path of any review file that failed to parse. Ordered by key for
/// deterministic output. A corrupt file is skipped rather than failing the
/// whole call, so one bad file never hides every other review; its path
/// comes back in the second list, for a caller that wants to tell the
/// reader. The directory listing is taken up front, so a split a file
/// triggers mid-scan never feeds back into this same call.
pub fn load_all(repo_root: &Path) -> Result<LoadedReviews, StoreError> {
    let mut reviews: Vec<(ReviewSource, Session)> = Vec::new();
    let mut corrupt: Vec<PathBuf> = Vec::new();
    let dir = reviews_dir(repo_root);
    match fs::read_dir(&dir) {
        Ok(entries) => {
            let paths: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            for path in paths {
                match read_session(repo_root, &path) {
                    Ok(Some(pair)) => reviews.push(pair),
                    Ok(None) => {}
                    Err(StoreError::Corrupt(path, _)) => corrupt.push(path),
                    Err(err) => return Err(err),
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    if !reviews
        .iter()
        .any(|(s, _)| matches!(s, ReviewSource::WorkingTree))
        && let Some((source, session)) = read_session(repo_root, &legacy_path(repo_root))?
    {
        reviews.push((source, session));
    }
    reviews.sort_by_key(|(source, _)| source.key());
    Ok((reviews, corrupt))
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
    use crate::test_support::anchor;

    use super::*;

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
        s.add_comment(anchor("a.txt", Some(1)), "reviewer", "hm");
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

    /// A `working.json` carrying the pre-source `walkthroughs` list splits
    /// each entry into its own `walkthrough-<id>.json`: a stop's own comment,
    /// a note the legacy `comments` list names, and a human reply anchored in
    /// the stop's region all move; a comment untouched by any stop stays
    /// with the working session.
    #[test]
    fn a_review_file_with_the_old_walkthroughs_list_splits_each_one_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".diffler/reviews")).expect("mkdir");
        std::fs::write(
            dir.path().join(".diffler/reviews/working.json"),
            r#"{"version":1,"comments":[
                {"id":"stop-0","author":"agent","anchor":{"file":"a.txt","line":1},"title":"first","anchor_ref":"a.txt:1","body":"why","status":"open","at":1},
                {"id":"human-0","author":"reviewer","anchor":{"file":"a.txt","line":1},"body":"a reply","status":"open","at":1},
                {"id":"unrelated","author":"reviewer","anchor":{"file":"b.txt","line":1},"body":"unrelated","status":"open","at":1}
            ],"viewed":{"a.txt":"h"},"walkthroughs":[
                {"id":"w1","title":"tour","author":"agent","at":1,"stops":["stop-0"],"comments":["stop-0"]}
            ]}"#,
        )
        .expect("write");

        let working = load(dir.path()).expect("load working");
        assert!(working.walkthrough.is_none());
        let ids: Vec<&str> = working.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["unrelated"], "only the working session's own comment");
        assert_eq!(
            working.viewed.get("a.txt").map(String::as_str),
            Some("h"),
            "file-viewed marks are the working tree's own and stay"
        );

        let split = load_source(dir.path(), &ReviewSource::walkthrough("w1")).expect("load w1");
        let walkthrough = split.walkthrough.expect("walkthrough");
        assert_eq!(walkthrough.id, "w1");
        assert_eq!(walkthrough.stops, ["stop-0".to_owned()]);
        let ids: Vec<&str> = split.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids.into_iter().collect::<std::collections::BTreeSet<_>>(),
            ["stop-0", "human-0"].into_iter().collect(),
            "the stop and the human reply in its region both moved"
        );

        // idempotent: loading again finds nothing left to split
        let reloaded = load(dir.path()).expect("reload");
        assert!(reloaded.walkthrough.is_none());
    }

    /// The older singular `walkthrough` field (from before a review could
    /// hold more than one) migrates the same way, as a one-element list.
    #[test]
    fn a_review_file_with_the_old_singular_walkthrough_key_splits_it_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".diffler/reviews")).expect("mkdir");
        std::fs::write(
            dir.path().join(".diffler/reviews/working.json"),
            r#"{"version":1,"comments":[
                {"id":"stop-0","author":"agent","anchor":{"file":"a.txt","line":1},"title":"first","anchor_ref":"a.txt:1","body":"why","status":"open","at":1}
            ],"viewed":{},"walkthrough":{"id":"w1","title":"tour","author":"agent","at":1,"stops":["stop-0"],"comments":["stop-0"]}}"#,
        )
        .expect("write");

        let working = load(dir.path()).expect("load working");
        assert!(working.comments.is_empty());
        let split = load_source(dir.path(), &ReviewSource::walkthrough("w1")).expect("load w1");
        assert_eq!(split.walkthrough.expect("walkthrough").id, "w1");
        assert_eq!(split.comments.len(), 1);
    }

    #[test]
    fn delete_source_removes_the_file_and_a_missing_one_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = ReviewSource::walkthrough("w1");
        save_source(dir.path(), &source, &Session::default()).expect("save");
        assert!(
            load_all(dir.path())
                .expect("load_all")
                .0
                .iter()
                .any(|(s, _)| *s == source)
        );

        delete_source(dir.path(), &source).expect("delete");
        assert!(
            !load_all(dir.path())
                .expect("load_all")
                .0
                .iter()
                .any(|(s, _)| *s == source)
        );
        delete_source(dir.path(), &source).expect("delete missing is a no-op");
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

        let (all, corrupt) = load_all(dir.path()).expect("load_all");
        let keys: Vec<String> = all.iter().map(|(s, _)| s.key()).collect();
        assert_eq!(keys, ["commit-aaa", "commit-bbb", "working"]);
        assert!(corrupt.is_empty());
    }

    /// A review file that fails to parse is skipped, not fatal: every other
    /// review still loads, and the bad file's path comes back so a caller
    /// can tell the reader.
    #[test]
    fn load_all_skips_a_corrupt_file_and_names_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        save_source(dir.path(), &ReviewSource::WorkingTree, &Session::default()).expect("w");
        save_source(
            dir.path(),
            &ReviewSource::commit("abc"),
            &Session::default(),
        )
        .expect("c");
        let bad = dir.path().join(".diffler/reviews/walkthrough-broken.json");
        std::fs::write(&bad, "{not json").expect("write corrupt file");

        let (all, corrupt) = load_all(dir.path()).expect("load_all");
        let keys: Vec<String> = all.iter().map(|(s, _)| s.key()).collect();
        assert_eq!(keys, ["commit-abc", "working"], "the good files still load");
        assert_eq!(corrupt, vec![bad]);
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

        let (all, _corrupt) = load_all(dir.path()).expect("load_all");
        let keys: Vec<String> = all.iter().map(|(s, _)| s.key()).collect();
        assert_eq!(keys, ["commit-abc", "working"]);
    }
}
