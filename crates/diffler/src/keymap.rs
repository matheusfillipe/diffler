//! Key bindings: neogit-style defaults per screen, overridable from
//! `[keys.*]` config sections, with multi-key sequence resolution.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Chord, KeyPress, KeysConfig, parse_chord};

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
    Stage,
    Unstage,
    StageAll,
    UnstageAll,
    Discard,
    Refresh,
    Open,
    OpenReviewDiff,
    CommitFlow,
    BranchPopup,
    LogView,
    NextHunk,
    PrevHunk,
    HalfPageDown,
    HalfPageUp,
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
            Self::Stage => "stage",
            Self::Unstage => "unstage",
            Self::StageAll => "stage_all",
            Self::UnstageAll => "unstage_all",
            Self::Discard => "discard",
            Self::Refresh => "refresh",
            Self::Open => "open",
            Self::OpenReviewDiff => "open_review_diff",
            Self::CommitFlow => "commit_flow",
            Self::BranchPopup => "branch_popup",
            Self::LogView => "log_view",
            Self::NextHunk => "next_hunk",
            Self::PrevHunk => "prev_hunk",
            Self::HalfPageDown => "half_page_down",
            Self::HalfPageUp => "half_page_up",
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

    const ALL: [Self; 37] = [
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
        Self::Stage,
        Self::Unstage,
        Self::StageAll,
        Self::UnstageAll,
        Self::Discard,
        Self::Refresh,
        Self::Open,
        Self::OpenReviewDiff,
        Self::CommitFlow,
        Self::BranchPopup,
        Self::LogView,
        Self::NextHunk,
        Self::PrevHunk,
        Self::HalfPageDown,
        Self::HalfPageUp,
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

/// Which screen's bindings apply (research/NEOGIT-UX.md mapping decisions).
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
    /// The sequence so far is a prefix of some chord; the caller keeps the
    /// pending buffer and applies a timeout.
    Pending,
    Unbound,
}

#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<(Chord, Action)>,
}

const STATUS_DEFAULTS: &[(&str, Action)] = &[
    ("j", Action::MoveDown),
    ("k", Action::MoveUp),
    ("gg", Action::GoTop),
    ("G", Action::GoBottom),
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
    ("cc", Action::CommitFlow),
    ("b", Action::BranchPopup),
    ("ll", Action::LogView),
    ("{", Action::PrevHunk),
    ("}", Action::NextHunk),
    ("e", Action::OpenEditor),
    ("Z", Action::SendFeedback),
    ("?", Action::Help),
    ("q", Action::Back),
];

const DIFF_DEFAULTS: &[(&str, Action)] = &[
    ("j", Action::MoveDown),
    ("k", Action::MoveUp),
    ("gg", Action::GoTop),
    ("G", Action::GoBottom),
    ("<c-d>", Action::HalfPageDown),
    ("<c-u>", Action::HalfPageUp),
    ("<c-n>", Action::NextFile),
    ("<c-p>", Action::PrevFile),
    ("<tab>", Action::ToggleFocus),
    ("<cr>", Action::Open),
    ("<c-r>", Action::Refresh),
    ("{", Action::PrevHunk),
    ("}", Action::NextHunk),
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
        let (defaults, overrides, section) = match context {
            Context::Status => (STATUS_DEFAULTS, &keys.status, "status"),
            Context::Diff => (DIFF_DEFAULTS, &keys.diff, "diff"),
            Context::Log => (LOG_DEFAULTS, &keys.log, "log"),
        };
        let mut keymap = Self {
            // defaults are static strings validated by tests; a default that
            // failed to parse would silently vanish, which the
            // all_defaults_parse test guards against
            bindings: defaults
                .iter()
                .filter_map(|(chord, action)| Some((parse_chord(chord).ok()?, *action)))
                .collect(),
        };
        let warnings = keymap.apply_overrides(overrides, section);
        (keymap, warnings)
    }

    fn apply_overrides(
        &mut self,
        overrides: &BTreeMap<String, String>,
        section: &str,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for (name, chord_str) in overrides {
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
        warnings
    }

    /// Feed one key press, accumulating multi-key sequences in `pending`.
    /// An exact match fires (and clears `pending`); a prefix stays pending;
    /// a dead sequence is dropped, retrying the new key on its own so e.g.
    /// `c` then `j` still moves down.
    pub fn resolve(&self, pending: &mut Vec<KeyPress>, press: KeyPress) -> Resolved {
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
        assert_eq!(keymap.chord_for(Action::CommitFlow).as_deref(), Some("cc"));
        assert_eq!(keymap.chord_for(Action::Comment), None, "unbound in status");

        let mut keys = KeysConfig::default();
        keys.status.insert("stage".to_owned(), "<c-s>".to_owned());
        let (keymap, _) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(keymap.chord_for(Action::Stage).as_deref(), Some("<c-s>"));
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
        // `c` becomes a strict prefix of the default `cc` commit chord
        keys.status.insert("stage".to_owned(), "c".to_owned());
        let (keymap, warnings) = Keymap::for_context(Context::Status, &keys);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("binding c for stage shadows chord cc (commit_flow)"),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("[keys.status]"), "{warnings:?}");
        // behavior is unchanged: the short binding still fires
        let mut pending = Vec::new();
        assert_eq!(
            keymap.resolve(&mut pending, press("c")),
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
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("c")), Resolved::Pending);
        assert_eq!(pending.len(), 1);
        assert_eq!(
            keymap.resolve(&mut pending, press("c")),
            Resolved::Action(Action::CommitFlow)
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn dangling_prefix_stays_pending() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("l")), Resolved::Pending);
        assert_eq!(pending.len(), 1);
        assert_eq!(
            keymap.resolve(&mut pending, press("l")),
            Resolved::Action(Action::LogView)
        );
    }

    #[test]
    fn unknown_key_clears_pending() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("c")), Resolved::Pending);
        assert_eq!(keymap.resolve(&mut pending, press("z")), Resolved::Unbound);
        assert!(pending.is_empty());
    }

    #[test]
    fn dead_sequence_retries_new_key_alone() {
        let keymap = keymap(Context::Status);
        let mut pending = Vec::new();
        assert_eq!(keymap.resolve(&mut pending, press("c")), Resolved::Pending);
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
}
