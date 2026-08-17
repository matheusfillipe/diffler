//! Counting a repo, and counting a review.
//!
//! Two tallies over the same [`crate::language`] table: the working tree's
//! lines per language, which needs a read per file, and the diff's churn per
//! language, which the model already holds. Both are pure functions of what
//! they are handed, so the caller decides which thread pays.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::classify::{Kind, Rules};
use crate::git::GitVcs;
use crate::language;
use crate::model::FileDiff;
use crate::review::ReviewError;
use crate::vcs::Vcs;

/// Files past this size count as data: a checked-in dump would otherwise
/// decide the whole breakdown.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// How much of one language a repo holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageCount {
    pub name: &'static str,
    pub color: language::Rgb,
    pub files: usize,
    pub lines: usize,
    pub code: usize,
    pub comments: usize,
    pub blanks: usize,
    pub bytes: u64,
}

/// The whole scan: one entry per language, heaviest first, plus what it skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStats {
    pub languages: Vec<LanguageCount>,
    /// Files read but written in no language the table knows.
    pub unknown_files: usize,
    /// Files not read at all: binary, oversized, or unreadable.
    pub skipped_files: usize,
    /// Files left out as generated, lockfiles included.
    pub generated_files: usize,
}

impl RepoStats {
    #[must_use]
    pub fn totals(&self) -> LanguageCount {
        let mut total = LanguageCount {
            name: "total",
            color: (0, 0, 0),
            files: 0,
            lines: 0,
            code: 0,
            comments: 0,
            blanks: 0,
            bytes: 0,
        };
        for language in &self.languages {
            total.files += language.files;
            total.lines += language.lines;
            total.code += language.code;
            total.comments += language.comments;
            total.blanks += language.blanks;
            total.bytes += language.bytes;
        }
        total
    }
}

/// Count every path under `root`, grouped by language and ordered by code
/// lines. Reads each file once; a path that is binary, oversized or unreadable
/// is counted as skipped and never parsed.
///
/// What `rules` calls generated is left out, lockfiles included, the way a
/// repository page counts: a lockfile runs to thousands of lines and would
/// outrank the code beside it.
#[must_use]
pub fn scan(root: &Path, paths: &[PathBuf], rules: &Rules) -> RepoStats {
    let mut stats = RepoStats::default();
    let mut counts: HashMap<&'static str, LanguageCount> = HashMap::new();
    for path in paths {
        let relative = path.to_string_lossy();
        if rules.kind(&relative, None) == Kind::Generated {
            stats.generated_files += 1;
            continue;
        }
        let full = root.join(path);
        let Ok(metadata) = std::fs::metadata(&full) else {
            stats.skipped_files += 1;
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            stats.skipped_files += 1;
            continue;
        }
        let Ok(bytes) = std::fs::read(&full) else {
            stats.skipped_files += 1;
            continue;
        };
        if is_binary(&bytes) {
            stats.skipped_files += 1;
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let Some(language) = language::of_path(&relative) else {
            stats.unknown_files += 1;
            continue;
        };
        let (code, comments, blanks) = language::count_lines(&text, Some(language));
        let entry = counts.entry(language.name).or_insert(LanguageCount {
            name: language.name,
            color: language.color,
            files: 0,
            lines: 0,
            code: 0,
            comments: 0,
            blanks: 0,
            bytes: 0,
        });
        entry.files += 1;
        entry.lines += code + comments + blanks;
        entry.code += code;
        entry.comments += comments;
        entry.blanks += blanks;
        entry.bytes += metadata.len();
    }
    // heaviest first, and by name where two languages tie, so the table never
    // reshuffles between two scans of the same tree
    let mut languages: Vec<LanguageCount> = counts.into_values().collect();
    languages.sort_by(|a, b| b.code.cmp(&a.code).then_with(|| a.name.cmp(b.name)));
    stats.languages = languages;
    stats
}

/// Scan a checkout from its root, opening the backend here so the whole job,
/// the index read included, happens on the caller's thread. `extra` carries
/// what git does not track yet, the status screen's untracked files.
pub fn scan_repo(
    repo_root: &Path,
    extra: &[String],
    rules: &Rules,
) -> Result<RepoStats, ReviewError> {
    let vcs = GitVcs::open(repo_root)?;
    let mut paths = vcs.tracked_files()?;
    paths.extend(extra.iter().map(PathBuf::from));
    Ok(scan(repo_root, &paths, rules))
}

/// A NUL byte in the first block is what git itself treats as binary.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}

/// One language's share of a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageChurn {
    pub name: &'static str,
    pub color: language::Rgb,
    pub files: usize,
    pub added: usize,
    pub deleted: usize,
}

impl LanguageChurn {
    #[must_use]
    pub fn churn(&self) -> usize {
        self.added + self.deleted
    }
}

