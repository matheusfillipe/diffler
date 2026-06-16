//! Key bindings: neogit-style defaults per screen, overridable from
//! `[keys.*]` config sections, with multi-key sequence resolution.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Chord, KeyPress, KeysConfig, parse_chord};
use crate::transient::TransientKind;

/// Everything a key can do. Defined as the full superset so config action
/// names stay stable across screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveDown,
    MoveUp,
    GoTop,
    GoBottom,
    NextSection,
    PrevSection,
    NextFile,
    PrevFile,
    ToggleFocus,
    ToggleFold,
    ToggleSideBySide,
    Stage,
    Unstage,
    StageAll,
    UnstageAll,
    Discard,
    Refresh,
    Open,
    OpenReviewDiff,
    CommitFlow,
    CommitExtend,
    CommitAmend,
    CommitReword,
    BranchCheckout,
    BranchCreateCheckout,
    BranchCreate,
    BranchDelete,
    LogView,
    Push,
    PushSetUpstream,
    Pull,
    Fetch,
    FetchAll,
    StashPush,
    StashPop,
    NextHunk,
    PrevHunk,
    NextComment,
    PrevComment,
    HalfPageDown,
    HalfPageUp,
    FullPageDown,
    FullPageUp,
    Comment,
    VisualSelect,
    Reply,
    Resolve,
    MarkViewed,
    CopyFileFeedback,
    CopyAllFeedback,
    OpenEditor,
    SendFeedback,
    Help,
    Quit,
    Back,
}

impl Action {
    /// Config-facing identifier: `snake_case` of the variant name.
    pub fn name(self) -> &'static str {
        match self {
            Self::MoveDown => "move_down",
            Self::MoveUp => "move_up",
            Self::GoTop => "go_top",
            Self::GoBottom => "go_bottom",
            Self::NextSection => "next_section",
            Self::PrevSection => "prev_section",
            Self::NextFile => "next_file",
            Self::PrevFile => "prev_file",
            Self::ToggleFocus => "toggle_focus",
            Self::ToggleFold => "toggle_fold",
            Self::ToggleSideBySide => "toggle_side_by_side",
            Self::Stage => "stage",
            Self::Unstage => "unstage",
            Self::StageAll => "stage_all",
            Self::UnstageAll => "unstage_all",
            Self::Discard => "discard",
            Self::Refresh => "refresh",
            Self::Open => "open",
            Self::OpenReviewDiff => "open_review_diff",
            Self::CommitFlow => "commit_flow",
            Self::CommitExtend => "commit_extend",
            Self::CommitAmend => "commit_amend",
            Self::CommitReword => "commit_reword",
            Self::BranchCheckout => "branch_checkout",
            Self::BranchCreateCheckout => "branch_create_checkout",
            Self::BranchCreate => "branch_create",
            Self::BranchDelete => "branch_delete",
            Self::LogView => "log_view",
            Self::Push => "push",
            Self::PushSetUpstream => "push_set_upstream",
            Self::Pull => "pull",
            Self::Fetch => "fetch",
            Self::FetchAll => "fetch_all",
            Self::StashPush => "stash_push",
            Self::StashPop => "stash_pop",
            Self::NextHunk => "next_hunk",
            Self::PrevHunk => "prev_hunk",
            Self::NextComment => "next_comment",
            Self::PrevComment => "prev_comment",
            Self::HalfPageDown => "half_page_down",
            Self::HalfPageUp => "half_page_up",
            Self::FullPageDown => "full_page_down",
            Self::FullPageUp => "full_page_up",
            Self::Comment => "comment",
            Self::VisualSelect => "visual_select",
            Self::Reply => "reply",
            Self::Resolve => "resolve",
            Self::MarkViewed => "mark_viewed",
            Self::CopyFileFeedback => "copy_file_feedback",
            Self::CopyAllFeedback => "copy_all_feedback",
            Self::OpenEditor => "open_editor",
            Self::SendFeedback => "send_feedback",
            Self::Help => "help",
            Self::Quit => "quit",
            Self::Back => "back",
        }
    }

    const ALL: [Self; 55] = [
        Self::MoveDown,
        Self::MoveUp,
        Self::GoTop,
        Self::GoBottom,
        Self::NextSection,
        Self::PrevSection,
        Self::NextFile,
        Self::PrevFile,
        Self::ToggleFocus,
        Self::ToggleFold,
        Self::ToggleSideBySide,
        Self::Stage,
        Self::Unstage,
        Self::StageAll,
        Self::UnstageAll,
        Self::Discard,
        Self::Refresh,
        Self::Open,
        Self::OpenReviewDiff,
        Self::CommitFlow,
        Self::CommitExtend,
        Self::CommitAmend,
        Self::CommitReword,
        Self::BranchCheckout,
        Self::BranchCreateCheckout,
        Self::BranchCreate,
        Self::BranchDelete,
        Self::LogView,
        Self::Push,
        Self::PushSetUpstream,
        Self::Pull,
        Self::Fetch,
        Self::FetchAll,
        Self::StashPush,
        Self::StashPop,
        Self::NextHunk,
        Self::PrevHunk,
        Self::NextComment,
        Self::PrevComment,
        Self::HalfPageDown,
        Self::HalfPageUp,
        Self::FullPageDown,
        Self::FullPageUp,
        Self::Comment,
        Self::VisualSelect,
        Self::Reply,
        Self::Resolve,
        Self::MarkViewed,
        Self::CopyFileFeedback,
        Self::CopyAllFeedback,
        Self::OpenEditor,
        Self::SendFeedback,
        Self::Help,
        Self::Quit,
        Self::Back,
    ];

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// Which screen's keymap is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Status,
    Diff,
    Log,
}

