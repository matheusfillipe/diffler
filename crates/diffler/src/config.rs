//! Layered TOML configuration: defaults → global file → project file → CLI.
//!
//! The global file lives at `$XDG_CONFIG_HOME/diffler/config.toml` (fallback
//! `~/.config/diffler/config.toml`) on every OS, macOS included — unix-first,
//! no platform dirs, so the config stays greppable and editable in one place.
//!
//! literal '<' is not bindable (no `<lt>` escape) — known v1 limit.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Merged effective configuration. Every field has a default so any layer
/// (including all of them) may be absent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub mcp: McpConfig,
    pub editor: EditorConfig,
    pub ci: CiConfig,
    pub keys: KeysConfig,
}

/// How a view lists files: a flat magit-style list (one row per file, full
/// repo-relative path) or a collapsible directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileLayout {
    List,
    Tree,
}

impl FileLayout {
    /// Parse a config string, falling back to `default` with a warning on an
    /// unknown value — the same lenient flow the theme key uses, so a typo
    /// never aborts startup.
    fn from_str(value: &str, key: &str, default: Self) -> (Self, Option<String>) {
        match value {
            "list" => (Self::List, None),
            "tree" => (Self::Tree, None),
            other => (
                default,
                Some(format!("unknown {key} \"{other}\", using \"{default}\"")),
            ),
        }
    }
}

impl fmt::Display for FileLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::List => "list",
            Self::Tree => "tree",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub context_lines: u32,
    pub recent_commits: usize,
    /// File-list layout on the status screen; flat magit list by default.
    pub status_file_layout: FileLayout,
    /// File-list layout in the diff sidebar; collapsible tree by default.
    pub diff_file_layout: FileLayout,
    /// Open the diff pane in side-by-side (old left, new right) mode; `|`
    /// toggles it live. Unified by default.
    pub side_by_side: bool,
    /// Emphasize only what changed *semantically* (AST diff): reindentation and
    /// block wrapping are not flagged. On by default; set false for the textual
    /// engine.
    pub semantic_diff: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "github-dark".to_owned(),
            context_lines: 3,
            recent_commits: 10,
            status_file_layout: FileLayout::List,
            diff_file_layout: FileLayout::Tree,
            side_by_side: false,
            semantic_diff: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub enabled: bool,
    pub port: u16,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8417,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    /// Editor command. `None` falls back to `$DIFFLER_EDITOR` then `$EDITOR`
    /// at the point of use, not at config load time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// CI monitoring: which provider to use for the repo's runs and how often to
/// re-poll. `provider = "auto"` detects from the remote/config files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiConfig {
    pub provider: String,
    pub poll_seconds: u64,
    pub gitlab: CiGitLabConfig,
    pub forgejo: CiForgejoConfig,
}

impl Default for CiConfig {
    fn default() -> Self {
        Self {
            provider: "auto".to_owned(),
            poll_seconds: 5,
            gitlab: CiGitLabConfig::default(),
            forgejo: CiForgejoConfig::default(),
        }
    }
}

/// GitLab-specific CI settings. `host` overrides remote detection for a
/// self-hosted instance.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CiGitLabConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Forgejo-specific CI settings. `host` overrides remote detection for a
/// self-hosted instance (without it, a forced provider targets codeberg.org).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CiForgejoConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Key remaps per screen and per transient: action (or sub-key) name → chord
/// string. Defaults are empty; built-in bindings live in the keymap and config
/// entries override them. The `commit`/`branch`/`log_menu` tables address the
/// transient sub-keys (e.g. `[keys.commit] amend = "m"`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    pub status: BTreeMap<String, String>,
    pub diff: BTreeMap<String, String>,
    pub log: BTreeMap<String, String>,
    pub logs: BTreeMap<String, String>,
    pub graph: BTreeMap<String, String>,
    pub prs: BTreeMap<String, String>,
    pub commit: BTreeMap<String, String>,
    pub branch: BTreeMap<String, String>,
    pub log_menu: BTreeMap<String, String>,
    pub push: BTreeMap<String, String>,
    pub pull: BTreeMap<String, String>,
    pub fetch: BTreeMap<String, String>,
    pub stash: BTreeMap<String, String>,
}

