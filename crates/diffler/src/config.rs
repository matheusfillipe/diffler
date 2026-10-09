//! Layered TOML configuration: defaults → global file → project file → CLI.
//!
//! The global file is `$XDG_CONFIG_HOME/diffler/config.toml` (default
//! `~/.config`) on every OS, macOS included.
//!
//! A literal '<' is not bindable, since chords have no `<lt>` escape.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use diffler_core::classify::{Kind, Rules};
use diffler_core::diffalgo::DiffAlgorithm;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Every field has a default, so any layer may be absent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub diff: DiffConfig,
    pub mcp: McpConfig,
    pub editor: EditorConfig,
    pub ci: CiConfig,
    pub classify: ClassifyConfig,
    /// `glob = language` highlighting rules, checked before name and shebang.
    pub syntax: BTreeMap<String, String>,
    pub keys: KeysConfig,
}

impl Config {
    pub fn diff_settings(&self) -> diffler_core::diffalgo::DiffSettings {
        diffler_core::diffalgo::DiffSettings {
            context_lines: self.ui.context_lines,
            algorithm: self.diff.algorithm,
            indent_heuristic: self.diff.indent_heuristic,
        }
    }
}

/// The live algorithm picker writes back here, so a later background refresh
/// keeps the switched algorithm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffConfig {
    pub algorithm: DiffAlgorithm,
    /// Shifts ambiguous hunk boundaries to indentation, as git does by default.
    pub indent_heuristic: bool,
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            algorithm: DiffAlgorithm::default(),
            indent_heuristic: diffler_core::git::DEFAULT_INDENT_HEURISTIC,
        }
    }
}

/// Globs that pin a path into a sidebar bucket, ahead of `linguist-*`
/// attributes and the built-in table. Gitignore-flavoured: a pattern with no
/// `/` matches the basename at any depth.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClassifyConfig {
    pub source: Vec<String>,
    pub tests: Vec<String>,
    pub docs: Vec<String>,
    pub config: Vec<String>,
    pub build: Vec<String>,
    pub generated: Vec<String>,
    pub assets: Vec<String>,
    pub other: Vec<String>,
}

impl ClassifyConfig {
    /// Buckets go in sidebar order so two patterns claiming one path resolve
    /// the same way every time.
    pub fn rules(&self) -> Rules {
        let patterns = |kind: Kind| match kind {
            Kind::Source => &self.source,
            Kind::Tests => &self.tests,
            Kind::Docs => &self.docs,
            Kind::Config => &self.config,
            Kind::Build => &self.build,
            Kind::Generated => &self.generated,
            Kind::Assets => &self.assets,
            Kind::Other => &self.other,
        };
        Rules::new(
            Kind::ALL
                .into_iter()
                .filter(|kind| !patterns(*kind).is_empty())
                .map(|kind| (kind, patterns(kind).clone()))
                .collect(),
        )
    }
}

/// `Review`, `Kinds` and `Walkthrough` apply to the diff sidebar only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileLayout {
    List,
    Tree,
    Review,
    Kinds,
    Walkthrough,
}