/// Outcome of feeding one key press into a keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    Action(Action),
    /// A top-level prefix key: open the named transient and resolve the next
    /// key against it.
    Transient(TransientKind),
    /// The sequence so far is a prefix of some chord; the caller keeps the
    /// pending buffer and applies a timeout.
    Pending,
    Unbound,
}

#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<(Chord, Action)>,
    /// Single-key prefixes that open transients (status context only). A
    /// prefix key is never also a leaf in the same context.
    prefixes: Vec<(KeyPress, TransientKind)>,
}

const STATUS_DEFAULTS: &[(&str, Action)] = &[
    ("j", Action::MoveDown),
    ("k", Action::MoveUp),
    ("gg", Action::GoTop),
    ("G", Action::GoBottom),
    ("<c-d>", Action::HalfPageDown),
    ("<c-u>", Action::HalfPageUp),
    ("v", Action::MarkViewed),
    ("<c-n>", Action::NextSection),
    ("<c-p>", Action::PrevSection),
    ("<tab>", Action::ToggleFold),
    ("s", Action::Stage),
    ("u", Action::Unstage),
    ("S", Action::StageAll),
    ("U", Action::UnstageAll),
    ("x", Action::Discard),
    ("<c-r>", Action::Refresh),
    ("<cr>", Action::Open),
    ("D", Action::OpenReviewDiff),
    ("{", Action::PrevHunk),
    ("}", Action::NextHunk),
    ("e", Action::OpenEditor),
    ("Z", Action::SendFeedback),
    ("?", Action::Help),
    ("q", Action::Back),
];

/// Status-context prefix keys: each opens a transient. The config name is the
/// transient's `name()`, so `[keys.status] commit = "x"` rebinds the prefix.
const STATUS_PREFIXES: &[(&str, TransientKind)] = &[
    ("c", TransientKind::Commit),
    ("b", TransientKind::Branch),
    ("l", TransientKind::Log),
    ("P", TransientKind::Push),
    ("p", TransientKind::Pull),
    ("f", TransientKind::Fetch),
    ("z", TransientKind::Stash),
];

/// Contexts with no transients (diff, log) bind no prefixes.
const NO_PREFIXES: &[(&str, TransientKind)] = &[];

