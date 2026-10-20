//! Forge reviewers' pictures beside their comments. A worker reads the disk
//! cache or downloads from the forge, then encodes the picture for the terminal,
//! so a draw only places an image that is already encoded.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use diffler_core::session::Session;
use ratatui::layout::Size;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, Resize};

use super::{App, Flow};

/// Two cells by one row, a square at a typical cell aspect.
pub const AVATAR_SIZE: Size = Size::new(2, 1);

const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub enum AvatarState {
    Loading,
    Ready(Box<Protocol>),
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarRequest {
    pub login: String,
    pub url: String,
    /// `None` when no cache directory resolves; the picture is then fetched every run.
    pub path: Option<PathBuf>,
}

pub struct Fetched {
    pub login: String,
    pub protocol: Option<Box<Protocol>>,
}

impl std::fmt::Debug for Fetched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetched")
            .field("login", &self.login)
            .field("protocol", &self.protocol.is_some())
            .finish()
    }
}

/// A comment or reply the forge synced, written by someone other than the
/// human or the agent here, whose names are local and name no forge account.
pub(crate) fn is_forge_author(author: &str, remote: bool, human: &str) -> bool {
    remote && author != human && author != crate::mcp::AGENT_AUTHOR
}

fn forge_logins<'a>(session: &'a Session, human: &str) -> BTreeSet<&'a str> {
    let comments = session
        .comments
        .iter()
        .map(|comment| (comment.author.as_str(), comment.remote_id.is_some()));
    let replies = session.comments.iter().flat_map(|comment| {
        comment
            .replies
            .iter()
            .map(|reply| (reply.author.as_str(), reply.remote_id.is_some()))
    });
    comments
        .chain(replies)
        .filter(|(author, remote)| is_forge_author(author, *remote, human))
        .map(|(author, _)| author)
        .collect()
}

pub(crate) fn cache_path(base: &Path, host: &str, login: &str) -> PathBuf {
    base.join(host).join(format!("{login}.png"))
}

/// Runs on the blocking pool: a cached file younger than a week, else a
/// download, else a stale cached file, encoded for the terminal.
pub fn fetch(picker: &Picker, request: &AvatarRequest) -> Fetched {
    let path = request.path.as_deref();
    let bytes = path
        .and_then(|path| read_cached(path, true))
        .or_else(|| download_and_store(&request.url, path))
        .or_else(|| path.and_then(|path| read_cached(path, false)));
    let protocol = bytes
        .and_then(|bytes| image::load_from_memory(&bytes).ok())
        .and_then(|image| {
            picker
                .new_protocol(image, AVATAR_SIZE, Resize::Fit(Some(FilterType::Triangle)))
                .ok()
        })
        .map(Box::new);
    Fetched {
        login: request.login.clone(),
        protocol,
    }
}

fn read_cached(path: &Path, fresh_only: bool) -> Option<Vec<u8>> {
    if fresh_only {
        let age = std::fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .ok()?;
        if age > CACHE_MAX_AGE {
            return None;
        }
    }
    std::fs::read(path).ok()
}

fn download_and_store(url: &str, path: Option<&Path>) -> Option<Vec<u8>> {
    let bytes = crate::ci::download_avatar(url)?;
    if let Some(path) = path {
        store(path, &bytes);
    }
    Some(bytes)
}

/// Best-effort: a failed write only costs a download next run.
fn store(path: &Path, bytes: &[u8]) {
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_ok()
    {
        let _ = diffler_core::store::write_atomic(path, bytes);
    }
}

impl App {
    /// Halfblocks would draw a 2x1 avatar as two coloured cells, which reads as noise.
    pub(crate) fn avatars_shown(&self) -> bool {
        self.config.ui.avatars && self.image_picker.protocol_type() != ProtocolType::Halfblocks
    }

    /// Queue a picture for every forge author in the open review and the open
    /// PR pages that has none yet. A login is asked for once per run, failures
    /// included.
    pub(crate) fn queue_avatars(&mut self) {
        if !self.avatars_shown() {
            return;
        }
        let Some(remote) = self.ci_remotes.first() else {
            return;
        };
        let remote_url = remote.url.as_deref().unwrap_or_default();
        let page_logins = self.pr_page_logins();
        let mut logins: BTreeSet<&str> = page_logins
            .iter()
            .map(String::as_str)
            .filter(|login| is_forge_author(login, true, &self.author))
            .collect();
        if let Some(diff) = self.diff.as_ref() {
            logins.extend(forge_logins(
                self.review.session_for(&diff.source),
                &self.author,
            ));
        }
        let missing: Vec<&str> = logins
            .into_iter()
            .filter(|login| !self.avatars.contains_key(*login))
            .collect();
        if missing.is_empty() {
            return;
        }
        let cache =
            crate::config::xdg_dir("XDG_CACHE_HOME", ".cache").map(|dir| dir.join("avatars"));
        for login in missing {
            let Some((host, url)) = crate::ci::avatar_url(&remote.detected, remote_url, login)
            else {
                self.avatars.insert(login.to_owned(), AvatarState::Failed);
                continue;
            };
            self.avatars.insert(login.to_owned(), AvatarState::Loading);
            self.pending_avatars.push(AvatarRequest {
                login: login.to_owned(),
                url,
                path: cache.as_deref().map(|base| cache_path(base, &host, login)),
            });
        }
    }

