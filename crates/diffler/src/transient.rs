//! Magit-style transient menus: a prefix key opens a menu whose keys are all
//! leaves. The model is plain data so resolution and the which-key layout stay
//! unit-testable; the app owns the live state and timer.

use crate::config::{KeyPress, KeysConfig, single_press};
use crate::keymap::{Action, render_chord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientKind {
    Commit,
    Branch,
    Diff,
    Log,
    Push,
    Pull,
    Fetch,
    Stash,
}

impl TransientKind {
    /// The `[keys.<section>]` table that overrides this transient's sub-keys.
    pub fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Branch => "branch",
            // `[keys.diff]` is already the diff screen's keymap
            Self::Diff => "diff_menu",
            Self::Log => "log_menu",
            Self::Push => "push",
            Self::Pull => "pull",
            Self::Fetch => "fetch",
            Self::Stash => "stash",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Commit => "Commit",
            Self::Branch => "Branch",
            Self::Diff => "Diff against",
            Self::Log => "Log",
            Self::Push => "Push",
            Self::Pull => "Pull",
            Self::Fetch => "Fetch",
            Self::Stash => "Stash",
        }
    }

    pub const ALL: [Self; 8] = [
        Self::Commit,
        Self::Branch,
        Self::Diff,
        Self::Log,
        Self::Push,
        Self::Pull,
        Self::Fetch,
        Self::Stash,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientEntry {
    pub key: KeyPress,
    pub action: Action,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientGroup {
    pub heading: &'static str,
    pub entries: Vec<TransientEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transient {
    pub kind: TransientKind,
    pub groups: Vec<TransientGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientResolve {
    Action(Action),
    /// The caller closes the transient and beeps, as neogit does.
    Unbound,
}

/// `(config key, chord, action, label)`; the override is
/// `[keys.<section>] <config key> = "<chord>"`.
type DefaultEntry = (&'static str, &'static str, Action, &'static str);

type DefaultGroup = (&'static str, &'static [DefaultEntry]);

const COMMIT_GROUPS: &[DefaultGroup] = &[(
    "Create",
    &[
        ("commit", "c", Action::CommitFlow, "Commit"),
        ("extend", "e", Action::CommitExtend, "Extend"),
        ("amend", "a", Action::CommitAmend, "Amend"),
        ("reword", "w", Action::CommitReword, "Reword"),
    ],
)];

const BRANCH_GROUPS: &[DefaultGroup] = &[(
    "Switch and create",
    &[
        ("checkout", "b", Action::BranchCheckout, "Checkout branch"),
        (
            "create_checkout",
            "c",
            Action::BranchCreateCheckout,
            "Create and checkout",
        ),
        ("create", "n", Action::BranchCreate, "Create"),
        ("delete", "D", Action::BranchDelete, "Delete"),
        ("prs", "p", Action::OpenPrs, "Pull requests"),
        ("pr_create", "P", Action::CreatePr, "Open a pull request"),
    ],
)];

const DIFF_GROUPS: &[DefaultGroup] = &[(
    "Diff the working tree against",
    &[
        ("base", "d", Action::DiffBase, "Base branch"),
        ("last_commit", "c", Action::DiffLastCommit, "Last commit"),
        ("branch", "b", Action::DiffBranch, "A branch"),
        ("commit", "s", Action::DiffCommit, "A commit from the log"),
        (
            "working",
            "w",
            Action::DiffWorkingTree,
            "HEAD (working tree)",
        ),
    ],
)];

const LOG_GROUPS: &[DefaultGroup] = &[(
    "Log",
    &[("current", "l", Action::LogView, "Current branch")],
)];

const PUSH_GROUPS: &[DefaultGroup] = &[(
    "Push to",
    &[
        ("push", "p", Action::Push, "Push"),
        (
            "set_upstream",
            "u",
            Action::PushSetUpstream,
            "Push and set upstream",
        ),
    ],
)];

const PULL_GROUPS: &[DefaultGroup] = &[("Pull from", &[("pull", "p", Action::Pull, "Pull")])];

const FETCH_GROUPS: &[DefaultGroup] = &[(
    "Fetch from",
    &[
        ("fetch", "f", Action::Fetch, "Fetch"),
        ("all", "a", Action::FetchAll, "Fetch all remotes"),
    ],
)];

const STASH_GROUPS: &[DefaultGroup] = &[(
    "Stash",
    &[
        ("push", "z", Action::StashPush, "Stash changes"),
        ("pop", "p", Action::StashPop, "Pop latest stash"),
    ],
)];

impl TransientKind {
    fn default_groups(self) -> &'static [DefaultGroup] {
        match self {
            Self::Commit => COMMIT_GROUPS,
            Self::Branch => BRANCH_GROUPS,
            Self::Diff => DIFF_GROUPS,
            Self::Log => LOG_GROUPS,
            Self::Push => PUSH_GROUPS,
            Self::Pull => PULL_GROUPS,
            Self::Stash => STASH_GROUPS,
            Self::Fetch => FETCH_GROUPS,
        }
    }
}

impl Transient {
    /// Applies `[keys.<section>]` overrides; a bad chord or a clash warns and
    /// falls back to the default.
    pub fn build(kind: TransientKind, keys: &KeysConfig) -> (Self, Vec<String>) {
        let section = kind.name();
        let overrides = keys.transient(kind);
        let mut warnings = Vec::new();
        let mut groups = Vec::new();
        for (heading, entries) in kind.default_groups() {
            let mut built = Vec::new();
            for (config_key, default_chord, action, label) in *entries {
                // a default that fails to parse vanishes; defaults_are_conflict_free guards it
                let Some(default_key) = single_press(default_chord) else {
                    continue;
                };
                let key = match overrides
                    .get(*config_key)
                    .map(|chord| (chord, single_press(chord)))
                {
                    None => default_key,
                    Some((_, Some(key))) => key,
                    Some((chord, None)) => {
                        warnings.push(format!(
                            "[keys.{section}] {config_key}: chord {chord:?} must be a single key; using default"
                        ));
                        default_key
                    }
                };
                built.push(TransientEntry {
                    key,
                    action: *action,
                    label,
                });
            }
            groups.push(TransientGroup {
                heading,
                entries: built,
            });
        }
        let mut transient = Self { kind, groups };
        warnings.extend(transient.resolve_conflicts(section));
        (transient, warnings)
    }

    /// A later entry on a taken chord falls back to its default when that is
    /// free, else drops; each clash warns.
    fn resolve_conflicts(&mut self, section: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut seen: Vec<KeyPress> = Vec::new();
        for group in &mut self.groups {
            for entry in &mut group.entries {
                if seen.contains(&entry.key) {
                    let clashing = render_chord(std::slice::from_ref(&entry.key));
                    let default = entry
                        .action
                        .name()
                        .pipe_default_key(self.kind)
                        .filter(|key| !seen.contains(key));
                    match default {
                        Some(default_key) => {
                            warnings.push(format!(
                                "[keys.{section}] {} clashes on {clashing}; using its default",
                                entry.action.name()
                            ));
                            entry.key = default_key;
                            seen.push(entry.key.clone());
                        }
                        None => {
                            warnings.push(format!(
                                "[keys.{section}] {} clashes on {clashing}; binding dropped",
                                entry.action.name()
                            ));
                        }
                    }
                } else {
                    seen.push(entry.key.clone());
                }
            }
        }
        warnings
    }

    pub fn resolve(&self, press: &KeyPress) -> TransientResolve {
        for group in &self.groups {
            for entry in &group.entries {
                if entry.key == *press {
                    return TransientResolve::Action(entry.action);
                }
            }
        }
        TransientResolve::Unbound
    }

    pub fn flat_entries(&self) -> impl Iterator<Item = (String, &TransientEntry)> + '_ {
        self.groups.iter().flat_map(|group| {
            group
                .entries
                .iter()
                .map(|entry| (render_chord(std::slice::from_ref(&entry.key)), entry))
        })
    }
}

trait DefaultKeyLookup {
    fn pipe_default_key(self, kind: TransientKind) -> Option<KeyPress>;
}

impl DefaultKeyLookup for &str {
    fn pipe_default_key(self, kind: TransientKind) -> Option<KeyPress> {
        for (_, entries) in kind.default_groups() {
            for (_, chord, action, _) in *entries {
                if action.name() == self {
                    return single_press(chord);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(chord: &str) -> KeyPress {
        single_press(chord).expect("single press")
    }

    fn transient(kind: TransientKind) -> Transient {
        let (transient, warnings) = Transient::build(kind, &KeysConfig::default());
        assert!(
            warnings.is_empty(),
            "default transient warned: {warnings:?}"
        );
        transient
    }

    #[test]
    fn commit_transient_resolves_its_leaves() {
        let commit = transient(TransientKind::Commit);
        assert_eq!(
            commit.resolve(&press("c")),
            TransientResolve::Action(Action::CommitFlow)
        );
        assert_eq!(
            commit.resolve(&press("a")),
            TransientResolve::Action(Action::CommitAmend)
        );
        assert_eq!(
            commit.resolve(&press("e")),
            TransientResolve::Action(Action::CommitExtend)
        );
        assert_eq!(
            commit.resolve(&press("w")),
            TransientResolve::Action(Action::CommitReword)
        );
        assert_eq!(commit.resolve(&press("z")), TransientResolve::Unbound);
    }

    #[test]
    fn branch_transient_resolves_its_leaves() {
        let branch = transient(TransientKind::Branch);
        assert_eq!(
            branch.resolve(&press("b")),
            TransientResolve::Action(Action::BranchCheckout)
        );
        assert_eq!(
            branch.resolve(&press("c")),
            TransientResolve::Action(Action::BranchCreateCheckout)
        );
        assert_eq!(
            branch.resolve(&press("n")),
            TransientResolve::Action(Action::BranchCreate)
        );
        assert_eq!(
            branch.resolve(&press("D")),
            TransientResolve::Action(Action::BranchDelete)
        );
    }

    #[test]
    fn diff_transient_resolves_its_leaves() {
        let diff = transient(TransientKind::Diff);
        assert_eq!(
            diff.resolve(&press("d")),
            TransientResolve::Action(Action::DiffBase)
        );
        assert_eq!(
            diff.resolve(&press("c")),
            TransientResolve::Action(Action::DiffLastCommit)
        );
        assert_eq!(
            diff.resolve(&press("b")),
            TransientResolve::Action(Action::DiffBranch)
        );
        assert_eq!(
            diff.resolve(&press("s")),
            TransientResolve::Action(Action::DiffCommit)
        );
        assert_eq!(
            diff.resolve(&press("w")),
            TransientResolve::Action(Action::DiffWorkingTree)
        );
    }

    #[test]
    fn log_transient_resolves_to_the_log_view() {
        let log = transient(TransientKind::Log);
        assert_eq!(
            log.resolve(&press("l")),
            TransientResolve::Action(Action::LogView)
        );
    }

    #[test]
    fn push_transient_resolves_its_leaves() {
        let push = transient(TransientKind::Push);
        assert_eq!(
            push.resolve(&press("p")),
            TransientResolve::Action(Action::Push)
        );
        assert_eq!(
            push.resolve(&press("u")),
            TransientResolve::Action(Action::PushSetUpstream)
        );
        assert_eq!(push.resolve(&press("z")), TransientResolve::Unbound);
    }

    #[test]
    fn pull_transient_resolves_its_leaf() {
        let pull = transient(TransientKind::Pull);
        assert_eq!(
            pull.resolve(&press("p")),
            TransientResolve::Action(Action::Pull)
        );
    }

    #[test]
    fn fetch_transient_resolves_its_leaves() {
        let fetch = transient(TransientKind::Fetch);
        assert_eq!(
            fetch.resolve(&press("f")),
            TransientResolve::Action(Action::Fetch)
        );
        assert_eq!(
            fetch.resolve(&press("a")),
            TransientResolve::Action(Action::FetchAll)
        );
    }

    #[test]
    fn stash_transient_resolves_its_leaves() {
        let stash = transient(TransientKind::Stash);
        assert_eq!(
            stash.resolve(&press("z")),
            TransientResolve::Action(Action::StashPush)
        );
        assert_eq!(
            stash.resolve(&press("p")),
            TransientResolve::Action(Action::StashPop)
        );
        assert_eq!(stash.resolve(&press("x")), TransientResolve::Unbound);
    }

    #[test]
    fn defaults_are_conflict_free_per_transient() {
        for kind in TransientKind::ALL {
            let (_, warnings) = Transient::build(kind, &KeysConfig::default());
            assert!(
                warnings.is_empty(),
                "{kind:?} defaults warned: {warnings:?}"
            );
        }
    }

    #[test]
    fn override_remaps_a_sub_key() {
        let mut keys = KeysConfig::default();
        keys.commit.insert("amend".to_owned(), "m".to_owned());
        let (commit, warnings) = Transient::build(TransientKind::Commit, &keys);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            commit.resolve(&press("m")),
            TransientResolve::Action(Action::CommitAmend)
        );
        assert_eq!(commit.resolve(&press("a")), TransientResolve::Unbound);
    }

    #[test]
    fn clashing_override_warns_and_falls_back_to_the_default() {
        let mut keys = KeysConfig::default();
        keys.commit.insert("amend".to_owned(), "c".to_owned());
        let (commit, warnings) = Transient::build(TransientKind::Commit, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("amend"), "{warnings:?}");
        assert!(warnings[0].contains("[keys.commit]"), "{warnings:?}");
        assert_eq!(
            commit.resolve(&press("c")),
            TransientResolve::Action(Action::CommitFlow)
        );
        assert_eq!(
            commit.resolve(&press("a")),
            TransientResolve::Action(Action::CommitAmend)
        );
    }

    #[test]
    fn flat_entries_lists_every_leaf() {
        let commit = transient(TransientKind::Commit);
        let labels: Vec<&str> = commit
            .flat_entries()
            .map(|(_, entry)| entry.label)
            .collect();
        assert_eq!(labels, vec!["Commit", "Extend", "Amend", "Reword"]);
    }
}