/// What the review is written in, busiest language first. Files in no known
/// language are left out: naming them would take a row from the languages the
/// reader can act on.
pub fn review_mix<'a>(files: impl IntoIterator<Item = &'a FileDiff>) -> Vec<LanguageChurn> {
    let mut totals: HashMap<&'static str, LanguageChurn> = HashMap::new();
    for file in files {
        let Some(language) = language::of_path(&file.path) else {
            continue;
        };
        let (added, deleted) = file.diffstat();
        let entry = totals.entry(language.name).or_insert(LanguageChurn {
            name: language.name,
            color: language.color,
            files: 0,
            added: 0,
            deleted: 0,
        });
        entry.files += 1;
        entry.added += added;
        entry.deleted += deleted;
    }
    let mut mix: Vec<LanguageChurn> = totals.into_values().collect();
    mix.sort_by(|a, b| b.churn().cmp(&a.churn()).then_with(|| a.name.cmp(b.name)));
    mix
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DiffLine, FileStatus, HashCache, Hunk, HunkId, LineKind};

    fn file(path: &str, added: usize, deleted: usize) -> FileDiff {
        let mut lines = Vec::new();
        for _ in 0..added {
            lines.push(DiffLine {
                kind: LineKind::Added,
                old_no: None,
                new_no: Some(1),
                text: "x".into(),
                emphasis: Vec::new(),
            });
        }
        for _ in 0..deleted {
            lines.push(DiffLine {
                kind: LineKind::Deleted,
                old_no: Some(1),
                new_no: None,
                text: "y".into(),
                emphasis: Vec::new(),
            });
        }
        FileDiff {
            path: path.to_owned(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            old_text: None,
            new_text: None,
            hunks: vec![Hunk {
                id: HunkId(String::new()),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                context: String::new(),
                lines,
            }],
            hashes: HashCache::default(),
        }
    }

    #[test]
    fn the_review_mix_groups_churn_by_language_busiest_first() {
        let files = vec![
            file("README.md", 3, 1),
            file("src/main.rs", 40, 10),
            file("src/lib.rs", 5, 5),
            file("data.bin", 99, 0),
        ];
        let mix = review_mix(&files);
        let shape: Vec<(&str, usize, usize, usize)> = mix
            .iter()
            .map(|entry| (entry.name, entry.files, entry.added, entry.deleted))
            .collect();
        assert_eq!(
            shape,
            vec![("Rust", 2, 45, 15), ("Markdown", 1, 3, 1)],
            "two rust files fold into one row; the binary has no language"
        );
    }

    #[test]
    fn scanning_counts_each_language_and_reports_what_it_left_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(
            root.join("src/main.rs"),
            "// note\nfn main() {\n\n    let x = 1;\n}\n",
        )
        .expect("write");
        std::fs::write(root.join("run.sh"), "#!/bin/sh\n# comment\necho hi\n").expect("write");
        std::fs::write(root.join("logo.bin"), [0u8, 1, 2, 3]).expect("write");
        std::fs::write(root.join("mystery.qqq"), "some text\n").expect("write");

        let paths = [
            PathBuf::from("src/main.rs"),
            PathBuf::from("run.sh"),
            PathBuf::from("logo.bin"),
            PathBuf::from("mystery.qqq"),
            PathBuf::from("gone.rs"),
        ];
        let stats = scan(root, &paths, &Rules::default());

        let shape: Vec<(&str, usize, usize, usize, usize)> = stats
            .languages
            .iter()
            .map(|l| (l.name, l.files, l.code, l.comments, l.blanks))
            .collect();
        assert_eq!(shape, vec![("Rust", 1, 3, 1, 1), ("Shell", 1, 2, 1, 0)]);
        assert_eq!(stats.unknown_files, 1, "the .qqq file has no language");
        assert_eq!(stats.skipped_files, 2, "the binary and the missing path");
        assert_eq!(stats.totals().lines, 8);
    }

    /// The worker hands `scan_repo` a root and nothing else, so it has to find
    /// the tracked files itself and fold in what git has not seen yet.
    #[test]
    fn scan_repo_counts_the_index_and_the_untracked_files_it_is_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let repo = crate::test_git::init_repo(root, Some("main"));
        std::fs::write(root.join("tracked.rs"), "fn main() {}\n").expect("write");
        let time = git2::Time::new(1_700_000_000, 0);
        let sig = git2::Signature::new("test", "test@test", &time).expect("sig");
        crate::test_git::commit_all(&repo, "base", &sig);
        std::fs::write(root.join("fresh.py"), "print(1)\n").expect("write");

        let counted = scan_repo(root, &[], &Rules::default()).expect("scan");
        assert_eq!(
            counted.languages.iter().map(|l| l.name).collect::<Vec<_>>(),
            vec!["Rust"],
            "the index alone knows nothing of fresh.py"
        );

        let both = scan_repo(root, &["fresh.py".to_owned()], &Rules::default()).expect("scan");
        assert_eq!(
            both.languages.iter().map(|l| l.name).collect::<Vec<_>>(),
            vec!["Python", "Rust"],
            "python outranks rust by a line, and the untracked file counts"
        );
    }

    #[test]
    fn an_empty_scan_totals_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stats = scan(dir.path(), &[], &Rules::default());
        assert!(stats.languages.is_empty());
        assert_eq!(stats.totals().code, 0);
    }
}
