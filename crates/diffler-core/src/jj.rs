//! jj backend. Reads delegate to [`GitVcs`]: in a colocated repo git's HEAD
//! sits on `@-` and the worktree matches `@`; otherwise we open jj's own git
//! store and point its reads of HEAD at `@-`. Writes shell out to `jj`,
//! which owns the operation log and working-copy snapshot.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diffalgo::{DiffAlgorithm, DiffSettings};
use crate::git::GitVcs;
use crate::model::{DiffModel, HunkId};
use crate::vcs::{
    BlameSpan, Bookmark, BranchInfo, ChangeInfo, HeadInfo, JjView, LogEntry, NetworkOp,
    StatusModel, Vcs, VcsError,
};

const NO_DESCRIPTION: &str = "(no description set)";
const FIELD_SEPARATOR: char = '\x1f';
const LIST_SEPARATOR: char = '\x1e';
const COMMIT_ID_TEMPLATE: &str = r#"commit_id ++ "\n""#;
/// The bookmarked commits nearest `@`, which name the line `@` sits on.
const NEAREST_BOOKMARKS: &str = "heads(::@ & (bookmarks() | remote_bookmarks()))";
/// The history whose change ids a view carries, matching what the log walks.
const HISTORY: &str = "::(@ | bookmarks() | remote_bookmarks() | tags())";

pub struct JjVcs {
    git: GitVcs,
    root: PathBuf,
    colocated: bool,
    view: RefCell<JjView>,
    /// Whether this handle has learned `@-` from jj, which a repo with no
    /// `.git` needs before its first read of HEAD.
    base_known: Cell<bool>,
}

impl JjVcs {
    pub fn open(root: &Path) -> Result<Self, VcsError> {
        Self::open_with_settings(root, &DiffSettings::default())
    }

    pub fn open_with_settings(root: &Path, settings: &DiffSettings) -> Result<Self, VcsError> {
        let colocated = root.join(".git").exists();
        let git = if colocated {
            GitVcs::open_with_settings(root, settings)?
        } else {
            GitVcs::open_jj_store(&store_git_dir(root)?, root, settings)?
        };
        Ok(Self {
            git,
            root: root.to_path_buf(),
            colocated,
            view: RefCell::default(),
            base_known: Cell::new(false),
        })
    }

    /// `jj log` over `revset` with `template`, which a jj repo answers
    /// without snapshotting the working copy.
    fn query(
        &self,
        revset: &str,
        template: &str,
        limit: Option<usize>,
    ) -> Result<String, VcsError> {
        let mut args = vec![
            "log".to_owned(),
            "--no-graph".to_owned(),
            "--ignore-working-copy".to_owned(),
            format!("--revisions={revset}"),
            format!("--template={template}"),
        ];
        if let Some(limit) = limit {
            args.push(format!("--limit={limit}"));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&args)
    }