    pub(crate) fn on_avatar(&mut self, fetched: Fetched) -> Flow {
        let Some(state) = self.avatars.get_mut(&fetched.login) else {
            return Flow::Idle;
        };
        if !matches!(state, AvatarState::Loading) {
            return Flow::Idle;
        }
        *state = fetched
            .protocol
            .map_or(AvatarState::Failed, AvatarState::Ready);
        Flow::Continue
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::CiRemote;
    use crate::ci::{Detected, ProviderKind};
    use crate::config::LoadedConfig;
    use crate::test_support::Fixture;

    pub(crate) fn kitty_picker() -> Picker {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        picker
    }

    /// The working-tree diff on a GitHub remote, holding a forge comment by
    /// `alice` with a forge reply by `bob` and a local agent reply, and a
    /// local comment by the human.
    pub(crate) fn forge_review(picker: Picker) -> (Fixture, App) {
        let fixture = crate::test_support::standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.image_picker = picker;
        app.ci_remotes = vec![CiRemote {
            name: "origin".to_owned(),
            detected: Detected {
                kind: ProviderKind::GitHub,
                host: None,
            },
            url: Some("git@github.com:acme/widgets.git".to_owned()),
        }];
        app.open_working_tree_diff(None);
        forge_thread(&mut app);
        (fixture, app)
    }

    fn forge_thread(app: &mut App) {
        let source = app.active_review_source();
        let human = app.author.clone();
        let session = app.review.session_for_mut(&source);
        let anchor = diffler_core::session::Anchor {
            file: "src/lib.rs".to_owned(),
            line: Some(2),
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        let forge = session
            .add_comment(anchor.clone(), "alice", "why 42?")
            .id
            .clone();
        session.reply(&forge, "bob", "seconded");
        session.reply(&forge, crate::mcp::AGENT_AUTHOR, "it is the answer");
        session.add_comment(anchor, &human, "mine");
        let comment = session
            .comments
            .iter_mut()
            .find(|comment| comment.id == forge)
            .expect("the forge comment");
        comment.remote_id = Some("101".to_owned());
        if let Some(reply) = comment.replies.first_mut() {
            reply.remote_id = Some("102".to_owned());
        }
        if let Some(diff) = app.diff.as_mut() {
            diff.invalidate();
        }
    }

    fn requested(app: &App) -> Vec<&str> {
        app.pending_avatars
            .iter()
            .map(|request| request.login.as_str())
            .collect()
    }

    #[test]
    fn a_cached_picture_is_named_after_its_host_and_login() {
        assert_eq!(
            cache_path(Path::new("/c"), "github.com", "reviewer"),
            PathBuf::from("/c/github.com/reviewer.png")
        );
    }

    #[test]
    fn halfblocks_asks_for_no_avatar() {
        let (_fixture, mut app) = forge_review(Picker::halfblocks());
        app.queue_avatars();
        assert!(app.pending_avatars.is_empty());
        app.image_picker = kitty_picker();
        app.config.ui.avatars = false;
        app.queue_avatars();
        assert!(app.pending_avatars.is_empty(), "the config turns it off");
    }

    #[test]
    fn each_forge_author_is_asked_for_once_and_a_failure_is_kept() {
        let (_fixture, mut app) = forge_review(kitty_picker());
        app.queue_avatars();
        assert_eq!(requested(&app), ["alice", "bob"], "local authors get none");
        assert_eq!(
            app.pending_avatars[0].url,
            "https://github.com/alice.png?size=64"
        );
        app.pending_avatars.clear();

        app.queue_avatars();
        assert!(
            app.pending_avatars.is_empty(),
            "in flight, so not asked again"
        );
        let flow = app.on_avatar(Fetched {
            login: "alice".to_owned(),
            protocol: None,
        });
        assert!(matches!(flow, Flow::Continue));
        assert!(matches!(
            app.avatars.get("alice"),
            Some(AvatarState::Failed)
        ));
        app.queue_avatars();
        assert!(app.pending_avatars.is_empty(), "a failure is not retried");
        assert!(
            matches!(
                app.on_avatar(Fetched {
                    login: "carol".to_owned(),
                    protocol: None,
                }),
                Flow::Idle
            ),
            "an answer nobody asked for is dropped"
        );
    }

    #[test]
    fn a_fresh_cached_picture_loads_and_a_missing_one_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = cache_path(dir.path(), "github.com", "alice");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, crate::app::image::tests::png(8, 8, [200, 30, 30])).expect("write");
        let unreachable = "file:///nonexistent/picture.png".to_owned();
        let cached = AvatarRequest {
            login: "alice".to_owned(),
            url: unreachable.clone(),
            path: Some(path),
        };
        assert!(fetch(&kitty_picker(), &cached).protocol.is_some());

        let missing = AvatarRequest {
            login: "bob".to_owned(),
            url: unreachable,
            path: Some(cache_path(dir.path(), "github.com", "bob")),
        };
        assert!(fetch(&kitty_picker(), &missing).protocol.is_none());
    }

    // the file URL below spells a unix path
    #[cfg(unix)]
    #[test]
    fn a_download_is_stored_in_the_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("served.png");
        std::fs::write(&source, crate::app::image::tests::png(8, 8, [30, 30, 200])).expect("write");
        let path = cache_path(&dir.path().join("cache"), "github.com", "alice");
        let request = AvatarRequest {
            login: "alice".to_owned(),
            url: format!("file://{}", source.display()),
            path: Some(path.clone()),
        };
        assert!(fetch(&kitty_picker(), &request).protocol.is_some());
        assert!(path.is_file(), "the picture is kept for the next run");
    }
}