impl KeysConfig {
    /// Override table for a transient's sub-keys.
    pub fn transient(&self, kind: crate::transient::TransientKind) -> &BTreeMap<String, String> {
        match kind {
            crate::transient::TransientKind::Commit => &self.commit,
            crate::transient::TransientKind::Branch => &self.branch,
            crate::transient::TransientKind::Log => &self.log_menu,
            crate::transient::TransientKind::Push => &self.push,
            crate::transient::TransientKind::Pull => &self.pull,
            crate::transient::TransientKind::Fetch => &self.fetch,
            crate::transient::TransientKind::Stash => &self.stash,
        }
    }
}

/// CLI flags that override file layers. Every flag maps to a config key.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub port: Option<u16>,
    pub mcp_enabled: Option<bool>,
    pub theme: Option<String>,
}

/// Which layer last set a config key (dotted path, e.g. `ui.theme`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Default,
    Global(PathBuf),
    Project(PathBuf),
    Cli,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::Global(path) => write!(f, "global:{}", path.display()),
            Self::Project(path) => write!(f, "project:{}", path.display()),
            Self::Cli => f.write_str("cli"),
        }
    }
}

#[derive(Debug, Default)]
pub struct LoadedConfig {
    pub config: Config,
    /// Origin per dotted key actually set by some layer; keys absent here
    /// kept their built-in default.
    pub origins: BTreeMap<String, Origin>,
    /// Non-fatal problems (unknown keys) worth surfacing to the user.
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config file {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid TOML in {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("cannot render config as TOML: {0}")]
    Render(#[from] toml::ser::Error),
}

/// Load and merge all configuration layers. Missing files are fine; a file
/// that exists but cannot be read or parsed is an error.
pub fn load(repo_root: Option<&Path>, cli: &CliOverrides) -> Result<LoadedConfig, ConfigError> {
    let global = global_config_path_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    let project = repo_root.map(|root| root.join(".diffler").join("config.toml"));
    load_layers(global.as_deref(), project.as_deref(), cli)
}

fn global_config_path_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(home?).join(".config"),
    };
    Some(base.join("diffler").join("config.toml"))
}

fn load_layers(
    global: Option<&Path>,
    project: Option<&Path>,
    cli: &CliOverrides,
) -> Result<LoadedConfig, ConfigError> {
    let mut config = Config::default();
    let mut origins = BTreeMap::new();
    let mut warnings = Vec::new();

    let layers = [
        (global, Origin::Global as fn(PathBuf) -> Origin),
        (project, Origin::Project as fn(PathBuf) -> Origin),
    ];
    for (path, make_origin) in layers {
        if let Some(path) = path
            && path.is_file()
        {
            let layer = read_layer(path, &mut warnings)?;
            apply_layer(
                layer,
                &mut config,
                &mut origins,
                &make_origin(path.to_path_buf()),
                &mut warnings,
            );
        }
    }
    apply_cli(cli, &mut config, &mut origins);

    Ok(LoadedConfig {
        config,
        origins,
        warnings,
    })
}