const DIFF_DEFAULTS: &[(&str, Action)] = &[
    ("j", Action::MoveDown),
    ("k", Action::MoveUp),
    ("gg", Action::GoTop),
    ("G", Action::GoBottom),
    ("<c-d>", Action::HalfPageDown),
    ("<c-u>", Action::HalfPageUp),
    ("<c-f>", Action::FullPageDown),
    ("<c-b>", Action::FullPageUp),
    ("<c-n>", Action::NextFile),
    ("<c-p>", Action::PrevFile),
    ("<tab>", Action::ToggleFocus),
    ("za", Action::ToggleFold),
    ("|", Action::ToggleSideBySide),
    ("<cr>", Action::Open),
    ("<c-r>", Action::Refresh),
    ("{", Action::PrevHunk),
    ("}", Action::NextHunk),
    ("[", Action::PrevComment),
    ("]", Action::NextComment),
    ("c", Action::Comment),
    ("V", Action::VisualSelect),
    ("r", Action::Reply),
    ("R", Action::Resolve),
    ("v", Action::MarkViewed),
    ("y", Action::CopyFileFeedback),
    ("Y", Action::CopyAllFeedback),
    ("e", Action::OpenEditor),
    ("Z", Action::SendFeedback),
    ("?", Action::Help),
    ("q", Action::Back),
];

const LOG_DEFAULTS: &[(&str, Action)] = &[
    ("j", Action::MoveDown),
    ("k", Action::MoveUp),
    ("gg", Action::GoTop),
    ("G", Action::GoBottom),
    ("<c-d>", Action::HalfPageDown),
    ("<c-u>", Action::HalfPageUp),
    ("<c-f>", Action::FullPageDown),
    ("<c-b>", Action::FullPageUp),
    ("V", Action::VisualSelect),
    ("<cr>", Action::Open),
    ("<c-r>", Action::Refresh),
    ("?", Action::Help),
    ("q", Action::Back),
];

impl Keymap {
    /// Build the keymap for one screen: built-in defaults, then config
    /// overrides (action name → chord). Returns user-facing warnings for
    /// entries that cannot apply.
    pub fn for_context(context: Context, keys: &KeysConfig) -> (Self, Vec<String>) {
        let (defaults, prefixes, overrides, section) = match context {
            Context::Status => (STATUS_DEFAULTS, STATUS_PREFIXES, &keys.status, "status"),
            Context::Diff => (DIFF_DEFAULTS, NO_PREFIXES, &keys.diff, "diff"),
            Context::Log => (LOG_DEFAULTS, NO_PREFIXES, &keys.log, "log"),
        };
        let mut keymap = Self {
            // defaults are static strings validated by tests; a default that
            // failed to parse would silently vanish, which the
            // all_defaults_parse test guards against
            bindings: defaults
                .iter()
                .filter_map(|(chord, action)| Some((parse_chord(chord).ok()?, *action)))
                .collect(),
            prefixes: prefixes
                .iter()
                .filter_map(|(chord, kind)| Some((single_press(chord)?, *kind)))
                .collect(),
        };
        let mut warnings = keymap.apply_overrides(overrides, section, defaults);
        warnings.extend(keymap.apply_prefix_overrides(overrides, section));
        warnings.extend(keymap.enforce_leaf_prefix(section));
        (keymap, warnings)
    }

    fn apply_overrides(
        &mut self,
        overrides: &BTreeMap<String, String>,
        section: &str,
        defaults: &[(&str, Action)],
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for (name, chord_str) in overrides {
            // prefix names (commit/branch/log_menu) are handled separately by
            // apply_prefix_overrides; don't flag them as unknown actions here
            if self.prefixes.iter().any(|(_, kind)| kind.name() == name) {
                continue;
            }
            let Some(action) = Action::from_name(name) else {
                warnings.push(format!("unknown action `{name}` in [keys.{section}]"));
                continue;
            };
            // config loading already validated chords; a parse failure here
            // means the entry was injected programmatically, so warn the same way
            let chord = match parse_chord(chord_str) {
                Ok(chord) => chord,
                Err(err) => {
                    warnings.push(format!("invalid chord for keys.{section}.{name}: {err}"));
                    continue;
                }
            };
            // a remap frees both the action's old key and the key's old action
            self.bindings.retain(|(c, a)| *a != action && *c != chord);
            self.bindings.push((chord, action));
        }
        // an exact match fires immediately, so a binding that is a strict
        // prefix of another chord makes the longer one unreachable
        for (chord, action) in &self.bindings {
            for (other, other_action) in &self.bindings {
                if other.len() > chord.len() && other.starts_with(chord) {
                    warnings.push(format!(
                        "binding {} for {} shadows chord {} ({}) in [keys.{section}]",
                        render_chord(chord),
                        action.name(),
                        render_chord(other),
                        other_action.name(),
                    ));
                }
            }
        }
        // a transient prefix fires on its single key before any chord starting
        // with that key can accumulate — a multi-key chord whose first key is a
        // live prefix is therefore unreachable
        let new_bindings_that_clash: Vec<(Chord, Action)> = self
            .bindings
            .iter()
            .filter(|(chord, _)| {
                chord.len() > 1
                    && chord.first().is_some_and(|first| {
                        self.prefixes
                            .iter()
                            .any(|(prefix_key, _)| first == prefix_key)
                    })
            })
            .cloned()
            .collect();
        for (chord, action) in new_bindings_that_clash {
            let prefix_kind = chord.first().and_then(|first| {
                self.prefixes
                    .iter()
                    .find(|(k, _)| k == first)
                    .map(|(_, kind)| *kind)
            });
            if let Some(kind) = prefix_kind {
                warnings.push(format!(
                    "binding {} for {} is shadowed by {} prefix in [keys.{section}]; using default",
                    render_chord(&chord),
                    action.name(),
                    kind.name(),
                ));
                // restore the default binding for this action and drop the
                // conflicting override
                self.bindings.retain(|(c, _)| *c != chord);
                if let Some(def_chord) = defaults
                    .iter()
                    .find(|(_, a)| *a == action)
                    .and_then(|(s, _)| parse_chord(s).ok())
                {
                    // only restore if the default key is still free
                    if !self.bindings.iter().any(|(c, _)| *c == def_chord) {
                        self.bindings.push((def_chord, action));
                    }
                }
            }
        }
        warnings
    }

