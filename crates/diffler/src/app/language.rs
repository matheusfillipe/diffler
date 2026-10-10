//! The `gl` picker: set the language a file highlights as when its name and
//! first line name the wrong one, for this run or saved to the project's
//! config as a rule for the file or for every file like it.

use std::collections::BTreeMap;
use std::sync::Arc;

use crossterm::event::KeyEvent;
use diffler_core::highlight::{Highlighter, SyntaxTheme};
use diffler_core::syntax::registry::REGISTRY;

use super::enrich::EnrichStamp;
use super::fuzzy::{FuzzyKey, FuzzyList, selected};
use super::{App, ChoiceKind, Flow, Modal, Screen};

/// How long a picked language holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageScope {
    /// Until diffler quits.
    Session,
    /// A `[syntax]` rule for this glob, saved to the project's config.
    Saved(String),
}

impl LanguageScope {
    /// The scope as the picker lists it.
    pub fn label(&self) -> String {
        match self {
            Self::Session => "use it until diffler quits".to_owned(),
            Self::Saved(glob) => format!("save it for {glob} in this project"),
        }
    }
}

/// Every bundled grammar's name, as the picker lists them.
pub fn names() -> Vec<String> {
    REGISTRY.names().into_iter().map(str::to_owned).collect()
}

impl App {
    /// `gl`: pick the language the file on screen highlights as.
    pub(crate) fn open_language_picker(&mut self) {
        if self.file_on_screen().is_none() {
            self.info("open a file to set its language");
            return;
        }
        self.open_choice_picker(ChoiceKind::Language);
    }

    pub(super) fn handle_language_key(&mut self, key: &KeyEvent) -> Flow {
        let Some(Modal::LanguageScope {
            path,
            language,
            scopes,
            list,
        }) = self.modal.as_mut()
        else {
            return Flow::Continue;
        };
        match list.feed(key) {
            FuzzyKey::Submit => {
                if let Some(scope) = selected(list, scopes).cloned() {
                    let (path, language) = (path.clone(), language.clone());
                    self.modal = None;
                    self.set_language(&path, &language, scope);
                }
            }
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited | FuzzyKey::Consumed | FuzzyKey::Other => {}
        }
        Flow::Continue
    }

    /// The language the file on screen highlights as now.
    pub(crate) fn language_on_screen(&self) -> Option<String> {
        let path = self.file_on_screen()?;
        let first_line = match self.screen() {
            Screen::File => self.file.as_ref()?.lines.first().cloned(),
            _ => None,
        };
        let entry = self
            .highlighter
            .language(&path, first_line.as_deref().unwrap_or_default())?;
        Some(entry.name.to_owned())
    }

    /// Ask how long `language` holds for the file on screen.
    pub(crate) fn open_scope_picker(&mut self, language: &str) {
        let Some(path) = self.file_on_screen() else {
            return;
        };
        let language = language.to_owned();
        let mut scopes = vec![
            LanguageScope::Session,
            LanguageScope::Saved(anchored(&path)),
        ];
        if let Some(glob) = sibling_glob(&path) {
            scopes.push(LanguageScope::Saved(glob));
        }
        let mut list = FuzzyList::default();
        list.rerank(&scopes.iter().map(LanguageScope::label).collect::<Vec<_>>());
        self.modal = Some(Modal::LanguageScope {
            path,
            language,
            scopes,
            list,
        });
    }

    fn set_language(&mut self, path: &str, language: &str, scope: LanguageScope) {
        match scope {
            LanguageScope::Session => {
                let glob = anchored(path);
                self.language_picks.retain(|(picked, _)| *picked != glob);
                self.language_picks.push((glob, language.to_owned()));
                self.info(format!("set {path} to {language} until diffler quits"));
            }
            LanguageScope::Saved(glob) => {
                let own = anchored(path);
                self.language_picks.retain(|(picked, _)| *picked != own);
                if let Err(err) =
                    crate::config::save_syntax_rule(&self.review.repo_root, &glob, language)
                {
                    self.error(format!("cannot save the rule: {err}"));
                    return;
                }
                self.config.syntax.insert(glob.clone(), language.to_owned());
                self.info(format!("saved {glob} as {language} in this project"));
            }
        }
        self.rebuild_highlighter();
        // we parsed the lens and the walkthrough anchors under the old grammar
        if let Some(diff) = self.diff.as_mut() {
            diff.drop_lens();
        }
        self.queue_walkthrough_anchors();
    }