/// Partial mirror of [`Config`] used for layering: every scalar is optional so
/// merging is per-field (later layer wins per field, never whole-section);
/// keys maps merge per entry.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialConfig {
    ui: PartialUi,
    mcp: PartialMcp,
    editor: PartialEditor,
    ci: PartialCi,
    keys: KeysConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialUi {
    theme: Option<String>,
    context_lines: Option<u32>,
    recent_commits: Option<usize>,
    // layouts are read as raw strings so an unknown value warns and falls back
    // (via [`FileLayout::from_str`]) instead of aborting the whole parse
    status_file_layout: Option<String>,
    diff_file_layout: Option<String>,
    side_by_side: Option<bool>,
    semantic_diff: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialMcp {
    enabled: Option<bool>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialEditor {
    command: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialCi {
    provider: Option<String>,
    poll_seconds: Option<u64>,
    gitlab: PartialCiGitLab,
    forgejo: PartialCiForgejo,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialCiGitLab {
    host: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialCiForgejo {
    host: Option<String>,
}

fn read_layer(path: &Path, warnings: &mut Vec<String>) -> Result<PartialConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let parse_err = |err: toml::de::Error| ConfigError::Parse {
        path: path.to_path_buf(),
        message: err.to_string(),
    };
    let de = toml::de::Deserializer::parse(&text).map_err(parse_err)?;
    serde_ignored::deserialize(de, |unknown| {
        warnings.push(format!("{}: unknown key `{unknown}`", path.display()));
    })
    .map_err(parse_err)
}

/// Apply a layer's file-layout value: an unknown string keeps the prior value
/// (the default) and warns, matching the theme key's lenient handling, rather
/// than aborting the parse.
fn set_layout(
    value: Option<String>,
    target: &mut FileLayout,
    key: &str,
    origin: &Origin,
    origins: &mut BTreeMap<String, Origin>,
    warnings: &mut Vec<String>,
) {
    let Some(value) = value else {
        return;
    };
    let (layout, warning) = FileLayout::from_str(&value, key, *target);
    if let Some(warning) = warning {
        warnings.push(warning);
        return;
    }
    *target = layout;
    origins.insert(key.to_owned(), origin.clone());
}

// flat per-key application list
#[allow(clippy::too_many_lines)]
fn apply_layer(
    layer: PartialConfig,
    config: &mut Config,
    origins: &mut BTreeMap<String, Origin>,
    origin: &Origin,
    warnings: &mut Vec<String>,
) {
    fn set<T>(
        value: Option<T>,
        target: &mut T,
        key: &str,
        origin: &Origin,
        origins: &mut BTreeMap<String, Origin>,
    ) {
        if let Some(value) = value {
            *target = value;
            origins.insert(key.to_owned(), origin.clone());
        }
    }

    set(
        layer.ui.theme,
        &mut config.ui.theme,
        "ui.theme",
        origin,
        origins,
    );
    set(
        layer.ui.context_lines,
        &mut config.ui.context_lines,
        "ui.context_lines",
        origin,
        origins,
    );
    set(
        layer.ui.recent_commits,
        &mut config.ui.recent_commits,
        "ui.recent_commits",
        origin,
        origins,
    );
    set_layout(
        layer.ui.status_file_layout,
        &mut config.ui.status_file_layout,
        "ui.status_file_layout",
        origin,
        origins,
        warnings,
    );
    set_layout(
        layer.ui.diff_file_layout,
        &mut config.ui.diff_file_layout,
        "ui.diff_file_layout",
        origin,
        origins,
        warnings,
    );
    set(
        layer.ui.side_by_side,
        &mut config.ui.side_by_side,
        "ui.side_by_side",
        origin,
        origins,
    );
    set(
        layer.ui.semantic_diff,
        &mut config.ui.semantic_diff,
        "ui.semantic_diff",
        origin,
        origins,
    );
    set(
        layer.mcp.enabled,
        &mut config.mcp.enabled,
        "mcp.enabled",
        origin,
        origins,
    );
    set(
        layer.mcp.port,
        &mut config.mcp.port,
        "mcp.port",
        origin,
        origins,
    );
    if let Some(command) = layer.editor.command {
        config.editor.command = Some(command);
        origins.insert("editor.command".to_owned(), origin.clone());
    }
    set(
        layer.ci.provider,
        &mut config.ci.provider,
        "ci.provider",
        origin,
        origins,
    );
    set(
        layer.ci.poll_seconds,
        &mut config.ci.poll_seconds,
        "ci.poll_seconds",
        origin,
        origins,
    );
    if let Some(host) = layer.ci.gitlab.host {
        config.ci.gitlab.host = Some(host);
        origins.insert("ci.gitlab.host".to_owned(), origin.clone());
    }
    if let Some(host) = layer.ci.forgejo.host {
        config.ci.forgejo.host = Some(host);
        origins.insert("ci.forgejo.host".to_owned(), origin.clone());
    }

    let key_sections = [
        (layer.keys.status, &mut config.keys.status, "status"),
        (layer.keys.diff, &mut config.keys.diff, "diff"),
        (layer.keys.log, &mut config.keys.log, "log"),
        (layer.keys.logs, &mut config.keys.logs, "logs"),
        (layer.keys.graph, &mut config.keys.graph, "graph"),
        (layer.keys.prs, &mut config.keys.prs, "prs"),
        (layer.keys.commit, &mut config.keys.commit, "commit"),
        (layer.keys.branch, &mut config.keys.branch, "branch"),
        (layer.keys.log_menu, &mut config.keys.log_menu, "log_menu"),
        (layer.keys.push, &mut config.keys.push, "push"),
        (layer.keys.pull, &mut config.keys.pull, "pull"),
        (layer.keys.fetch, &mut config.keys.fetch, "fetch"),
        (layer.keys.stash, &mut config.keys.stash, "stash"),
    ];
    for (entries, target, section) in key_sections {
        for (action, chord) in entries {
            let key_path = format!("keys.{section}.{action}");
            if let Err(err) = parse_chord(&chord) {
                warnings.push(format!("invalid chord \"{chord}\" for {key_path}: {err}"));
                continue;
            }
            origins.insert(key_path, origin.clone());
            target.insert(action, chord);
        }
    }
}

fn apply_cli(cli: &CliOverrides, config: &mut Config, origins: &mut BTreeMap<String, Origin>) {
    if let Some(port) = cli.port {
        config.mcp.port = port;
        origins.insert("mcp.port".to_owned(), Origin::Cli);
    }
    if let Some(enabled) = cli.mcp_enabled {
        config.mcp.enabled = enabled;
        origins.insert("mcp.enabled".to_owned(), Origin::Cli);
    }
    if let Some(theme) = &cli.theme {
        config.ui.theme.clone_from(theme);
        origins.insert("ui.theme".to_owned(), Origin::Cli);
    }
}

/// Scalar keys always listed in the `--dump` origins block; `keys.*` entries
/// are appended dynamically since their names come from the user.
const SCALAR_KEYS: [&str; 14] = [
    "ui.theme",
    "ui.context_lines",
    "ui.recent_commits",
    "ui.status_file_layout",
    "ui.diff_file_layout",
    "ui.side_by_side",
    "ui.semantic_diff",
    "mcp.enabled",
    "mcp.port",
    "editor.command",
    "ci.provider",
    "ci.poll_seconds",
    "ci.gitlab.host",
    "ci.forgejo.host",
];

/// Render the merged config as TOML followed by a comment block with the
/// origin of every tracked key, for `diffler config --dump`.
pub fn render_dump(loaded: &LoadedConfig) -> Result<String, ConfigError> {
    use std::fmt::Write as _;

    let mut out = toml::to_string(&loaded.config)?;
    out.push_str("\n## origins\n");
    for key in SCALAR_KEYS {
        let origin = loaded
            .origins
            .get(key)
            .map_or_else(|| Origin::Default.to_string(), ToString::to_string);
        let _ = writeln!(out, "# {key} = {origin}");
    }
    for (key, origin) in &loaded.origins {
        if key.starts_with("keys.") {
            let _ = writeln!(out, "# {key} = {origin}");
        }
    }
    if !loaded.warnings.is_empty() {
        out.push_str("\n## warnings\n");
        for warning in &loaded.warnings {
            let _ = writeln!(out, "# {warning}");
        }
    }
    Ok(out)
}

/// One key press in a chord, mirroring crossterm's `KeyEvent` shape: an
/// uppercase letter carries `shift` like the events crossterm delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A chord is one or more key presses in sequence (e.g. `cc`, `<c-x><c-c>`).
pub type Chord = Vec<KeyPress>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChordError {
    #[error("empty key chord")]
    Empty,
    #[error("unterminated `<` in key chord {0:?}")]
    Unterminated(String),
    #[error("unknown key {0:?} in key chord")]
    UnknownKey(String),
}

/// Parse a chord string: plain chars (`q`, `V` = shift), bracketed tokens
/// (`<c-r>`, `<a-x>`, `<cr>`, `<tab>`, `<esc>`, `<space>`, `<s-cr>`), and
/// concatenation for sequences (`cc`, `<c-x><c-c>`).
pub fn parse_chord(s: &str) -> Result<Chord, ChordError> {
    if s.is_empty() {
        return Err(ChordError::Empty);
    }
    let mut presses = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut token = String::new();
            let mut closed = false;
            for inner in chars.by_ref() {
                if inner == '>' {
                    closed = true;
                    break;
                }
                token.push(inner);
            }
            if !closed {
                return Err(ChordError::Unterminated(s.to_owned()));
            }
            presses.push(parse_bracketed(&token)?);
        } else {
            presses.push(plain_press(c));
        }
    }
    Ok(presses)
}

fn plain_press(c: char) -> KeyPress {
    KeyPress {
        code: KeyCode::Char(c),
        ctrl: false,
        alt: false,
        shift: c.is_uppercase(),
    }
}

fn parse_bracketed(token: &str) -> Result<KeyPress, ChordError> {
    let mut rest = token;
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    loop {
        if let Some(stripped) = strip_modifier(rest, 'c') {
            ctrl = true;
            rest = stripped;
        } else if let Some(stripped) = strip_modifier(rest, 'a') {
            alt = true;
            rest = stripped;
        } else if let Some(stripped) = strip_modifier(rest, 's') {
            shift = true;
            rest = stripped;
        } else {
            break;
        }
    }
    let code = match rest.to_ascii_lowercase().as_str() {
        "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        _ => {
            let mut rest_chars = rest.chars();
            match (rest_chars.next(), rest_chars.next()) {
                (Some(c), None) => {
                    shift = shift || c.is_uppercase();
                    // crossterm delivers shift+letter as Char('A')+SHIFT, so
                    // Char('a')+SHIFT would never match an incoming event
                    let c = if shift && c.is_ascii_lowercase() {
                        c.to_ascii_uppercase()
                    } else {
                        c
                    };
                    KeyCode::Char(c)
                }
                _ => return Err(ChordError::UnknownKey(format!("<{token}>"))),
            }
        }
    };
    Ok(KeyPress {
        code,
        ctrl,
        alt,
        shift,
    })
}

/// Strip a leading `m-` modifier prefix (case-insensitive), e.g. `c-` / `C-`.
fn strip_modifier(rest: &str, modifier: char) -> Option<&str> {
    let mut chars = rest.chars();
    if chars.next()?.to_ascii_lowercase() != modifier {
        return None;
    }
    if chars.next()? != '-' {
        return None;
    }
    Some(chars.as_str())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::*;

    fn press(code: KeyCode, ctrl: bool, alt: bool, shift: bool) -> KeyPress {
        KeyPress {
            code,
            ctrl,
            alt,
            shift,
        }
    }

    #[test]
    fn defaults_are_the_documented_values() {
        let config = Config::default();
        assert_eq!(config.ui.theme, "github-dark");
        assert_eq!(config.ui.context_lines, 3);
        assert_eq!(config.ui.recent_commits, 10);
        assert_eq!(config.ui.status_file_layout, FileLayout::List);
        assert_eq!(config.ui.diff_file_layout, FileLayout::Tree);
        assert!(!config.ui.side_by_side);
        assert!(config.ui.semantic_diff);
        assert!(config.mcp.enabled);
        assert_eq!(config.mcp.port, 8417);
        assert_eq!(config.editor.command, None);
        assert!(config.keys.status.is_empty());
        assert!(config.keys.diff.is_empty());
        assert!(config.keys.log.is_empty());
        assert!(config.keys.commit.is_empty());
        assert!(config.keys.branch.is_empty());
        assert!(config.keys.log_menu.is_empty());
        assert!(config.keys.push.is_empty());
        assert!(config.keys.pull.is_empty());
        assert!(config.keys.fetch.is_empty());
    }

    #[test]
    fn no_files_no_cli_yields_defaults_with_empty_origins() {
        let loaded = load_layers(None, None, &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.origins.is_empty());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn missing_files_are_fine() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_layers(
            Some(&dir.path().join("nope.toml")),
            Some(&dir.path().join("also-nope.toml")),
            &CliOverrides::default(),
        )
        .unwrap();
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn precedence_default_global_project_cli_per_field() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        let project = dir.path().join("project.toml");
        fs::write(
            &global,
            "[ui]\ntheme = \"global-theme\"\ncontext_lines = 5\n\n[mcp]\nport = 9000\n",
        )
        .unwrap();
        // project overrides only theme; context_lines must survive from global
        fs::write(&project, "[ui]\ntheme = \"project-theme\"\n").unwrap();
        let cli = CliOverrides {
            port: Some(9999),
            mcp_enabled: Some(false),
            theme: None,
        };

        let loaded = load_layers(Some(&global), Some(&project), &cli).unwrap();
        assert_eq!(loaded.config.ui.theme, "project-theme");
        assert_eq!(loaded.config.ui.context_lines, 5);
        assert_eq!(loaded.config.ui.recent_commits, 10);
        assert_eq!(loaded.config.mcp.port, 9999);
        assert!(!loaded.config.mcp.enabled);

        assert_eq!(loaded.origins["ui.theme"], Origin::Project(project));
        assert_eq!(loaded.origins["ui.context_lines"], Origin::Global(global));
        assert_eq!(loaded.origins["mcp.port"], Origin::Cli);
        assert_eq!(loaded.origins["mcp.enabled"], Origin::Cli);
        assert!(!loaded.origins.contains_key("ui.recent_commits"));
    }

    #[test]
    fn cli_theme_overrides_files() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        fs::write(&project, "[ui]\ntheme = \"project-theme\"\n").unwrap();
        let cli = CliOverrides {
            theme: Some("cli-theme".to_owned()),
            ..CliOverrides::default()
        };
        let loaded = load_layers(None, Some(&project), &cli).unwrap();
        assert_eq!(loaded.config.ui.theme, "cli-theme");
        assert_eq!(loaded.origins["ui.theme"], Origin::Cli);
    }

    #[test]
    fn keys_maps_merge_per_entry() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        let project = dir.path().join("project.toml");
        fs::write(
            &global,
            "[keys.status]\nquit = \"q\"\nrefresh = \"<c-r>\"\n\n[keys.diff]\nfold = \"<tab>\"\n",
        )
        .unwrap();
        fs::write(&project, "[keys.status]\nrefresh = \"R\"\n").unwrap();

        let loaded = load_layers(Some(&global), Some(&project), &CliOverrides::default()).unwrap();
        // per-entry merge: quit survives from global, refresh overridden by project
        assert_eq!(loaded.config.keys.status["quit"], "q");
        assert_eq!(loaded.config.keys.status["refresh"], "R");
        assert_eq!(loaded.config.keys.diff["fold"], "<tab>");
        assert_eq!(loaded.origins["keys.status.quit"], Origin::Global(global));
        assert_eq!(
            loaded.origins["keys.status.refresh"],
            Origin::Project(project)
        );
    }

    #[test]
    fn editor_command_layers() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        fs::write(&global, "[editor]\ncommand = \"hx\"\n").unwrap();
        let loaded = load_layers(Some(&global), None, &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.editor.command.as_deref(), Some("hx"));
        assert_eq!(loaded.origins["editor.command"], Origin::Global(global));
    }

    #[test]
    fn file_layouts_default_to_status_list_and_diff_tree() {
        let loaded = load_layers(None, None, &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.ui.status_file_layout, FileLayout::List);
        assert_eq!(loaded.config.ui.diff_file_layout, FileLayout::Tree);
    }

    #[test]
    fn file_layouts_override_in_either_direction() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        // flip both away from their defaults: status to tree, diff to list
        fs::write(
            &project,
            "[ui]\nstatus_file_layout = \"tree\"\ndiff_file_layout = \"list\"\n",
        )
        .unwrap();
        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.ui.status_file_layout, FileLayout::Tree);
        assert_eq!(loaded.config.ui.diff_file_layout, FileLayout::List);
        assert_eq!(
            loaded.origins["ui.status_file_layout"],
            Origin::Project(project.clone())
        );
        assert_eq!(
            loaded.origins["ui.diff_file_layout"],
            Origin::Project(project)
        );
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn unknown_file_layout_warns_and_keeps_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        fs::write(&project, "[ui]\nstatus_file_layout = \"nope\"\n").unwrap();
        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        // the bad value falls back to the default; nothing aborts
        assert_eq!(loaded.config.ui.status_file_layout, FileLayout::List);
        assert!(!loaded.origins.contains_key("ui.status_file_layout"));
        assert_eq!(loaded.warnings.len(), 1);
        let warning = &loaded.warnings[0];
        assert!(warning.contains("nope"), "names the bad value: {warning}");
        assert!(warning.contains("list"), "names the fallback: {warning}");
    }

    #[test]
    fn bad_toml_error_includes_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        fs::write(&path, "[ui\ntheme = ").unwrap();
        let err = load_layers(Some(&path), None, &CliOverrides::default()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "error display should name the file: {message}"
        );
    }

    #[test]
    fn type_error_includes_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("typed.toml");
        fs::write(&path, "[mcp]\nport = \"not-a-number\"\n").unwrap();
        let err = load_layers(Some(&path), None, &CliOverrides::default()).unwrap_err();
        assert!(err.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn unknown_keys_warn_but_do_not_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extra.toml");
        fs::write(
            &path,
            "[ui]\ntheme = \"t\"\ntypo_key = 1\n\n[surprise]\nx = 1\n",
        )
        .unwrap();
        let loaded = load_layers(Some(&path), None, &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.ui.theme, "t");
        assert_eq!(loaded.warnings.len(), 2);
        assert!(loaded.warnings.iter().any(|w| w.contains("ui.typo_key")));
        assert!(loaded.warnings.iter().any(|w| w.contains("surprise")));
        assert!(
            loaded
                .warnings
                .iter()
                .all(|w| w.contains(&path.display().to_string()))
        );
    }

    #[test]
    fn xdg_config_home_wins_over_home() {
        let path = global_config_path_from(
            Some(OsString::from("/xdg")),
            Some(OsString::from("/home/u")),
        );
        assert_eq!(path, Some(PathBuf::from("/xdg/diffler/config.toml")));
    }

    #[test]
    fn empty_xdg_falls_back_to_home_dot_config() {
        let path = global_config_path_from(Some(OsString::new()), Some(OsString::from("/home/u")));
        assert_eq!(
            path,
            Some(PathBuf::from("/home/u/.config/diffler/config.toml"))
        );
        let path = global_config_path_from(None, Some(OsString::from("/home/u")));
        assert_eq!(
            path,
            Some(PathBuf::from("/home/u/.config/diffler/config.toml"))
        );
    }

    #[test]
    fn no_home_no_xdg_means_no_global_file() {
        assert_eq!(global_config_path_from(None, None), None);
    }

    #[test]
    fn dump_lists_merged_values_and_origins() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        fs::write(
            &project,
            "[ui]\ntheme = \"dark\"\n\n[keys.status]\nquit = \"q\"\n",
        )
        .unwrap();
        let cli = CliOverrides {
            port: Some(1234),
            ..CliOverrides::default()
        };
        let loaded = load_layers(None, Some(&project), &cli).unwrap();
        let dump = render_dump(&loaded).unwrap();
        assert!(dump.contains("theme = \"dark\""));
        assert!(dump.contains("port = 1234"));
        assert!(dump.contains(&format!("# ui.theme = project:{}", project.display())));
        assert!(dump.contains("# mcp.port = cli"));
        assert!(dump.contains("# ui.context_lines = default"));
        assert!(dump.contains(&format!(
            "# keys.status.quit = project:{}",
            project.display()
        )));
    }

    #[test]
    fn chord_plain_char() {
        assert_eq!(
            parse_chord("q").unwrap(),
            vec![press(KeyCode::Char('q'), false, false, false)]
        );
    }

    #[test]
    fn chord_uppercase_implies_shift() {
        assert_eq!(
            parse_chord("V").unwrap(),
            vec![press(KeyCode::Char('V'), false, false, true)]
        );
    }

    #[test]
    fn chord_ctrl_combo() {
        assert_eq!(
            parse_chord("<c-r>").unwrap(),
            vec![press(KeyCode::Char('r'), true, false, false)]
        );
    }

    #[test]
    fn chord_alt_combo() {
        assert_eq!(
            parse_chord("<a-x>").unwrap(),
            vec![press(KeyCode::Char('x'), false, true, false)]
        );
    }

    #[test]
    fn chord_named_keys() {
        assert_eq!(
            parse_chord("<cr>").unwrap(),
            vec![press(KeyCode::Enter, false, false, false)]
        );
        assert_eq!(
            parse_chord("<tab>").unwrap(),
            vec![press(KeyCode::Tab, false, false, false)]
        );
        assert_eq!(
            parse_chord("<esc>").unwrap(),
            vec![press(KeyCode::Esc, false, false, false)]
        );
        assert_eq!(
            parse_chord("<space>").unwrap(),
            vec![press(KeyCode::Char(' '), false, false, false)]
        );
    }

    #[test]
    fn chord_shift_enter() {
        assert_eq!(
            parse_chord("<s-cr>").unwrap(),
            vec![press(KeyCode::Enter, false, false, true)]
        );
    }

    #[test]
    fn chord_stacked_modifiers() {
        // crossterm delivers ctrl+shift+x as Char('X')+CTRL+SHIFT
        assert_eq!(
            parse_chord("<c-s-x>").unwrap(),
            vec![press(KeyCode::Char('X'), true, false, true)]
        );
    }

    #[test]
    fn chord_two_key_sequences() {
        assert_eq!(
            parse_chord("cc").unwrap(),
            vec![
                press(KeyCode::Char('c'), false, false, false),
                press(KeyCode::Char('c'), false, false, false),
            ]
        );
        assert_eq!(
            parse_chord("<c-x><c-c>").unwrap(),
            vec![
                press(KeyCode::Char('x'), true, false, false),
                press(KeyCode::Char('c'), true, false, false),
            ]
        );
        assert_eq!(
            parse_chord("g<cr>").unwrap(),
            vec![
                press(KeyCode::Char('g'), false, false, false),
                press(KeyCode::Enter, false, false, false),
            ]
        );
    }

    #[test]
    fn chord_invalid_inputs() {
        assert_eq!(parse_chord(""), Err(ChordError::Empty));
        assert_eq!(
            parse_chord("<c-"),
            Err(ChordError::Unterminated("<c-".to_owned()))
        );
        assert_eq!(
            parse_chord("<weird>"),
            Err(ChordError::UnknownKey("<weird>".to_owned()))
        );
        assert_eq!(
            parse_chord("<>"),
            Err(ChordError::UnknownKey("<>".to_owned()))
        );
    }

    // <s-{letter}> must produce the same press as the bare uppercase
    // letter, because crossterm delivers shift+a as Char('A')+SHIFT, not
    // Char('a')+SHIFT.
    #[test]
    fn chord_shift_letter_normalizes_to_uppercase() {
        assert_eq!(parse_chord("<s-a>").unwrap(), parse_chord("A").unwrap());
        assert_eq!(parse_chord("<s-z>").unwrap(), parse_chord("Z").unwrap());
        // uppercase input already works the same way (regression guard)
        assert_eq!(parse_chord("<s-A>").unwrap(), parse_chord("A").unwrap());
    }

    // A bad chord string in a keys section must not abort loading;
    // the entry is dropped and a warning naming the dotted key path is emitted.
    #[test]
    fn bad_chord_in_keys_warns_and_drops_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        fs::write(
            &path,
            "[keys.status]\nquit = \"q\"\nrefresh = \"<ctlr-r>\"\n",
        )
        .unwrap();
        let loaded = load_layers(Some(&path), None, &CliOverrides::default()).unwrap();
        // good chord survives
        assert_eq!(loaded.config.keys.status["quit"], "q");
        // bad chord is dropped
        assert!(!loaded.config.keys.status.contains_key("refresh"));
        // warning names the dotted key path and the bad string
        assert_eq!(loaded.warnings.len(), 1, "expected exactly one warning");
        let w = &loaded.warnings[0];
        assert!(
            w.contains("keys.status.refresh"),
            "warning should name the key path: {w}"
        );
        assert!(
            w.contains("<ctlr-r>"),
            "warning should quote the bad chord: {w}"
        );
    }

    // Good chords in keys sections are stored and not warned about.
    #[test]
    fn valid_chords_in_keys_stored_without_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys_ok.toml");
        fs::write(&path, "[keys.diff]\nfold = \"<tab>\"\nnext = \"j\"\n").unwrap();
        let loaded = load_layers(Some(&path), None, &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.keys.diff["fold"], "<tab>");
        assert_eq!(loaded.config.keys.diff["next"], "j");
        assert!(loaded.warnings.is_empty());
    }
}