    /// Apply `[keys.<section>]` overrides that rebind a transient prefix key
    /// (keyed by the transient name, e.g. `commit = "x"`). A prefix override
    /// must be a single key; remapping it frees both the prefix's old key and
    /// the key's old owner.
    fn apply_prefix_overrides(
        &mut self,
        overrides: &BTreeMap<String, String>,
        section: &str,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for (key, kind) in &mut self.prefixes {
            let Some(chord_str) = overrides.get(kind.name()) else {
                continue;
            };
            match single_press(chord_str) {
                // a duplicate prefix key (two prefixes on one chord) is caught
                // by enforce_leaf_prefix, which drops the later one
                Some(new_key) => *key = new_key,
                None => warnings.push(format!(
                    "[keys.{section}] {}: prefix chord {chord_str:?} must be a single key; using default",
                    kind.name()
                )),
            }
        }
        warnings
    }

    /// Enforce the level invariant: no key is both a leaf and a prefix, and no
    /// two prefixes share a key. Drop the conflicting prefix and warn (the
    /// HARD config requirement; defaults are conflict-free by construction).
    ///
    /// Also covers the multi-key case: a prefix whose key equals the first
    /// press of a multi-key chord makes that chord unreachable (the transient
    /// fires on the first press before the second can accumulate). Treat this
    /// the same as a leaf clash: drop the offending prefix and warn.
    fn enforce_leaf_prefix(&mut self, section: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        let leaf_singletons: Vec<(KeyPress, Action)> = self
            .bindings
            .iter()
            .filter_map(|(chord, action)| match chord.as_slice() {
                [key] => Some((key.clone(), *action)),
                _ => None,
            })
            .collect();
        // first key of every multi-key chord — a prefix on that key swallows
        // the press before the chord can accumulate
        let chord_firsts: Vec<(KeyPress, Action)> = self
            .bindings
            .iter()
            .filter_map(|(chord, action)| {
                if chord.len() > 1 {
                    chord.first().map(|k| (k.clone(), *action))
                } else {
                    None
                }
            })
            .collect();
        let mut kept: Vec<(KeyPress, TransientKind)> = Vec::new();
        for (key, kind) in std::mem::take(&mut self.prefixes) {
            if let Some((_, action)) = leaf_singletons.iter().find(|(k, _)| *k == key) {
                warnings.push(format!(
                    "[keys.{section}] {} prefix on {} clashes with leaf {}; prefix dropped",
                    kind.name(),
                    render_chord(std::slice::from_ref(&key)),
                    action.name(),
                ));
                continue;
            }
            // a prefix key that is also the first key of a multi-key chord
            // shadows the chord: the transient fires before the chord completes
            if let Some((_, action)) = chord_firsts.iter().find(|(k, _)| *k == key) {
                let full_chord = self
                    .bindings
                    .iter()
                    .find(|(_, a)| *a == *action)
                    .map_or(String::new(), |(c, _)| render_chord(c));
                warnings.push(format!(
                    "[keys.{section}] {} prefix on {} shadows chord {} ({}); prefix dropped",
                    kind.name(),
                    render_chord(std::slice::from_ref(&key)),
                    full_chord,
                    action.name(),
                ));
                continue;
            }
            if let Some((_, other)) = kept.iter().find(|(k, _)| *k == key) {
                warnings.push(format!(
                    "[keys.{section}] {} prefix on {} clashes with {} prefix; dropped",
                    kind.name(),
                    render_chord(std::slice::from_ref(&key)),
                    other.name(),
                ));
                continue;
            }
            kept.push((key, kind));
        }
        self.prefixes = kept;
        warnings
    }