impl FileLayout {
    /// An unknown value falls back to `default` with a warning, so a typo
    /// never aborts startup.
    fn from_str(value: &str, key: &str, default: Self) -> (Self, Option<String>) {
        match value {
            "list" => (Self::List, None),
            "tree" => (Self::Tree, None),
            "review" => (Self::Review, None),
            "kinds" => (Self::Kinds, None),
            "walkthrough" => (Self::Walkthrough, None),
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
            Self::Review => "review",
            Self::Kinds => "kinds",
            Self::Walkthrough => "walkthrough",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub context_lines: u32,
    pub recent_commits: usize,
    pub status_file_layout: FileLayout,
    pub diff_file_layout: FileLayout,
    pub side_by_side: bool,
    /// Emphasize changes by AST diff, leaving reindentation and block wrapping unflagged.
    pub semantic_diff: bool,
    pub show_agent_activity: bool,
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
            show_agent_activity: true,
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
    /// `None` falls back to `$DIFFLER_EDITOR`, then `$EDITOR`, read at the point of use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// `provider = "auto"` detects the forge from the remote and config files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiConfig {
    pub provider: String,
    /// Unset picks the remote the branch pushes to, then `origin`.
    pub remote: Option<String>,
    pub poll_seconds: u64,
    pub gitlab: CiGitLabConfig,
    pub forgejo: CiForgejoConfig,
}

impl Default for CiConfig {
    fn default() -> Self {
        Self {
            provider: "auto".to_owned(),
            remote: None,
            poll_seconds: 5,
            gitlab: CiGitLabConfig::default(),
            forgejo: CiForgejoConfig::default(),
        }
    }
}

/// `host` overrides remote detection for a self-hosted instance.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CiGitLabConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// `host` overrides remote detection for a self-hosted instance.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CiForgejoConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Action name → chord, per screen and per transient (`[keys.commit] amend = "m"`).
/// Entries override the keymap's built-in bindings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    pub status: BTreeMap<String, String>,
    pub diff: BTreeMap<String, String>,
    pub log: BTreeMap<String, String>,
    /// Configs written as `[keys.logs]` still load.
    #[serde(alias = "logs")]
    pub ci_log: BTreeMap<String, String>,
    pub graph: BTreeMap<String, String>,
    pub prs: BTreeMap<String, String>,
    pub file: BTreeMap<String, String>,
    pub stats: BTreeMap<String, String>,
    pub tabs: BTreeMap<String, String>,
    pub commit: BTreeMap<String, String>,
    pub branch: BTreeMap<String, String>,
    pub diff_menu: BTreeMap<String, String>,
    pub log_menu: BTreeMap<String, String>,
    pub push: BTreeMap<String, String>,
    pub pull: BTreeMap<String, String>,
    pub fetch: BTreeMap<String, String>,
    pub stash: BTreeMap<String, String>,
}

impl KeysConfig {
    pub fn transient(&self, kind: crate::transient::TransientKind) -> &BTreeMap<String, String> {
        match kind {
            crate::transient::TransientKind::Commit => &self.commit,
            crate::transient::TransientKind::Branch => &self.branch,
            crate::transient::TransientKind::Diff => &self.diff_menu,
            crate::transient::TransientKind::Log => &self.log_menu,
            crate::transient::TransientKind::Push => &self.push,
            crate::transient::TransientKind::Pull => &self.pull,
            crate::transient::TransientKind::Fetch => &self.fetch,
            crate::transient::TransientKind::Stash => &self.stash,
        }
    }
}

fn project_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".diffler").join(PROJECT_CONFIG)
}

const PROJECT_CONFIG: &str = "config.toml";

/// Keeps every other line of the project config as the reader wrote it.
pub fn save_syntax_rule(repo_root: &Path, glob: &str, language: &str) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(project_config_path(repo_root)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let edited = with_syntax_rule(&text, glob, language).ok_or_else(|| {
        std::io::Error::other("edit the [syntax] section of .diffler/config.toml by hand")
    })?;
    diffler_core::store::write_file(repo_root, PROJECT_CONFIG, &edited)
}