    /// `@`, its first parent and the nearest bookmark, plus every bookmark
    /// when the git store has none of its own.
    fn read_state(&self) -> Result<JjView, VcsError> {
        let revset = if self.colocated {
            format!("@ | {NEAREST_BOOKMARKS}")
        } else {
            "@ | bookmarks() | remote_bookmarks()".to_owned()
        };
        let marker = format!(
            r#"if(self.current_working_copy(), "@", if(self.contained_in("{NEAREST_BOOKMARKS}"), "^", "-"))"#
        );
        let template = line_template(&[
            &marker,
            "commit_id",
            "change_id.short(8)",
            "description.first_line()",
            &list_template("parents.map(|c| c.commit_id())"),
            &list_template("local_bookmarks.map(|b| b.name())"),
            &list_template(r#"remote_bookmarks.map(|b| b.name() ++ "@" ++ b.remote())"#),
            r#"committer.timestamp().format("%s")"#,
        ]);
        let out = self.query(&revset, &template, None)?;
        Ok(parse_state(&out, !self.colocated))
    }

    /// Short change id by commit id over the newest `limit` commits of
    /// [`HISTORY`].
    fn read_change_ids(&self, limit: usize) -> Result<HashMap<String, String>, VcsError> {
        let template = line_template(&["commit_id", "change_id.short(8)"]);
        let out = self.query(HISTORY, &template, Some(limit))?;
        Ok(out
            .lines()
            .filter_map(|line| line.split_once(FIELD_SEPARATOR))
            .map(|(oid, change)| (oid.to_owned(), change.to_owned()))
            .collect())
    }

    /// A repo with no `.git` learns `@-` from jj once per handle; a
    /// refresh's view moves it after that. Without it every read of HEAD
    /// would show an unborn repo, so a failing `jj` fails the read.
    fn ensure_base(&self) -> Result<(), VcsError> {
        if self.colocated || self.base_known.get() {
            return Ok(());
        }
        self.reread_state()
    }

    /// The state alone, keeping the change ids this handle already holds.
    fn reread_state(&self) -> Result<(), VcsError> {
        let state = self.read_state()?;
        let change_ids = std::mem::take(&mut self.view.borrow_mut().change_ids);
        self.adopt_jj_view(JjView {
            change_ids,
            ..state
        });
        Ok(())
    }

    /// Short change ids, jj's bookmarks and jj's wording for an empty
    /// description, and no `jj/keep/*` refs, which jj makes to keep commits
    /// from git's gc.
    fn decorated(&self, mut entries: Vec<LogEntry>) -> Vec<LogEntry> {
        let view = self.view.borrow();
        for entry in &mut entries {
            entry.change_id = view.change_ids.get(&entry.oid).cloned();
            if self.colocated {
                entry.refs.retain(|name| !name.starts_with("jj/keep/"));
            } else {
                entry.refs = view
                    .bookmarks
                    .iter()
                    .filter(|bookmark| bookmark.oid == entry.oid)
                    .map(|bookmark| bookmark.name.clone())
                    .collect();
            }
            if entry.subject.is_empty() {
                NO_DESCRIPTION.clone_into(&mut entry.subject);
            }
        }
        entries
    }

    /// The commit a jj bookmark (`name` or `name@remote`) sits on.
    fn bookmark_oid(&self, name: &str) -> Option<String> {
        self.view
            .borrow()
            .bookmarks
            .iter()
            .find(|bookmark| bookmark.name == name)
            .map(|bookmark| bookmark.oid.clone())
    }

    /// Exactly one commit `revision` names as a jj revset.
    fn resolve_revset(&self, revision: &str) -> Result<String, VcsError> {
        let out = self.query(revision, COMMIT_ID_TEMPLATE, Some(2))?;
        match out.lines().collect::<Vec<_>>().as_slice() {
            [oid] if is_root(oid) => Err(VcsError::Rejected(format!(
                "`{revision}` names jj's root commit, which git has no commit for; name another"
            ))),
            [oid] => Ok((*oid).to_owned()),
            [] => Err(VcsError::Rejected(format!(
                "`{revision}` matches no commit, name one that exists"
            ))),
            _ => Err(VcsError::Rejected(format!(
                "`{revision}` matches several commits, name one"
            ))),
        }
    }

    /// Run jj from the repo root, since jj resolves paths against the cwd. A
    /// failure carries jj's `Error:` line.
    fn run(&self, args: &[&str]) -> Result<String, VcsError> {
        let output = Command::new("jj")
            .current_dir(&self.root)
            .args(args)
            // we pass every message as an argument, so an editor jj opens
            // anyway would freeze the UI thread; we make it fail at once
            .env("JJ_EDITOR", "false")
            .output()
            .map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => VcsError::Rejected(
                    "install jj (jujutsu) on PATH to read and change this repo".into(),
                ),
                _ => VcsError::Io(err),
            })?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut lines = stderr.lines().map(str::trim);
        let message = lines
            .clone()
            .find(|line| line.starts_with("Error:"))
            .or_else(|| lines.find(|line| !line.is_empty()))
            .unwrap_or("run the jj command in a shell to see why it failed")
            .to_owned();
        Err(VcsError::Rejected(message))
    }

    /// Colocation exports jj's state to git synchronously, so HEAD already
    /// names the commit a write made; a repo with no `.git` asks jj again.
    fn head_oid(&self) -> Result<String, VcsError> {
        if !self.colocated {
            self.reread_state()?;
        }
        self.git.resolve("HEAD")
    }
}