    /// The transient kind a single press opens, if any.
    pub fn prefix_for(&self, press: &KeyPress) -> Option<TransientKind> {
        self.prefixes
            .iter()
            .find(|(key, _)| key == press)
            .map(|(_, kind)| *kind)
    }

    /// The key bound to a transient prefix, rendered in config syntax, for
    /// the prefix-only hint line and help popup.
    pub fn prefix_chord(&self, kind: TransientKind) -> Option<String> {
        self.prefixes
            .iter()
            .find(|(_, k)| *k == kind)
            .map(|(key, _)| render_chord(std::slice::from_ref(key)))
    }

    /// Feed one key press, accumulating multi-key sequences in `pending`.
    /// A prefix key opens its transient; an exact leaf match fires (and clears
    /// `pending`); a prefix stays pending; a dead sequence is dropped,
    /// retrying the new key on its own so e.g. `c` then `j` still moves down.
    pub fn resolve(&self, pending: &mut Vec<KeyPress>, press: KeyPress) -> Resolved {
        // a transient prefix is single-key and only fires from a clean buffer
        if pending.is_empty()
            && let Some(kind) = self.prefix_for(&press)
        {
            return Resolved::Transient(kind);
        }
        pending.push(press.clone());
        match self.lookup(pending) {
            Lookup::Exact(action) => {
                pending.clear();
                Resolved::Action(action)
            }
            Lookup::Prefix => Resolved::Pending,
            Lookup::None if pending.len() > 1 => {
                pending.clear();
                match self.lookup(std::slice::from_ref(&press)) {
                    Lookup::Exact(action) => Resolved::Action(action),
                    Lookup::Prefix => {
                        pending.push(press);
                        Resolved::Pending
                    }
                    Lookup::None => Resolved::Unbound,
                }
            }
            Lookup::None => {
                pending.clear();
                Resolved::Unbound
            }
        }
    }

    /// All `(chord, action)` pairs in binding order, defaults first then
    /// config overrides — what the help popup lists.
    pub fn bindings(&self) -> &[(Chord, Action)] {
        &self.bindings
    }

    /// The chord bound to `action`, rendered in config syntax, so hints and
    /// help reflect remaps. `None` when a remap stole the action's key.
    pub fn chord_for(&self, action: Action) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(chord, _)| render_chord(chord))
    }

    fn lookup(&self, seq: &[KeyPress]) -> Lookup {
        if let Some((_, action)) = self.bindings.iter().find(|(chord, _)| chord == seq) {
            return Lookup::Exact(*action);
        }
        let is_prefix = self
            .bindings
            .iter()
            .any(|(chord, _)| chord.len() > seq.len() && chord.starts_with(seq));
        if is_prefix {
            Lookup::Prefix
        } else {
            Lookup::None
        }
    }
}

enum Lookup {
    Exact(Action),
    Prefix,
    None,
}

/// Parse a chord that must be exactly one key press; `None` otherwise. Prefix
/// and transient keys are single-press by design.
fn single_press(chord: &str) -> Option<KeyPress> {
    let mut presses = parse_chord(chord).ok()?;
    if presses.len() == 1 {
        Some(presses.remove(0))
    } else {
        None
    }
}

/// Render a chord back to the `parse_chord` syntax for warnings, hints,
/// and the help popup.
pub fn render_chord(chord: &[KeyPress]) -> String {
    chord.iter().map(render_press).collect()
}

