//! Session persistence: `.diffler/session.json` inside the repo,
//! atomically written, self-gitignored.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::session::Session;

const DIR: &str = ".diffler";
const FILE: &str = "session.json";

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
    #[serde(flatten)]
    session: Session,
}

pub fn session_path(repo_root: &Path) -> PathBuf {
    repo_root.join(DIR).join(FILE)
}

pub fn load(repo_root: &Path) -> Result<Session, StoreError> {
    let path = session_path(repo_root);
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let on_disk: OnDisk =
                serde_json::from_str(&raw).map_err(|e| StoreError::Corrupt(path, e))?;
            Ok(on_disk.session)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Session::default()),
        Err(err) => Err(err.into()),
    }
}

/// Atomic write: temp file in the same directory, then rename, so a crash
/// mid-write never corrupts the session. The dir self-gitignores.
pub fn save(repo_root: &Path, session: &Session) -> Result<(), StoreError> {
    let dir = repo_root.join(DIR);
    fs::create_dir_all(&dir)?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n")?;
    }
    let on_disk = OnDisk {
        version: 1,
        session: session.clone(),
    };
    let json = serde_json::to_string_pretty(&on_disk).map_err(std::io::Error::other)?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(json.as_bytes())?;
    tmp.persist(session_path(repo_root))
        .map_err(|e| StoreError::Io(e.error))?;
    Ok(())
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
        std::fs::create_dir_all(dir.path().join(".diffler")).expect("mkdir");
        std::fs::write(dir.path().join(".diffler/session.json"), "{not json").expect("write");
        assert!(matches!(load(dir.path()), Err(StoreError::Corrupt(..))));
    }
}