/// The git store a jj repo without `.git` keeps; `git_target` names it
/// relative to `.jj/repo/store`, and it may sit outside the repo.
fn store_git_dir(root: &Path) -> Result<PathBuf, VcsError> {
    // a secondary workspace's `.jj/repo` is a file naming the main one
    if root.join(".jj/repo").is_file() {
        return Err(VcsError::Rejected(
            "open the repository's main jj workspace; secondary workspaces aren't supported yet"
                .into(),
        ));
    }
    let store = root.join(".jj/repo/store");
    let target = fs::read_to_string(store.join("git_target"))?;
    Ok(store.join(target.trim()))
}

/// `fields` joined into one line per commit.
fn line_template(fields: &[&str]) -> String {
    let separator = format!(r#" ++ "\x{:02x}" ++ "#, u32::from(FIELD_SEPARATOR));
    format!(r#"{} ++ "\n""#, fields.join(&separator))
}

fn list_template(list: &str) -> String {
    format!(r#"{list}.join("\x{:02x}")"#, u32::from(LIST_SEPARATOR))
}

fn split_list(list: &str) -> impl Iterator<Item = &str> {
    list.split(LIST_SEPARATOR).filter(|item| !item.is_empty())
}

/// The `git` remote is jj's mirror of a colocated repo's local bookmarks.
fn real_remotes(list: &str) -> impl Iterator<Item = &str> {
    split_list(list).filter(|name| !name.ends_with("@git"))
}

/// [`JjVcs::read_state`]'s rows as a view with no change ids. `bookmarks`
/// lists every bookmark the rows carry.
fn parse_state(out: &str, bookmarks: bool) -> JjView {
    let mut view = JjView::default();
    let mut nearest = None;
    for line in out.lines() {
        let fields: Vec<&str> = line.split(FIELD_SEPARATOR).collect();
        let &[
            marker,
            oid,
            change_id,
            description,
            parents,
            local,
            remote,
            time,
        ] = fields.as_slice()
        else {
            continue;
        };
        let bookmark = || split_list(local).chain(real_remotes(remote)).next();
        if marker == "@" {
            view.base = split_list(parents)
                .next()
                .filter(|id| !is_root(id))
                .map(str::to_owned);
            view.change = Some(ChangeInfo {
                change_id: change_id.to_owned(),
                bookmark: bookmark().map(str::to_owned),
                description: description.to_owned(),
            });
        } else if marker == "^" && nearest.is_none() {
            nearest = bookmark();
        }
        if bookmarks {
            let tip_unix = time.parse().unwrap_or_default();
            let names = split_list(local)
                .map(|name| (name, false))
                .chain(real_remotes(remote).map(|name| (name, true)));
            view.bookmarks.extend(names.map(|(name, remote)| Bookmark {
                name: name.to_owned(),
                oid: oid.to_owned(),
                remote,
                tip_unix,
            }));
        }
    }
    if let Some(change) = view.change.as_mut()
        && change.bookmark.is_none()
    {
        change.bookmark = nearest.map(str::to_owned);
    }
    view
}

/// `revision` split before a trailing git ancestry suffix: `main~2` is
/// `("main", "~2")`, `v2` has none.
fn split_ancestry(revision: &str) -> (&str, &str) {
    let kept = revision
        .trim_end_matches(|c: char| matches!(c, '~' | '^') || c.is_ascii_digit())
        .len();
    revision
        .get(kept..)
        .and_then(|tail| tail.find(['~', '^']))
        .map(|offset| kept + offset)
        .filter(|&at| at > 0)
        .and_then(|at| revision.split_at_checked(at))
        .unwrap_or((revision, ""))
}

/// jj's root commit, which the git store never holds.
fn is_root(oid: &str) -> bool {
    oid.bytes().all(|byte| byte == b'0')
}

/// `@` and the revsets built on it, which only jj reads right: git reads a
/// bare `@` as HEAD.
fn is_working_copy_revset(revision: &str) -> bool {
    revision == "@" || revision.starts_with("@-") || revision.starts_with("@+")
}

/// `s` as a jj string literal, since git names like `fix(x)` and `u@v` parse
/// as fileset or revset expressions when passed bare.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

const EMPTY_MESSAGE: &str = "write a commit message first";
const NO_STAGING: &str = "commit the whole working copy; jj has no staging area";
const NO_STASH: &str = "run jj new in a shell to set this change aside; jj has no stash";
const NO_PUSH_PULL: &str = "run jj git push or jj git fetch in a shell to sync a jj repo";

impl Vcs for JjVcs {
    fn has_index(&self) -> bool {
        false
    }

    fn native_git_checkout(&self) -> bool {
        false
    }

    fn git_dir(&self) -> Result<PathBuf, VcsError> {
        self.git.git_dir()
    }

    fn head(&self) -> Result<HeadInfo, VcsError> {
        self.ensure_base()?;
        let mut head = self.git.head()?;
        if head.oid7.is_empty() {
            head.branch = None;
        }
        Ok(head)
    }

    /// Change ids cover twice `log_limit`, since jj and git order a busy
    /// history differently.
    fn read_jj_view(&self, log_limit: usize) -> Result<Option<JjView>, VcsError> {
        let view = JjView {
            change_ids: self.read_change_ids(log_limit.saturating_mul(2))?,
            ..self.read_state()?
        };
        self.adopt_jj_view(view.clone());
        Ok(Some(view))
    }

    fn adopt_jj_view(&self, view: JjView) {
        if !self.colocated {
            self.base_known.set(true);
            // jj prints full hex ids, so the base always parses
            self.git.set_base(view.base.as_deref()).ok();
        }
        *self.view.borrow_mut() = view;
    }

    /// One diff pass of `@-` vs the working copy. jj marks a new file
    /// intent-to-add in the index, so git's three lists would count it twice.
    fn status(&self) -> Result<StatusModel, VcsError> {
        Ok(StatusModel {
            untracked: DiffModel::default(),
            unstaged: DiffModel::default(),
            staged: self.working_tree_diff()?,
        })
    }

    fn working_tree_diff(&self) -> Result<DiffModel, VcsError> {
        self.ensure_base()?;
        self.git.working_tree_diff()
    }

    fn tree_to_workdir_diff(&self, base_oid: &str) -> Result<DiffModel, VcsError> {
        self.git.tree_to_workdir_diff(base_oid)
    }

    fn commit_diff(&self, oid: &str) -> Result<DiffModel, VcsError> {
        self.git.commit_diff(oid)
    }

    fn range_diff(&self, oldest_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError> {
        self.git.range_diff(oldest_oid, newest_oid)
    }

    fn tree_diff(&self, base_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError> {
        self.git.tree_diff(base_oid, newest_oid)
    }

    fn merge_base(&self, a: &str, b: &str) -> Result<String, VcsError> {
        self.git.merge_base(a, b)
    }

    /// Git syntax (`main~2`, refs, oids, `HEAD` as `@-`) resolves through
    /// git, since a colocated repo's git refs are current where jj's import
    /// of them may lag. With no `.git` the store's refs are stale or absent,
    /// so jj's bookmarks name the commit there, and git applies the
    /// ancestry suffix. What git cannot read (`trunk()`, change ids) is a jj
    /// revset; a git suffix never goes to jj, which reads `~` as a set
    /// difference.
    fn resolve(&self, revision: &str) -> Result<String, VcsError> {
        if is_working_copy_revset(revision) {
            return self.resolve_revset(revision);
        }
        self.ensure_base()?;
        let (name, ancestry) = split_ancestry(revision);
        if !self.colocated
            && let Some(oid) = self.bookmark_oid(name)
        {
            return self.git.resolve(&format!("{oid}{ancestry}"));
        }
        let resolved = self.git.resolve(revision);
        // jj has no HEAD, so asking it would only cost a `jj` run
        if resolved.is_ok() || name == "HEAD" {
            return resolved;
        }
        if !ancestry.is_empty() {
            return Err(VcsError::Rejected(format!(
                "`{name}` names no bookmark, ref or commit; name one before `{ancestry}`"
            )));
        }
        self.resolve_revset(revision)
    }

    fn log(&self, limit: usize, all_branches: bool) -> Result<Vec<LogEntry>, VcsError> {
        self.ensure_base()?;
        let entries = {
            let view = self.view.borrow();
            let tips: Vec<&str> = view
                .bookmarks
                .iter()
                .map(|bookmark| bookmark.oid.as_str())
                .filter(|oid| !is_root(oid))
                .collect();
            self.git.log_from(limit, all_branches, &tips)?
        };
        Ok(self.decorated(entries))
    }

    fn default_branch(&self, remote: &str) -> Result<Option<String>, VcsError> {
        self.git.default_branch(remote)
    }

    fn commits_between(&self, base: &str, head: &str) -> Result<Vec<LogEntry>, VcsError> {
        self.ensure_base()?;
        Ok(self.decorated(self.git.commits_between(base, head)?))
    }

    fn unpushed(&self, limit: usize) -> Result<Option<Vec<LogEntry>>, VcsError> {
        self.ensure_base()?;
        Ok(self
            .git
            .unpushed(limit)?
            .map(|entries| self.decorated(entries)))
    }

    fn blame(&self, rel: &Path) -> Result<Vec<BlameSpan>, VcsError> {
        self.ensure_base()?;
        self.git.blame(rel)
    }

    fn read_at(&self, rev: &str, path: &str) -> Result<Option<String>, VcsError> {
        self.ensure_base()?;
        self.git.read_at(rev, path)
    }

    fn read_blob(&self, oid: &str) -> Result<Option<Vec<u8>>, VcsError> {
        self.git.read_blob(oid)
    }

    fn tracked_files(&self) -> Result<Vec<PathBuf>, VcsError> {
        self.ensure_base()?;
        self.git.tracked_files()
    }

    fn attr(&self, rel: &Path, name: &str) -> bool {
        self.git.attr(rel, name)
    }

    /// A repo with no `.git` lists jj's local bookmarks; the one `@` sits on
    /// or under counts as checked out.
    fn branches(&self) -> Result<Vec<BranchInfo>, VcsError> {
        if self.colocated {
            return self.git.branches();
        }
        let view = self.view.borrow();
        let here = view.change.as_ref().and_then(|c| c.bookmark.as_deref());
        Ok(view
            .bookmarks
            .iter()
            .filter(|bookmark| !bookmark.remote)
            .map(|bookmark| BranchInfo {
                name: bookmark.name.clone(),
                is_head: here == Some(bookmark.name.as_str()),
                tip_unix: bookmark.tip_unix,
                divergence: None,
            })
            .collect())
    }

    /// A repo with no `.git` measures a bookmark against its first remote
    /// namesake.
    fn divergence(&self, branch: &str) -> Result<Option<(usize, usize)>, VcsError> {
        if self.colocated {
            return self.git.divergence(branch);
        }
        let view = self.view.borrow();
        let local = view
            .bookmarks
            .iter()
            .find(|bookmark| !bookmark.remote && bookmark.name == branch);
        let upstream = view.bookmarks.iter().find(|bookmark| {
            bookmark.remote
                && bookmark
                    .name
                    .rsplit_once('@')
                    .is_some_and(|(name, _)| name == branch)
        });
        let (Some(local), Some(upstream)) = (local, upstream) else {
            return Ok(None);
        };
        self.git.ahead_behind(&local.oid, &upstream.oid).map(Some)
    }

    fn all_branches(&self) -> Result<Vec<String>, VcsError> {
        if self.colocated {
            return self.git.all_branches();
        }
        Ok(self
            .view
            .borrow()
            .bookmarks
            .iter()
            .map(|bookmark| bookmark.name.clone())
            .collect())
    }

    fn stage(&self, _rel: &Path) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn stage_everything(&self) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn unstage_everything(&self) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn stage_hunk(&self, _rel: &Path, _hunk: &HunkId) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn unstage(&self, _rel: &Path) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn unstage_hunk(&self, _rel: &Path, _hunk: &HunkId) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn discard(&self, rel: &Path) -> Result<(), VcsError> {
        let fileset = format!("root-file:{}", quoted(&rel.to_string_lossy()));
        self.run(&["restore", "--", &fileset])?;
        Ok(())
    }

    /// Messages go as `--message=<text>`: clap reads a separate value that
    /// starts with `-` as a flag.
    fn commit(&self, message: &str) -> Result<String, VcsError> {
        if message.trim().is_empty() {
            return Err(VcsError::Rejected(EMPTY_MESSAGE.into()));
        }
        self.run(&["commit", &format!("--message={message}")])?;
        self.head_oid()
    }

    fn head_message(&self) -> Result<String, VcsError> {
        self.ensure_base()?;
        self.git.head_message()
    }

    /// `None` squashes with `-u`, since a bare squash opens an editor when
    /// both sides are described. `Some` without `use_index` rewords `@-` alone.
    fn amend(&self, message: Option<&str>, use_index: bool) -> Result<String, VcsError> {
        if let Some(message) = message
            && message.trim().is_empty()
        {
            return Err(VcsError::Rejected(EMPTY_MESSAGE.into()));
        }
        match message {
            None => self.run(&["squash", "--use-destination-message"])?,
            Some(message) if use_index => self.run(&["squash", &format!("--message={message}")])?,
            Some(message) => {
                self.run(&["describe", "-r", "@-", &format!("--message={message}")])?
            }
        };
        self.head_oid()
    }

    /// Bookmarks `@`; `checkout` is moot since `@` is already checked out.
    fn create_branch(&self, name: &str, _checkout: bool) -> Result<(), VcsError> {
        self.run(&["bookmark", "create", "-r", "@", "--", name])?;
        Ok(())
    }

    /// `exact:`, since jj reads a bare name as a glob.
    fn delete_branch(&self, name: &str) -> Result<(), VcsError> {
        self.run(&[
            "bookmark",
            "delete",
            "--",
            &format!("exact:{}", quoted(name)),
        ])?;
        Ok(())
    }

    /// `jj new <rev>` works on top of the branch and keeps the change left
    /// behind; `jj edit` would rewrite the tip in place.
    fn checkout(&self, name: &str) -> Result<(), VcsError> {
        self.run(&["new", "--", &quoted(name)])?;
        Ok(())
    }

    fn stash_push(&self, _message: Option<&str>) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STASH.into()))
    }

    fn stash_pop(&self) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STASH.into()))
    }

    fn network_argv(&self, op: NetworkOp) -> Result<Vec<String>, VcsError> {
        let args: &[&str] = match op {
            NetworkOp::Fetch => &["git", "fetch"],
            NetworkOp::FetchAll => &["git", "fetch", "--all-remotes"],
            NetworkOp::Push
            | NetworkOp::PushSetUpstream { .. }
            | NetworkOp::Pull
            | NetworkOp::PullFrom { .. }
            | NetworkOp::PullRebase
            | NetworkOp::PullMerge => return Err(VcsError::Rejected(NO_PUSH_PULL.into())),
        };
        Ok(std::iter::once("jj")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect())
    }

    fn workdir(&self) -> Result<PathBuf, VcsError> {
        self.git.workdir()
    }

    fn remote_url(&self, name: &str) -> Result<Option<String>, VcsError> {
        self.git.remote_url(name)
    }

    fn remotes(&self) -> Result<Vec<String>, VcsError> {
        self.git.remotes()
    }

    fn set_diff_algorithm(&self, algorithm: DiffAlgorithm, indent_heuristic: bool) {
        self.git.set_diff_algorithm(algorithm, indent_heuristic);
    }
}