fn render_press(press: &KeyPress) -> String {
    let key = match press.code {
        KeyCode::Char(' ') => "space".to_owned(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "cr".to_owned(),
        KeyCode::Tab => "tab".to_owned(),
        KeyCode::Esc => "esc".to_owned(),
        other => format!("{other:?}").to_ascii_lowercase(),
    };
    let mut mods = String::new();
    if press.ctrl {
        mods.push_str("c-");
    }
    if press.alt {
        mods.push_str("a-");
    }
    // letters carry shift in their case; only non-letters need the prefix
    if press.shift && !matches!(press.code, KeyCode::Char(c) if c.is_alphabetic()) {
        mods.push_str("s-");
    }
    if mods.is_empty() && matches!(press.code, KeyCode::Char(c) if c != ' ') {
        key
    } else {
        format!("<{mods}{key}>")
    }
}

/// Normalize a crossterm key event into the chord-matching shape. Letters
/// carry shift via their case (matching `parse_chord`); shifted symbols like
/// `{` already encode shift in the character, so the modifier is dropped —
/// terminals disagree on whether they report it.
pub fn press_from_event(event: &KeyEvent) -> KeyPress {
    let mods = event.modifiers;
    let (code, shift) = match event.code {
        KeyCode::Char(c) if c.is_alphabetic() => (KeyCode::Char(c), c.is_uppercase()),
        KeyCode::Char(c) => (KeyCode::Char(c), false),
        code => (code, mods.contains(KeyModifiers::SHIFT)),
    };
    KeyPress {
        code,
        ctrl: mods.contains(KeyModifiers::CONTROL),
        alt: mods.contains(KeyModifiers::ALT),
        shift,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap(context: Context) -> Keymap {
        let (keymap, warnings) = Keymap::for_context(context, &KeysConfig::default());
        assert!(warnings.is_empty(), "default keymap warned: {warnings:?}");
        keymap
    }

    fn press(chord: &str) -> KeyPress {
        let mut presses = parse_chord(chord).expect("valid chord");
        assert_eq!(presses.len(), 1, "single press expected");
        presses.remove(0)
    }

    #[test]
    fn all_defaults_parse() {
        for (defaults, context) in [
            (STATUS_DEFAULTS, Context::Status),
            (DIFF_DEFAULTS, Context::Diff),
            (LOG_DEFAULTS, Context::Log),
        ] {
            let (keymap, _) = Keymap::for_context(context, &KeysConfig::default());
            assert_eq!(keymap.bindings.len(), defaults.len(), "{context:?}");
        }
    }

    #[test]
    fn defaults_resolve() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("j")),
            Resolved::Action(Action::MoveDown)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("<c-r>")),
            Resolved::Action(Action::Refresh)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("q")),
            Resolved::Action(Action::Back)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("?")),
            Resolved::Action(Action::Help)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("D")),
            Resolved::Action(Action::OpenReviewDiff)
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn chord_for_renders_defaults_and_overrides() {
        let keymap = keymap(Context::Status);
        assert_eq!(
            keymap.chord_for(Action::ToggleFold).as_deref(),
            Some("<tab>")
        );
        // commit_flow is a transient leaf now, not a top-level leaf
        assert_eq!(keymap.chord_for(Action::CommitFlow), None);
        assert_eq!(keymap.chord_for(Action::Comment), None, "unbound in status");

        let mut keys = KeysConfig::default();
        keys.status.insert("stage".to_owned(), "<c-s>".to_owned());
        let (keymap, _) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(keymap.chord_for(Action::Stage).as_deref(), Some("<c-s>"));
    }

    #[test]
    fn status_prefixes_open_transients() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("c")),
            Resolved::Transient(TransientKind::Commit)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("b")),
            Resolved::Transient(TransientKind::Branch)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("l")),
            Resolved::Transient(TransientKind::Log)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("P")),
            Resolved::Transient(TransientKind::Push)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("p")),
            Resolved::Transient(TransientKind::Pull)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("f")),
            Resolved::Transient(TransientKind::Fetch)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("z")),
            Resolved::Transient(TransientKind::Stash)
        );
        assert!(pending.is_empty(), "a prefix never leaves a pending buffer");
        assert_eq!(
            keymap.prefix_chord(TransientKind::Commit).as_deref(),
            Some("c")
        );
        assert_eq!(
            keymap.prefix_chord(TransientKind::Branch).as_deref(),
            Some("b")
        );
        assert_eq!(
            keymap.prefix_chord(TransientKind::Push).as_deref(),
            Some("P")
        );
    }

    #[test]
    fn a_prefix_key_is_not_a_leaf() {
        let keymap = keymap(Context::Status);
        // no bare c/b/l chord is a leaf; they are prefixes only (ctrl combos
        // like <c-b> are distinct keys and may be leaves)
        assert!(keymap.bindings.iter().all(|(chord, _)| {
            !matches!(chord.as_slice(), [k] if matches!(k.code, KeyCode::Char('c' | 'b' | 'l')) && !k.ctrl && !k.alt)
        }));
    }

    #[test]
    fn prefix_override_remaps_the_transient_key() {
        let mut keys = KeysConfig::default();
        // remap the commit prefix to a free key (<c-c> owns no leaf in status)
        keys.status.insert("commit".to_owned(), "<c-c>".to_owned());
        let (keymap, warnings) = Keymap::for_context(Context::Status, &keys);
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("<c-c>")),
            Resolved::Transient(TransientKind::Commit)
        );
        // the old `c` no longer opens the commit transient
        assert_eq!(keymap.prefix_for(&press("c")), None);
    }

    #[test]
    fn a_prefix_clashing_with_a_leaf_is_dropped_with_a_warning() {
        let mut keys = KeysConfig::default();
        // aim the commit prefix at `s`, which the stage leaf owns
        keys.status.insert("commit".to_owned(), "s".to_owned());
        let (keymap, warnings) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("commit"), "{warnings:?}");
        assert!(warnings[0].contains("stage"), "{warnings:?}");
        // the leaf still fires; the prefix is gone
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("s")),
            Resolved::Action(Action::Stage)
        );
        assert_eq!(keymap.prefix_for(&press("s")), None);
    }

    #[test]
    fn diff_context_binds_review_keys() {
        let keymap = keymap(Context::Diff);
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("c")),
            Resolved::Action(Action::Comment)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("V")),
            Resolved::Action(Action::VisualSelect)
        );
        assert_eq!(
            keymap.resolve(&mut pending, press("Y")),
            Resolved::Action(Action::CopyAllFeedback)
        );
    }

    #[test]
    fn config_override_wins() {
        let mut keys = KeysConfig::default();
        keys.status.insert("refresh".to_owned(), "R".to_owned());
        let (keymap, warnings) = Keymap::for_context(Context::Status, &keys);
        assert!(warnings.is_empty());
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("R")),
            Resolved::Action(Action::Refresh)
        );
        // the old default no longer fires
        assert_eq!(
            keymap.resolve(&mut pending, press("<c-r>")),
            Resolved::Unbound
        );
    }

    #[test]
    fn override_steals_key_from_other_action() {
        let mut keys = KeysConfig::default();
        keys.status.insert("stage".to_owned(), "j".to_owned());
        let (keymap, _) = Keymap::for_context(Context::Status, &keys);
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("j")),
            Resolved::Action(Action::Stage)
        );
    }

    #[test]
    fn unknown_action_name_warns() {
        let mut keys = KeysConfig::default();
        keys.status.insert("warp_speed".to_owned(), "w".to_owned());
        let (_, warnings) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("warp_speed"));
        assert!(warnings[0].contains("keys.status"));
    }

    #[test]
    fn override_creating_a_prefix_collision_warns() {
        let mut keys = KeysConfig::default();
        // `g` becomes a strict prefix of the default `gg` go-top chord
        keys.status.insert("stage".to_owned(), "g".to_owned());
        let (keymap, warnings) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("binding g for stage shadows chord gg (go_top)"),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("[keys.status]"), "{warnings:?}");
        // behavior is unchanged: the short binding still fires
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("g")),
            Resolved::Action(Action::Stage)
        );
    }

    #[test]
    fn overriding_to_a_longer_chord_warns_about_the_shadowing_default() {
        let mut keys = KeysConfig::default();
        // diff binds `c` (comment) by default; a `cV` override can never fire
        keys.diff.insert("resolve".to_owned(), "cV".to_owned());
        let (_, warnings) = Keymap::for_context(Context::Diff, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("binding c for comment shadows chord cV (resolve)"),
            "{warnings:?}"
        );
    }

    #[test]
    fn non_colliding_overrides_stay_silent() {
        let mut keys = KeysConfig::default();
        keys.status.insert("stage".to_owned(), "<c-s>".to_owned());
        let (_, warnings) = Keymap::for_context(Context::Status, &keys);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn two_key_sequence_resolves() {
        // gg (go-top) is the surviving two-key chord in the status context
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("g")), Resolved::Pending);
        assert_eq!(pending.len(), 1);
        assert_eq!(
            keymap.resolve(&mut pending, press("g")),
            Resolved::Action(Action::GoTop)
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn dangling_prefix_stays_pending() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("g")), Resolved::Pending);
        assert_eq!(pending.len(), 1);
        assert_eq!(
            keymap.resolve(&mut pending, press("g")),
            Resolved::Action(Action::GoTop)
        );
    }

    #[test]
    fn unknown_key_clears_pending() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("g")), Resolved::Pending);
        assert_eq!(keymap.resolve(&mut pending, press("z")), Resolved::Unbound);
        assert!(pending.is_empty());
    }

    #[test]
    fn dead_sequence_retries_new_key_alone() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("g")), Resolved::Pending);
        assert_eq!(
            keymap.resolve(&mut pending, press("j")),
            Resolved::Action(Action::MoveDown)
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn shifted_symbol_event_matches_unshifted_chord() {
        // many terminals report `{` with the SHIFT modifier set
        let event = KeyEvent::new(KeyCode::Char('{'), KeyModifiers::SHIFT);
        assert_eq!(press_from_event(&event), press("{"));
    }

    #[test]
    fn uppercase_letter_event_carries_shift() {
        let event = KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::SHIFT);
        assert_eq!(press_from_event(&event), press("Z"));
    }

    #[test]
    fn ctrl_event_matches_ctrl_chord() {
        let event = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(press_from_event(&event), press("<c-r>"));
    }

    // --- conflict-enforcement gap: prefix vs first key of multi-key chord ---

    #[test]
    fn prefix_override_onto_first_key_of_chord_warns_and_falls_back() {
        // Moving the commit prefix to `g` would swallow the first press of `gg`
        // (GoTop), making it unreachable. The prefix must be dropped.
        let mut keys = KeysConfig::default();
        keys.status.insert("commit".to_owned(), "g".to_owned());
        let (keymap, warnings) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("commit"), "{warnings:?}");
        assert!(warnings[0].contains("gg"), "{warnings:?}");
        assert!(warnings[0].contains("go_top"), "{warnings:?}");
        // prefix dropped: `g` must not open any transient
        assert_eq!(keymap.prefix_for(&press("g")), None);
        // `gg` still resolves to GoTop because the prefix was dropped
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("g")), Resolved::Pending);
        assert_eq!(
            keymap.resolve(&mut pending, press("g")),
            Resolved::Action(Action::GoTop)
        );
    }

    #[test]
    fn leaf_override_onto_chord_starting_with_live_prefix_warns_and_falls_back() {
        // `cx` can never fire in the status context because `c` is the commit
        // prefix — the transient opens before the second key is seen. The
        // override must be rejected and the action fall back to its default.
        let mut keys = KeysConfig::default();
        keys.status.insert("discard".to_owned(), "cx".to_owned());
        let (keymap, warnings) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("cx"), "{warnings:?}");
        assert!(warnings[0].contains("discard"), "{warnings:?}");
        assert!(warnings[0].contains("commit"), "{warnings:?}");
        // discard fell back to its default `x`
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("x")),
            Resolved::Action(Action::Discard)
        );
        // the commit prefix is still intact
        assert_eq!(
            keymap.resolve(&mut pending, press("c")),
            Resolved::Transient(TransientKind::Commit)
        );
    }

    #[test]
    fn defaults_remain_conflict_free_under_stricter_check() {
        // All three default contexts must produce zero warnings under the new
        // checks. In particular: `g` (first key of `gg`) is not a transient
        // prefix by default, so `gg` must not false-positive.
        for context in [Context::Status, Context::Diff, Context::Log] {
            let (_, warnings) = Keymap::for_context(context, &KeysConfig::default());
            assert!(
                warnings.is_empty(),
                "default {context:?} keymap emitted unexpected warnings: {warnings:?}"
            );
        }
    }
}
