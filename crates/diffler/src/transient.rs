//! Magit/neogit-style transient menus: a top-level prefix key opens a
//! transient whose own keys are pure leaves (immediate actions). The model is
//! data, not closures, so resolution and the which-key panel layout stay pure
//! and unit-testable; the app owns the live transient state and timer.

use crate::config::{KeyPress, KeysConfig, parse_chord};
use crate::keymap::{Action, render_chord};

/// Which transient a top-level prefix opens. Commit and branch are multi-leaf;
/// log/push/pull/fetch are small transients so every git group is reached the
/// same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientKind {
    Commit,
    Branch,
    Log,
    Push,
    Pull,
    Fetch,
}

impl TransientKind {
    /// Config-facing section name, mirroring `Action::name`. Also the
    /// `[keys.<section>]` table that overrides this transient's sub-keys.
    pub fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Branch => "branch",
            Self::Log => "log_menu",
            Self::Push => "push",
            Self::Pull => "pull",
            Self::Fetch => "fetch",
        }
    }

    /// Human title shown atop the which-key panel and the help popup group.
    pub fn title(self) -> &'static str {
        match self {
            Self::Commit => "Commit",
            Self::Branch => "Branch",
            Self::Log => "Log",
            Self::Push => "Push",
            Self::Pull => "Pull",
            Self::Fetch => "Fetch",
        }
    }

    pub const ALL: [Self; 6] = [
        Self::Commit,
        Self::Branch,
        Self::Log,
        Self::Push,
        Self::Pull,
        Self::Fetch,
    ];
}

/// One entry inside a transient: a single key bound to an `Action`, with a
/// short label for the which-key panel and help popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientEntry {
    pub key: KeyPress,
    pub action: Action,
    pub label: &'static str,
}

/// A labelled column of entries within a transient (magit groups its keys
/// under headings like "Create" / "Commit").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientGroup {
    pub heading: &'static str,
    pub entries: Vec<TransientEntry>,
}

/// A fully-resolved transient: a title and its groups, ready to resolve keys
/// against and to render as a which-key panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transient {
    pub kind: TransientKind,
    pub groups: Vec<TransientGroup>,
}

/// Outcome of feeding one key into an open transient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientResolve {
    /// A leaf fired; the transient closes and the action dispatches.
    Action(Action),
    /// No entry owns the key; neogit-style, the caller closes and beeps.
    Unbound,
}

/// A built-in transient sub-key: `(config key, chord, action, label)`. The
/// config key addresses the override as `[keys.<section>] <key> = "<chord>"`.
type DefaultEntry = (&'static str, &'static str, Action, &'static str);

/// A built-in group: heading plus its default entries.
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

impl TransientKind {
    fn default_groups(self) -> &'static [DefaultGroup] {
        match self {
            Self::Commit => COMMIT_GROUPS,
            Self::Branch => BRANCH_GROUPS,
            Self::Log => LOG_GROUPS,
            Self::Push => PUSH_GROUPS,
            Self::Pull => PULL_GROUPS,
            Self::Fetch => FETCH_GROUPS,
        }
    }
}

impl Transient {
    /// Build a transient from its defaults, applying `[keys.<section>]`
    /// overrides keyed by the entry's config name. Returns warnings for
    /// unknown actions, bad chords, and within-transient conflicts (two
    /// entries on one chord), falling back to the default per conflict.
    pub fn build(kind: TransientKind, keys: &KeysConfig) -> (Self, Vec<String>) {
        let section = kind.name();
        let overrides = keys.transient(kind);
        let mut warnings = Vec::new();
        let mut groups = Vec::new();
        for (heading, entries) in kind.default_groups() {
            let mut built = Vec::new();
            for (config_key, default_chord, action, label) in *entries {
                // a default that failed to parse would silently vanish; the
                // defaults_are_conflict_free test guards against that
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

    /// Drop entries whose chord collides with an earlier entry in the same
    /// transient, restoring the loser to its default when that default is
    /// itself free; emit a warning per collision. Keeps every level
    /// internally unambiguous (the HARD config requirement).
    fn resolve_conflicts(&mut self, section: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut seen: Vec<KeyPress> = Vec::new();
        for group in &mut self.groups {
            for entry in &mut group.entries {
                if seen.contains(&entry.key) {
                    let clashing = render_chord(std::slice::from_ref(&entry.key));
                    // the override aimed this entry at a taken key; fall back
                    // to its default chord if that is still free
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

    /// Resolve one key against this transient's entries.
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

    /// `(key, label)` pairs across all groups, for the help popup listing.
    pub fn flat_entries(&self) -> impl Iterator<Item = (String, &'static str)> + '_ {
        self.groups.iter().flat_map(|group| {
            group
                .entries
                .iter()
                .map(|entry| (render_chord(std::slice::from_ref(&entry.key)), entry.label))
        })
    }
}

/// Look up the default key for `action` within `kind` so a clashing override
/// can fall back to it.
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

/// Parse a chord that must be exactly one key press; `None` otherwise.
fn single_press(chord: &str) -> Option<KeyPress> {
    let mut presses = parse_chord(chord).ok()?;
    if presses.len() == 1 {
        Some(presses.remove(0))
    } else {
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
        // the old `a` no longer fires amend
        assert_eq!(commit.resolve(&press("a")), TransientResolve::Unbound);
    }

    #[test]
    fn clashing_override_warns_and_falls_back_to_the_default() {
        let mut keys = KeysConfig::default();
        // aim amend at `c`, which the commit leaf already owns
        keys.commit.insert("amend".to_owned(), "c".to_owned());
        let (commit, warnings) = Transient::build(TransientKind::Commit, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("amend"), "{warnings:?}");
        assert!(warnings[0].contains("[keys.commit]"), "{warnings:?}");
        // commit still fires on `c`; amend fell back to its default `a`
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
        let labels: Vec<&str> = commit.flat_entries().map(|(_, label)| label).collect();
        assert_eq!(labels, vec!["Commit", "Extend", "Amend", "Reword"]);
    }
}