    /// Build the highlighter again from the theme and the rules as they
    /// stand, then colour everything on screen with it.
    pub(crate) fn rebuild_highlighter(&mut self) {
        self.highlighter = Arc::new(highlighter(
            self.theme.syntax,
            &self.language_picks,
            &self.config.syntax,
        ));
        self.highlighter_generation += 1;
        self.enrich_inflight.clear();
        self.pending_enrich.clear();
        if let Some(diff) = self.diff.as_mut() {
            diff.highlights.clear();
            diff.invalidate();
        }
        self.status.highlights.clear();
        self.reload_file();
        self.queue_enrich_selected();
    }

    pub(crate) fn enrich_stamp(&self) -> EnrichStamp {
        EnrichStamp {
            algorithm: self.config.diff.algorithm,
            highlighter: self.highlighter_generation,
        }
    }

    /// The repository path of the file the reader is looking at.
    fn file_on_screen(&self) -> Option<String> {
        match self.screen() {
            Screen::File => self.file.as_ref().map(|view| view.path.clone()),
            Screen::Diff => self.diff.as_ref()?.selected_path(&self.review),
            _ => None,
        }
    }
}

/// A highlighter for `syntax`'s palette under the reader's rules: the
/// languages picked this run, newest first, then the config's globs, those
/// without a wildcard first and then the longer before the shorter, so a
/// rule for one file beats one for its folder or its extension.
pub fn highlighter(
    syntax: SyntaxTheme,
    picks: &[(String, String)],
    config: &BTreeMap<String, String>,
) -> Highlighter {
    let mut saved: Vec<(String, String)> = config.clone().into_iter().collect();
    saved.sort_by_key(|(glob, _)| {
        let broad = glob.contains(['*', '?']) || glob.ends_with('/');
        (broad, std::cmp::Reverse(glob.len()))
    });
    Highlighter::new(syntax).with_rules(picks.iter().rev().cloned().chain(saved).collect())
}

/// The glob for `path` alone, anchored at the repository root.
fn anchored(path: &str) -> String {
    format!("/{path}")
}

/// The rule that covers `path` and every file named like it: its extension,
/// or for a file with none, its exact name at any depth.
fn sibling_glob(path: &str) -> Option<String> {
    let name = std::path::Path::new(path).file_name()?.to_str()?;
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => Some(format!("*.{ext}")),
        _ => Some(name.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::*;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::Fixture;

    #[test]
    fn a_rule_for_one_file_outranks_one_for_its_extension() {
        let config = BTreeMap::from([
            ("*.yml".to_owned(), "toml".to_owned()),
            ("/ci/a.yml".to_owned(), "bash".to_owned()),
            ("ci/".to_owned(), "json".to_owned()),
            ("ci/*.yml".to_owned(), "python".to_owned()),
        ]);
        let hl = highlighter(SyntaxTheme::default(), &[], &config);
        let name = |path| hl.language(path, "").map(|entry| entry.name);
        assert_eq!(name("ci/a.yml"), Some("bash"));
        assert_eq!(name("ci/b.yml"), Some("python"));
        assert_eq!(name("ci/c.txt"), Some("json"));
        assert_eq!(name("b.yml"), Some("toml"));
    }

    #[test]
    fn a_saved_language_highlights_the_file_and_lands_in_config() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "one\n");
        fixture.commit_all("base");
        fixture.write("a.txt", "key: two\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let press = |app: &mut App, code| app.handle(AppEvent::Key(KeyEvent::from(code)));
        for c in "glyaml".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert!(
            app.pending_enrich
                .iter()
                .all(|job| job.stamp == app.enrich_stamp())
        );
        let saved = std::fs::read_to_string(fixture.root.join(".diffler/config.toml"))
            .expect("the project config");
        assert!(saved.contains("\"/a.txt\" = \"yaml\""), "{saved}");
        let lang = app
            .highlighter
            .language("a.txt", "")
            .map(|entry| entry.name);
        assert_eq!(lang, Some("yaml"));
    }

    #[test]
    fn the_rule_for_files_like_this_one_follows_its_extension_or_name() {
        assert_eq!(sibling_glob("deploy/app.env").as_deref(), Some("*.env"));
        assert_eq!(sibling_glob("Dockerfile").as_deref(), Some("Dockerfile"));
        assert_eq!(sibling_glob(".envrc").as_deref(), Some(".envrc"));
    }
}