/// `None` when `text` spells its rules in a shape we cannot edit line by line.
fn with_syntax_rule(text: &str, glob: &str, language: &str) -> Option<String> {
    let rule = format!("{} = {}", toml_string(glob), toml_string(language));
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if let Some(header) = lines
        .iter()
        .position(|line| is_table_header(line, "syntax"))
    {
        let end = lines
            .iter()
            .skip(header + 1)
            .position(|line| line.trim_start().starts_with('['))
            .map_or(lines.len(), |at| header + 1 + at);
        let existing =
            (header + 1..end).find(|&at| lines.get(at).is_some_and(|line| sets_key(line, glob)));
        match existing.and_then(|at| lines.get_mut(at)) {
            Some(line) => *line = rule,
            None => lines.insert(header + 1, rule),
        }
    } else {
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("[syntax]".to_owned());
        lines.push(rule);
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let edited = lines.join(newline) + newline;
    let parsed: toml::Table = toml::from_str(&edited).ok()?;
    let saved = parsed.get("syntax")?.get(glob)?.as_str()?;
    (saved == language).then_some(edited)
}

fn is_table_header(line: &str, name: &str) -> bool {
    line.trim_start().starts_with('[')
        && toml::from_str::<toml::Table>(line).is_ok_and(|table| {
            table.len() == 1 && table.get(name).is_some_and(toml::Value::is_table)
        })
}

fn sets_key(line: &str, key: &str) -> bool {
    toml::from_str::<toml::Table>(line).is_ok_and(|table| table.contains_key(key))
}

fn toml_string(value: &str) -> String {
    toml::Value::from(value).to_string()
}

/// Every flag maps to a config key.
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
    /// A key absent here kept its built-in default.
    pub origins: BTreeMap<String, Origin>,
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

/// A missing file is skipped; an unreadable or invalid one is an error.
pub fn load(repo_root: Option<&Path>, cli: &CliOverrides) -> Result<LoadedConfig, ConfigError> {
    let global = global_config_path_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    let project = repo_root.map(project_config_path);
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

/// Every scalar is optional so a later layer wins per field; keys maps merge per entry.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialConfig {
    ui: PartialUi,
    diff: PartialDiff,
    mcp: PartialMcp,
    editor: PartialEditor,
    ci: PartialCi,
    classify: ClassifyConfig,
    syntax: BTreeMap<String, String>,
    keys: KeysConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialDiff {
    // raw strings, so an unknown value warns without aborting the parse
    algorithm: Option<String>,
    indent_heuristic: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PartialUi {
    theme: Option<String>,
    context_lines: Option<u32>,
    recent_commits: Option<usize>,
    // raw strings, so an unknown value warns without aborting the parse
    status_file_layout: Option<String>,
    diff_file_layout: Option<String>,
    side_by_side: Option<bool>,
    semantic_diff: Option<bool>,
    show_agent_activity: Option<bool>,
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
    remote: Option<String>,
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

/// Each key accepts its own set: the review layout needs viewed marks, and the
/// flat list suits only short paths. A rejected value warns and keeps the prior one.
fn set_layout(
    value: Option<String>,
    target: &mut FileLayout,
    key: &str,
    accepted: &[FileLayout],
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
    if !accepted.contains(&layout) {
        let names = accepted
            .iter()
            .map(|accepted| format!("\"{accepted}\""))
            .collect::<Vec<_>>()
            .join(" or ");
        warnings.push(format!("{key} takes {names}, using \"{target}\""));
        return;
    }
    *target = layout;
    origins.insert(key.to_owned(), origin.clone());
}

fn set_algorithm(
    value: Option<String>,
    target: &mut DiffAlgorithm,
    origin: &Origin,
    origins: &mut BTreeMap<String, Origin>,
    warnings: &mut Vec<String>,
) {
    let Some(value) = value else {
        return;
    };
    let Some(algorithm) = DiffAlgorithm::parse(&value) else {
        warnings.push(format!(
            "unknown diff.algorithm \"{value}\", using \"{target}\""
        ));
        return;
    };
    *target = algorithm;
    origins.insert("diff.algorithm".to_owned(), origin.clone());
}

// a flat list, one statement per key
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
        &[FileLayout::List, FileLayout::Tree],
        origin,
        origins,
        warnings,
    );
    set_layout(
        layer.ui.diff_file_layout,
        &mut config.ui.diff_file_layout,
        "ui.diff_file_layout",
        &[FileLayout::Tree, FileLayout::Review, FileLayout::Kinds],
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
    set_algorithm(
        layer.diff.algorithm,
        &mut config.diff.algorithm,
        origin,
        origins,
        warnings,
    );
    set(
        layer.diff.indent_heuristic,
        &mut config.diff.indent_heuristic,
        "diff.indent_heuristic",
        origin,
        origins,
    );
    set(
        layer.ui.show_agent_activity,
        &mut config.ui.show_agent_activity,
        "ui.show_agent_activity",
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
    if let Some(remote) = layer.ci.remote {
        config.ci.remote = Some(remote);
        origins.insert("ci.remote".to_owned(), origin.clone());
    }
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

    // a bucket a layer lists replaces the lower layer's, so the effective globs stay readable
    let classify_buckets = [
        (layer.classify.source, &mut config.classify.source, "source"),
        (layer.classify.tests, &mut config.classify.tests, "tests"),
        (layer.classify.docs, &mut config.classify.docs, "docs"),
        (layer.classify.config, &mut config.classify.config, "config"),
        (layer.classify.build, &mut config.classify.build, "build"),
        (
            layer.classify.generated,
            &mut config.classify.generated,
            "generated",
        ),
        (layer.classify.assets, &mut config.classify.assets, "assets"),
        (layer.classify.other, &mut config.classify.other, "other"),
    ];
    for (globs, target, bucket) in classify_buckets {
        if globs.is_empty() {
            continue;
        }
        origins.insert(format!("classify.{bucket}"), origin.clone());
        *target = globs;
    }

    for (glob, language) in layer.syntax {
        if diffler_core::syntax::registry::REGISTRY
            .by_name(&language)
            .is_none()
        {
            warnings.push(format!(
                "syntax.\"{glob}\": no bundled language \"{language}\""
            ));
            continue;
        }
        origins.insert(format!("syntax.{glob}"), origin.clone());
        config.syntax.insert(glob, language);
    }

    let key_sections = [
        (layer.keys.status, &mut config.keys.status, "status"),
        (layer.keys.diff, &mut config.keys.diff, "diff"),
        (layer.keys.log, &mut config.keys.log, "log"),
        (layer.keys.ci_log, &mut config.keys.ci_log, "ci_log"),
        (layer.keys.graph, &mut config.keys.graph, "graph"),
        (layer.keys.prs, &mut config.keys.prs, "prs"),
        (layer.keys.file, &mut config.keys.file, "file"),
        (layer.keys.stats, &mut config.keys.stats, "stats"),
        (layer.keys.tabs, &mut config.keys.tabs, "tabs"),
        (layer.keys.commit, &mut config.keys.commit, "commit"),
        (layer.keys.branch, &mut config.keys.branch, "branch"),
        (
            layer.keys.diff_menu,
            &mut config.keys.diff_menu,
            "diff_menu",
        ),
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

/// Always listed in the `--dump` origins block; `keys.*` entries follow from the user's config.
const SCALAR_KEYS: [&str; 18] = [
    "ui.theme",
    "ui.context_lines",
    "ui.recent_commits",
    "ui.status_file_layout",
    "ui.diff_file_layout",
    "ui.side_by_side",
    "ui.semantic_diff",
    "ui.show_agent_activity",
    "diff.algorithm",
    "diff.indent_heuristic",
    "mcp.enabled",
    "mcp.port",
    "editor.command",
    "ci.provider",
    "ci.remote",
    "ci.poll_seconds",
    "ci.gitlab.host",
    "ci.forgejo.host",
];

/// `diffler config --dump`: the merged TOML, then each key's origin as comments.
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

/// An uppercase letter carries `shift`, as crossterm's events do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

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

/// Plain chars (`V` is shift+v), bracketed tokens (`<c-r>`, `<s-cr>`), and
/// concatenation for sequences (`<c-x><c-c>`).
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

pub(crate) fn single_press(chord: &str) -> Option<KeyPress> {
    let mut presses = parse_chord(chord).ok()?;
    if presses.len() == 1 {
        Some(presses.remove(0))
    } else {
        None
    }
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
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        _ => {
            let mut rest_chars = rest.chars();
            match (rest_chars.next(), rest_chars.next()) {
                (Some(c), None) => {
                    shift = shift || c.is_uppercase();
                    // crossterm delivers shift+a as Char('A')+SHIFT
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

/// Case-insensitive: `c-` and `C-` both match.
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
    fn classify_globs_replace_per_bucket_and_drive_the_rules() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        let project = dir.path().join("project.toml");
        fs::write(
            &global,
            "[classify]\ntests = [\"harness/**\"]\ndocs = [\"notes/**\"]\n",
        )
        .unwrap();
        fs::write(&project, "[classify]\ntests = [\"e2e/**\"]\n").unwrap();

        let loaded = load_layers(Some(&global), Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.classify.tests, vec!["e2e/**".to_owned()]);
        assert_eq!(
            loaded.config.classify.docs,
            vec!["notes/**".to_owned()],
            "a bucket the project leaves alone keeps the global globs"
        );
        assert_eq!(loaded.origins["classify.tests"], Origin::Project(project));

        let rules = loaded.config.classify.rules();
        assert_eq!(rules.kind("e2e/login.rs", None), Kind::Tests);
        assert_eq!(rules.kind("harness/login.rs", None), Kind::Source);
        assert_eq!(rules.kind("notes/plan.rs", None), Kind::Docs);
    }

    #[test]
    fn keys_logs_alias_still_loads_into_ci_log() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        fs::write(&project, "[keys.logs]\nfold = \"<tab>\"\n").unwrap();

        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.keys.ci_log["fold"], "<tab>");
        assert_eq!(loaded.origins["keys.ci_log.fold"], Origin::Project(project));
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
        fs::write(
            &project,
            "[ui]\nstatus_file_layout = \"tree\"\ndiff_file_layout = \"review\"\n",
        )
        .unwrap();
        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.ui.status_file_layout, FileLayout::Tree);
        assert_eq!(loaded.config.ui.diff_file_layout, FileLayout::Review);
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
    fn each_screen_rejects_the_layout_it_does_not_offer() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        // the status screen has no viewed marks, the diff sidebar no flat list
        fs::write(
            &project,
            "[ui]\nstatus_file_layout = \"review\"\ndiff_file_layout = \"list\"\n",
        )
        .unwrap();
        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.ui.status_file_layout, FileLayout::List);
        assert_eq!(loaded.config.ui.diff_file_layout, FileLayout::Tree);
        assert_eq!(loaded.warnings.len(), 2);
        for warning in &loaded.warnings {
            assert!(warning.contains("takes"), "names what it takes: {warning}");
        }
    }

    #[test]
    fn unknown_file_layout_warns_and_keeps_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        fs::write(&project, "[ui]\nstatus_file_layout = \"nope\"\n").unwrap();
        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.ui.status_file_layout, FileLayout::List);
        assert!(!loaded.origins.contains_key("ui.status_file_layout"));
        assert_eq!(loaded.warnings.len(), 1);
        let warning = &loaded.warnings[0];
        assert!(warning.contains("nope"), "names the bad value: {warning}");
        assert!(warning.contains("list"), "names the fallback: {warning}");
    }

    #[test]
    fn diff_algorithm_and_indent_heuristic_parse_from_a_project_layer() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        fs::write(
            &project,
            "[diff]\nalgorithm = \"histogram\"\nindent_heuristic = false\n",
        )
        .unwrap();
        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.warnings, Vec::<String>::new());
        assert_eq!(loaded.config.diff.algorithm, DiffAlgorithm::Histogram);
        assert!(!loaded.config.diff.indent_heuristic);
        assert!(loaded.origins.contains_key("diff.algorithm"));
        assert!(loaded.origins.contains_key("diff.indent_heuristic"));
    }

    #[test]
    fn unknown_diff_algorithm_warns_and_keeps_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        fs::write(&project, "[diff]\nalgorithm = \"bogus\"\n").unwrap();
        let loaded = load_layers(None, Some(&project), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.config.diff.algorithm, DiffAlgorithm::Myers);
        assert!(!loaded.origins.contains_key("diff.algorithm"));
        assert_eq!(loaded.warnings.len(), 1);
        let warning = &loaded.warnings[0];
        assert!(warning.contains("bogus"), "names the bad value: {warning}");
        assert!(warning.contains("myers"), "names the fallback: {warning}");
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

    #[test]
    fn chord_shift_letter_normalizes_to_uppercase() {
        assert_eq!(parse_chord("<s-a>").unwrap(), parse_chord("A").unwrap());
        assert_eq!(parse_chord("<s-z>").unwrap(), parse_chord("Z").unwrap());
        assert_eq!(parse_chord("<s-A>").unwrap(), parse_chord("A").unwrap());
    }

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
        assert_eq!(loaded.config.keys.status["quit"], "q");
        assert!(!loaded.config.keys.status.contains_key("refresh"));
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

    #[test]
    fn a_syntax_rule_lands_in_its_section_and_leaves_the_rest_alone() {
        let edit = |text, glob| with_syntax_rule(text, glob, "bash").expect("editable");
        assert_eq!(edit("", "*.env"), "[syntax]\n\"*.env\" = \"bash\"\n");

        let existing = "[ui]\ntheme = \"nord\"\n\n[ syntax ] # mine\n'*.env'=\"toml\"\nJenkinsfile = \"groovy\"\n\n[mcp]\nport = 1\n";
        let replaced = edit(existing, "*.env");
        assert_eq!(replaced.matches("*.env").count(), 1, "{replaced}");
        assert!(!replaced.contains("toml") && replaced.contains("port = 1"));
        assert!(edit(existing, "Jenkinsfile").contains("Jenkinsfile\" = \"bash\""));

        let added = edit(existing, "Makefile.in");
        let rule = added.find("Makefile.in").expect("added");
        assert!(rule < added.find("[mcp]").expect("kept"), "{added}");

        assert!(edit("[ui]\r\ntheme = \"nord\"\r\n", "*.env").ends_with("\"bash\"\r\n"));
        assert_eq!(
            with_syntax_rule("syntax = { \"*.env\" = \"toml\" }\n", "*.env", "bash"),
            None
        );
    }

    #[test]
    fn a_syntax_rule_naming_no_bundled_language_is_dropped_with_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "[syntax]\n\"*.tpl\" = \"yaml\"\n\"*.x\" = \"yml\"\n").expect("write");
        let loaded = load_layers(None, Some(&path), &CliOverrides::default()).expect("load");
        assert_eq!(
            loaded.config.syntax.get("*.tpl").map(String::as_str),
            Some("yaml")
        );
        assert!(!loaded.config.syntax.contains_key("*.x"));
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
    }

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

    /// The example config with its `# key = value` lines uncommented, so we can
    /// compare it against the built-in defaults.
    fn uncommented_example() -> String {
        const EXAMPLE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/config.example.toml"
        ));
        EXAMPLE
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if is_assignment(rest) => rest,
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_assignment(line: &str) -> bool {
        line.split_once(" = ").is_some_and(|(key, _)| {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    }

    #[test]
    fn example_config_documents_the_real_keys_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("example.toml");
        fs::write(&path, uncommented_example()).unwrap();
        let loaded = load_layers(Some(&path), None, &CliOverrides::default()).unwrap();
        assert_eq!(loaded.warnings, Vec::<String>::new());

        assert_eq!(loaded.config.ui, UiConfig::default());
        assert_eq!(loaded.config.mcp, McpConfig::default());
        assert_eq!(loaded.config.ci.provider, CiConfig::default().provider);
        assert_eq!(
            loaded.config.ci.poll_seconds,
            CiConfig::default().poll_seconds
        );

        let keys = &loaded.config.keys;
        for (context, documented) in [
            (crate::keymap::Context::Status, &keys.status),
            (crate::keymap::Context::Diff, &keys.diff),
            (crate::keymap::Context::Log, &keys.log),
            (crate::keymap::Context::CiLog, &keys.ci_log),
            (crate::keymap::Context::Graph, &keys.graph),
            (crate::keymap::Context::Prs, &keys.prs),
        ] {
            let (_, warnings) = crate::keymap::Keymap::for_context(context, keys);
            assert_eq!(warnings, Vec::<String>::new(), "{context:?}");
            let (built_in, _) = crate::keymap::Keymap::for_context(context, &KeysConfig::default());
            for (name, chord) in documented {
                if let Some(kind) = crate::transient::TransientKind::ALL
                    .into_iter()
                    .find(|kind| kind.name() == name)
                {
                    assert_eq!(
                        built_in.prefix_chord(kind).as_deref(),
                        Some(chord.as_str()),
                        "{context:?} {name}"
                    );
                    continue;
                }
                assert!(
                    built_in.bindings().iter().any(|(built, action)| {
                        action.name() == name && crate::keymap::render_chord(built) == *chord
                    }),
                    "[keys.{context:?}] {name} = {chord:?} is not a built-in binding"
                );
            }
            for (_, action) in built_in.bindings() {
                assert!(
                    OMITTED_MOTIONS.contains(&action.name())
                        || documented.contains_key(action.name()),
                    "{} is bound on {context:?} but undocumented in the example config",
                    action.name()
                );
            }
        }

        for kind in crate::transient::TransientKind::ALL {
            let (documented, warnings) =
                crate::transient::Transient::build(kind, &loaded.config.keys);
            assert_eq!(warnings, Vec::<String>::new(), "{}", kind.name());
            let (built_in, _) = crate::transient::Transient::build(kind, &KeysConfig::default());
            assert_eq!(documented, built_in, "{}", kind.name());
            assert_eq!(
                keys.transient(kind).len(),
                built_in.flat_entries().count(),
                "the {} menu documents a different number of rows than it has",
                kind.name()
            );
        }
    }

    /// Motions the example config leaves to the `?` popup, so the reverse check
    /// below only demands the bindings a user would want to remap.
    const OMITTED_MOTIONS: &[&str] = &[
        "move_down",
        "move_up",
        "go_top",
        "go_bottom",
        "half_page_down",
        "half_page_up",
        "full_page_down",
        "full_page_up",
        "search",
        "search_next",
        "search_prev",
        "palette",
        "help",
        "quit",
        "back",
    ];
}
