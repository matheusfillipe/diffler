//! Color themes. `github-dark` is the default; `ui.theme` selects a built-in or
//! a TOML file in the themes directory, each pairing its palette with a
//! syntax-highlight theme.

use std::path::Path;

use diffler_core::highlight::SyntaxTheme;
use ratatui::style::{Color, Style};
use serde::Deserialize;

use crate::config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub bg: Color,
    pub panel: Color,
    pub cursor_line: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub purple: Color,
    pub border: Color,
    pub del_line_bg: Color,
    pub add_line_bg: Color,
    pub del_emph_bg: Color,
    pub add_emph_bg: Color,
    /// Background for lines a comment is anchored to (GitHub-style amber), so
    /// the scope of a multi-line review comment is visible in the diff.
    pub annotated: Color,
    pub search: Color,
    pub search_current: Color,
    /// Diffstat added foreground (a readable green for `+N` counts). Deletes
    /// reuse `error_fg`, the same red the status accent uses for them.
    pub added: Color,
    /// Status-bar message severities (attention/danger).
    pub warn_fg: Color,
    pub error_fg: Color,
    /// Status-bar mode chip (e.g. ` STATUS `).
    pub chip: Style,
    /// Syntax-highlight palette paired with this UI theme.
    pub syntax: SyntaxTheme,
}

type Builder = fn() -> Theme;

const DEFAULT_BUILTIN: (&str, Builder) = ("github-dark", Theme::github_dark);

pub const DEFAULT: &str = DEFAULT_BUILTIN.0;

const BUILTINS: &[(&str, Builder)] = &[
    DEFAULT_BUILTIN,
    ("catppuccin-mocha", Theme::catppuccin_mocha),
    ("tokyo-night", Theme::tokyo_night),
    ("gruvbox-dark", Theme::gruvbox_dark),
    ("nord", Theme::nord),
    ("rose-pine", Theme::rose_pine),
    ("kanagawa", Theme::kanagawa),
    ("dracula", Theme::dracula),
    ("github-light", Theme::github_light),
];

pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTINS.iter().map(|(name, _)| *name)
}

fn builtin(name: &str) -> Option<Theme> {
    BUILTINS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, build)| build())
}

/// Built-ins first, then the user's theme files by name.
pub fn names() -> Vec<String> {
    names_in(config::themes_dir().as_deref())
}

fn names_in(dir: Option<&Path>) -> Vec<String> {
    builtin_names()
        .map(str::to_owned)
        .chain(user_names(dir))
        .collect()
}

fn user_names(dir: Option<&Path>) -> Vec<String> {
    let Some(entries) = dir.and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
        .filter(|name| builtin(name).is_none())
        .collect();
    names.sort();
    names
}

macro_rules! theme_file {
    ($($colour:ident),* $(,)?) => {
        /// A user theme file. Every colour is optional and replaces the one in
        /// the theme it extends.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ThemeFile {
            extends: Option<String>,
            syntax: Option<String>,
            chip_fg: Option<String>,
            chip_bg: Option<String>,
            $($colour: Option<String>,)*
        }

        impl ThemeFile {
            fn paint_colours(&self, theme: &mut Theme) -> Result<(), String> {
                $(if let Some(text) = &self.$colour {
                    theme.$colour = parse_hex(stringify!($colour), text)?;
                })*
                Ok(())
            }
        }
    };
}

theme_file!(
    bg,
    panel,
    cursor_line,
    fg,
    dim,
    accent,
    purple,
    border,
    del_line_bg,
    add_line_bg,
    del_emph_bg,
    add_emph_bg,
    annotated,
    search,
    search_current,
    added,
    warn_fg,
    error_fg,
);

fn parse_hex(field: &str, text: &str) -> Result<Color, String> {
    let bad = || format!("bad colour for {field}, use #rrggbb");
    let digits = text
        .strip_prefix('#')
        .filter(|digits| digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(bad)?;
    let [_, red, green, blue] = u32::from_str_radix(digits, 16)
        .map_err(|_| bad())?
        .to_be_bytes();
    Ok(Color::Rgb(red, green, blue))
}

fn parse_theme_file(text: &str) -> Result<Theme, String> {
    let file: ThemeFile = toml::from_str(text).map_err(|err| err.message().to_owned())?;
    let base = file.extends.as_deref().unwrap_or(DEFAULT);
    let mut theme = builtin(base).ok_or_else(|| format!("unknown extends \"{base}\""))?;
    if let Some(name) = &file.syntax {
        theme.syntax = builtin(name)
            .ok_or_else(|| format!("unknown syntax \"{name}\""))?
            .syntax;
    }
    file.paint_colours(&mut theme)?;
    let chip_text = file
        .chip_fg
        .as_deref()
        .map_or(Ok(theme.bg), |text| parse_hex("chip_fg", text))?;
    let chip_fill = file
        .chip_bg
        .as_deref()
        .map_or(Ok(theme.accent), |text| parse_hex("chip_bg", text))?;
    theme.chip = Style::new().fg(chip_text).bg(chip_fill);
    Ok(theme)
}

fn load_user_theme(path: &Path) -> Result<Theme, String> {
    let fix = |problem: String| format!("fix {}: {problem}", path.display());
    let text = std::fs::read_to_string(path).map_err(|err| fix(err.to_string()))?;
    parse_theme_file(&text).map_err(fix)
}

impl Theme {
    pub fn github_dark() -> Self {
        let bg = Color::Rgb(0x0d, 0x11, 0x17);
        let accent = Color::Rgb(0x58, 0xa6, 0xff);
        Self {
            bg,
            panel: Color::Rgb(0x16, 0x1b, 0x22),
            // blue-tinted so the selected row stays legible against the dark bg
            cursor_line: Color::Rgb(0x26, 0x46, 0x6b),
            fg: Color::Rgb(0xe6, 0xed, 0xf3),
            dim: Color::Rgb(0x8b, 0x94, 0x9e),
            accent,
            purple: Color::Rgb(0xbc, 0x8c, 0xff),
            border: Color::Rgb(0x30, 0x36, 0x3d),
            del_line_bg: Color::Rgb(0x3c, 0x16, 0x18),
            add_line_bg: Color::Rgb(0x12, 0x35, 0x2a),
            del_emph_bg: Color::Rgb(0x8b, 0x2c, 0x2f),
            add_emph_bg: Color::Rgb(0x1f, 0x6f, 0x48),
            annotated: Color::Rgb(0x3d, 0x2e, 0x0c),
            search: Color::Rgb(0x6f, 0x5a, 0x0e),
            search_current: Color::Rgb(0xb0, 0x80, 0x00),
            added: Color::Rgb(0x3f, 0xb9, 0x50),
            warn_fg: Color::Rgb(0xd2, 0x99, 0x22),
            error_fg: Color::Rgb(0xf8, 0x51, 0x49),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::OneHalfDark,
        }
    }

    pub fn github_light() -> Self {
        let bg = Color::Rgb(0xff, 0xff, 0xff);
        let accent = Color::Rgb(0x09, 0x69, 0xda);
        Self {
            bg,
            panel: Color::Rgb(0xf6, 0xf8, 0xfa),
            cursor_line: Color::Rgb(0xb6, 0xe3, 0xff),
            fg: Color::Rgb(0x1f, 0x23, 0x28),
            dim: Color::Rgb(0x65, 0x6d, 0x76),
            accent,
            purple: Color::Rgb(0x82, 0x50, 0xdf),
            border: Color::Rgb(0xd0, 0xd7, 0xde),
            del_line_bg: Color::Rgb(0xff, 0xeb, 0xe9),
            add_line_bg: Color::Rgb(0xda, 0xfb, 0xe1),
            del_emph_bg: Color::Rgb(0xff, 0xc1, 0xbc),
            add_emph_bg: Color::Rgb(0xab, 0xf2, 0xbc),
            annotated: Color::Rgb(0xff, 0xf8, 0xc5),
            search: Color::Rgb(0xff, 0xf1, 0x7a),
            search_current: Color::Rgb(0xff, 0xb7, 0x00),
            added: Color::Rgb(0x1a, 0x7f, 0x37),
            warn_fg: Color::Rgb(0x9a, 0x67, 0x00),
            error_fg: Color::Rgb(0xcf, 0x22, 0x2e),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::OneHalfLight,
        }
    }

    pub fn dracula() -> Self {
        let bg = Color::Rgb(0x28, 0x2a, 0x36);
        let accent = Color::Rgb(0x8b, 0xe9, 0xfd);
        Self {
            bg,
            panel: Color::Rgb(0x21, 0x22, 0x2c),
            cursor_line: Color::Rgb(0x44, 0x47, 0x5a),
            fg: Color::Rgb(0xf8, 0xf8, 0xf2),
            dim: Color::Rgb(0x62, 0x72, 0xa4),
            accent,
            purple: Color::Rgb(0xbd, 0x93, 0xf9),
            border: Color::Rgb(0x44, 0x47, 0x5a),
            del_line_bg: Color::Rgb(0x44, 0x2a, 0x30),
            add_line_bg: Color::Rgb(0x22, 0x3a, 0x30),
            del_emph_bg: Color::Rgb(0x6e, 0x2a, 0x33),
            add_emph_bg: Color::Rgb(0x2c, 0x5a, 0x3e),
            annotated: Color::Rgb(0x44, 0x3a, 0x1f),
            search: Color::Rgb(0x57, 0x52, 0x1c),
            search_current: Color::Rgb(0x9a, 0x82, 0x2a),
            added: Color::Rgb(0x50, 0xfa, 0x7b),
            warn_fg: Color::Rgb(0xf1, 0xfa, 0x8c),
            error_fg: Color::Rgb(0xff, 0x55, 0x55),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::Dracula,
        }
    }

    pub fn catppuccin_mocha() -> Self {
        let bg = Color::Rgb(0x1e, 0x1e, 0x2e);
        let accent = Color::Rgb(0x89, 0xb4, 0xfa);
        Self {
            bg,
            panel: Color::Rgb(0x18, 0x18, 0x25),
            cursor_line: Color::Rgb(0x31, 0x32, 0x44),
            fg: Color::Rgb(0xcd, 0xd6, 0xf4),
            dim: Color::Rgb(0x7f, 0x84, 0x9c),
            accent,
            purple: Color::Rgb(0xcb, 0xa6, 0xf7),
            border: Color::Rgb(0x45, 0x47, 0x5a),
            del_line_bg: Color::Rgb(0x3a, 0x1e, 0x2a),
            add_line_bg: Color::Rgb(0x1e, 0x34, 0x28),
            del_emph_bg: Color::Rgb(0x5c, 0x2b, 0x3b),
            add_emph_bg: Color::Rgb(0x2d, 0x5a, 0x3c),
            annotated: Color::Rgb(0x45, 0x38, 0x18),
            search: Color::Rgb(0x5c, 0x4a, 0x14),
            search_current: Color::Rgb(0x9a, 0x78, 0x1e),
            added: Color::Rgb(0xa6, 0xe3, 0xa1),
            warn_fg: Color::Rgb(0xf9, 0xe2, 0xaf),
            error_fg: Color::Rgb(0xf3, 0x8b, 0xa8),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::CatppuccinMocha,
        }
    }

    pub fn tokyo_night() -> Self {
        let bg = Color::Rgb(0x1a, 0x1b, 0x26);
        let accent = Color::Rgb(0x7a, 0xa2, 0xf7);
        Self {
            bg,
            panel: Color::Rgb(0x16, 0x16, 0x1e),
            cursor_line: Color::Rgb(0x29, 0x2e, 0x42),
            fg: Color::Rgb(0xc0, 0xca, 0xf5),
            dim: Color::Rgb(0x56, 0x5f, 0x89),
            accent,
            purple: Color::Rgb(0xbb, 0x9a, 0xf7),
            border: Color::Rgb(0x3b, 0x42, 0x61),
            del_line_bg: Color::Rgb(0x39, 0x22, 0x2c),
            add_line_bg: Color::Rgb(0x1c, 0x2e, 0x24),
            del_emph_bg: Color::Rgb(0x5c, 0x33, 0x40),
            add_emph_bg: Color::Rgb(0x2c, 0x50, 0x38),
            annotated: Color::Rgb(0x3c, 0x34, 0x18),
            search: Color::Rgb(0x54, 0x48, 0x16),
            search_current: Color::Rgb(0x8f, 0x76, 0x1e),
            added: Color::Rgb(0x9e, 0xce, 0x6a),
            warn_fg: Color::Rgb(0xe0, 0xaf, 0x68),
            error_fg: Color::Rgb(0xf7, 0x76, 0x8e),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::TokyoNight,
        }
    }

    pub fn gruvbox_dark() -> Self {
        let bg = Color::Rgb(0x28, 0x28, 0x28);
        let accent = Color::Rgb(0x83, 0xa5, 0x98);
        Self {
            bg,
            panel: Color::Rgb(0x1d, 0x20, 0x21),
            cursor_line: Color::Rgb(0x3c, 0x38, 0x36),
            fg: Color::Rgb(0xeb, 0xdb, 0xb2),
            dim: Color::Rgb(0x92, 0x83, 0x74),
            accent,
            purple: Color::Rgb(0xd3, 0x86, 0x9b),
            border: Color::Rgb(0x50, 0x49, 0x45),
            del_line_bg: Color::Rgb(0x42, 0x20, 0x1c),
            add_line_bg: Color::Rgb(0x2c, 0x30, 0x18),
            del_emph_bg: Color::Rgb(0x6e, 0x30, 0x24),
            add_emph_bg: Color::Rgb(0x48, 0x50, 0x24),
            annotated: Color::Rgb(0x42, 0x36, 0x14),
            search: Color::Rgb(0x5e, 0x48, 0x10),
            search_current: Color::Rgb(0xa8, 0x7a, 0x18),
            added: Color::Rgb(0xb8, 0xbb, 0x26),
            warn_fg: Color::Rgb(0xfa, 0xbd, 0x2f),
            error_fg: Color::Rgb(0xfb, 0x49, 0x34),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::GruvboxDark,
        }
    }

    pub fn nord() -> Self {
        let bg = Color::Rgb(0x2e, 0x34, 0x40);
        let accent = Color::Rgb(0x88, 0xc0, 0xd0);
        Self {
            bg,
            panel: Color::Rgb(0x29, 0x2e, 0x39),
            cursor_line: Color::Rgb(0x3b, 0x42, 0x52),
            fg: Color::Rgb(0xd8, 0xde, 0xe9),
            dim: Color::Rgb(0x7b, 0x88, 0xa1),
            accent,
            purple: Color::Rgb(0xb4, 0x8e, 0xad),
            border: Color::Rgb(0x43, 0x4c, 0x5e),
            del_line_bg: Color::Rgb(0x3f, 0x2c, 0x30),
            add_line_bg: Color::Rgb(0x30, 0x3a, 0x33),
            del_emph_bg: Color::Rgb(0x5e, 0x3c, 0x42),
            add_emph_bg: Color::Rgb(0x4a, 0x5a, 0x42),
            annotated: Color::Rgb(0x45, 0x3f, 0x24),
            search: Color::Rgb(0x5c, 0x52, 0x22),
            search_current: Color::Rgb(0x9a, 0x86, 0x30),
            added: Color::Rgb(0xa3, 0xbe, 0x8c),
            warn_fg: Color::Rgb(0xeb, 0xcb, 0x8b),
            error_fg: Color::Rgb(0xbf, 0x61, 0x6a),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::Nord,
        }
    }

    pub fn rose_pine() -> Self {
        let bg = Color::Rgb(0x19, 0x17, 0x24);
        let accent = Color::Rgb(0x9c, 0xcf, 0xd8);
        Self {
            bg,
            panel: Color::Rgb(0x1f, 0x1d, 0x2e),
            cursor_line: Color::Rgb(0x26, 0x23, 0x3a),
            fg: Color::Rgb(0xe0, 0xde, 0xf4),
            dim: Color::Rgb(0x6e, 0x6a, 0x86),
            accent,
            purple: Color::Rgb(0xc4, 0xa7, 0xe7),
            border: Color::Rgb(0x40, 0x3d, 0x52),
            del_line_bg: Color::Rgb(0x3a, 0x22, 0x2c),
            add_line_bg: Color::Rgb(0x1c, 0x2e, 0x2a),
            del_emph_bg: Color::Rgb(0x5e, 0x32, 0x42),
            add_emph_bg: Color::Rgb(0x2e, 0x4e, 0x48),
            annotated: Color::Rgb(0x43, 0x36, 0x1c),
            search: Color::Rgb(0x5a, 0x46, 0x1a),
            search_current: Color::Rgb(0x96, 0x74, 0x26),
            added: Color::Rgb(0x9c, 0xcf, 0xd8),
            warn_fg: Color::Rgb(0xf6, 0xc1, 0x77),
            error_fg: Color::Rgb(0xeb, 0x6f, 0x92),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::RosePine,
        }
    }

    pub fn kanagawa() -> Self {
        let bg = Color::Rgb(0x1f, 0x1f, 0x28);
        let accent = Color::Rgb(0x7e, 0x9c, 0xd8);
        Self {
            bg,
            panel: Color::Rgb(0x16, 0x16, 0x1d),
            cursor_line: Color::Rgb(0x36, 0x36, 0x46),
            fg: Color::Rgb(0xdc, 0xd7, 0xba),
            dim: Color::Rgb(0x72, 0x71, 0x69),
            accent,
            purple: Color::Rgb(0x95, 0x7f, 0xb8),
            border: Color::Rgb(0x54, 0x54, 0x6d),
            del_line_bg: Color::Rgb(0x3a, 0x22, 0x24),
            add_line_bg: Color::Rgb(0x24, 0x2e, 0x24),
            del_emph_bg: Color::Rgb(0x5e, 0x32, 0x34),
            add_emph_bg: Color::Rgb(0x3a, 0x50, 0x36),
            annotated: Color::Rgb(0x42, 0x3a, 0x1c),
            search: Color::Rgb(0x59, 0x4c, 0x1a),
            search_current: Color::Rgb(0x95, 0x7d, 0x26),
            added: Color::Rgb(0x98, 0xbb, 0x6c),
            warn_fg: Color::Rgb(0xe6, 0xc3, 0x84),
            error_fg: Color::Rgb(0xff, 0x5d, 0x62),
            chip: Style::new().fg(bg).bg(accent),
            syntax: SyntaxTheme::Kanagawa,
        }
    }

    /// Look up a theme by config name, built-in or a file in the user's
    /// themes directory. A bad or unknown one falls back to github-dark with
    /// a warning for the status bar.
    pub fn from_name(name: &str) -> (Self, Option<String>) {
        Self::from_name_in(config::themes_dir().as_deref(), name)
    }

    fn from_name_in(dir: Option<&Path>, name: &str) -> (Self, Option<String>) {
        if let Some(theme) = builtin(name) {
            return (theme, None);
        }
        let fallback = (DEFAULT_BUILTIN.1)();
        let (Some(dir), true) = (dir, user_names(dir).iter().any(|user| user == name)) else {
            let warning = format!(
                "unknown theme \"{name}\", using {DEFAULT} (try: {})",
                names_in(dir).join(", ")
            );
            return (fallback, Some(warning));
        };
        match load_user_theme(&dir.join(format!("{name}.toml"))) {
            Ok(theme) => (theme, None),
            Err(warning) => (fallback, Some(warning)),
        }
    }

    /// Default text style: theme foreground over the full-screen background.
    pub fn base(&self) -> Style {
        Style::new().fg(self.fg).bg(self.bg)
    }

    pub fn dim_style(&self) -> Style {
        Style::new().fg(self.dim).bg(self.bg)
    }
}

/// `from` moved `percent` of the way toward `to`. Terminal-palette colours
/// carry no components to mix, so they pass through unchanged.
pub fn blend(from: Color, to: Color, percent: u16) -> Color {
    let (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) = (from, to) else {
        return from;
    };
    let mix =
        |f: u8, t: u8| ((u16::from(f) * (100 - percent) + u16::from(t) * percent) / 100) as u8;
    Color::Rgb(mix(fr, tr), mix(fg, tg), mix(fb, tb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_name_resolves_without_warning() {
        let (theme, warning) = Theme::from_name("github-dark");
        assert_eq!(theme, Theme::github_dark());
        assert_eq!(warning, None);
    }

    #[test]
    fn unknown_name_falls_back_with_warning() {
        let (theme, warning) = Theme::from_name("nonesuch");
        assert_eq!(theme, Theme::github_dark());
        let warning = warning.expect("warning");
        assert!(warning.contains("nonesuch"));
        assert!(warning.contains("github-dark"));
    }

    #[test]
    fn every_listed_name_resolves_without_warning() {
        for name in builtin_names() {
            let (_, warning) = Theme::from_name(name);
            assert_eq!(warning, None, "{name} should be a known theme");
        }
    }

    #[test]
    fn every_theme_mixes_in_rgb_so_blending_reaches_it() {
        for name in builtin_names() {
            let theme = Theme::from_name(name).0;
            for (label, color) in [("bg", theme.bg), ("panel", theme.panel), ("fg", theme.fg)] {
                assert!(
                    matches!(color, Color::Rgb(..)),
                    "{name} {label} must be RGB for blend to reach it"
                );
            }
        }
    }

    #[test]
    fn each_theme_pairs_a_distinct_syntax_palette() {
        let syntaxes: std::collections::HashSet<SyntaxTheme> = builtin_names()
            .map(|name| Theme::from_name(name).0.syntax)
            .collect();
        assert_eq!(
            syntaxes.len(),
            BUILTINS.len(),
            "each theme has its own palette"
        );
    }

    fn themes_dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).expect("write theme");
        }
        dir
    }

    #[test]
    fn a_partial_file_extends_the_default_and_overrides_only_what_it_sets() {
        let dir = themes_dir_with(&[("mine.toml", "accent = \"#ff8800\"\n")]);
        let (theme, warning) = Theme::from_name_in(Some(dir.path()), "mine");
        assert_eq!(warning, None);
        assert_eq!(theme.accent, Color::Rgb(0xff, 0x88, 0x00));
        assert_eq!(theme.chip.bg, Some(theme.accent));
        assert_eq!(theme.bg, Theme::github_dark().bg);
    }

    #[test]
    fn extends_and_syntax_pick_other_built_ins() {
        let dir = themes_dir_with(&[(
            "mine.toml",
            "extends = \"dracula\"\nsyntax = \"nord\"\nchip_fg = \"#000000\"\n",
        )]);
        let (theme, warning) = Theme::from_name_in(Some(dir.path()), "mine");
        assert_eq!(warning, None);
        assert_eq!(theme.bg, Theme::dracula().bg);
        assert_eq!(theme.syntax, SyntaxTheme::Nord);
        assert_eq!(theme.chip.fg, Some(Color::Rgb(0, 0, 0)));
    }

    #[test]
    fn a_bad_file_falls_back_with_a_warning_naming_the_file_and_problem() {
        for (text, problem) in [
            ("accent = \"orange\"", "bad colour for accent"),
            ("accent = \"#12345\"", "bad colour for accent"),
            ("accent = \"#€€\"", "bad colour for accent"),
            ("accent = \"#+f+f+f\"", "bad colour for accent"),
            ("extends = \"nonesuch\"", "unknown extends \"nonesuch\""),
            ("syntax = \"nonesuch\"", "unknown syntax \"nonesuch\""),
            ("colour = \"#ffffff\"", "unknown field"),
            ("accent = ", "expected"),
        ] {
            let dir = themes_dir_with(&[("bad.toml", text)]);
            let (theme, warning) = Theme::from_name_in(Some(dir.path()), "bad");
            assert_eq!(theme, Theme::github_dark(), "{text}");
            let warning = warning.expect("warning");
            assert!(warning.starts_with("fix "), "{warning}");
            assert!(warning.contains("bad.toml"), "{warning}");
            assert!(warning.contains(problem), "{warning}");
        }
    }

    #[test]
    fn user_themes_list_after_the_built_ins_and_never_shadow_one() {
        let dir = themes_dir_with(&[
            ("zeta.toml", ""),
            ("alpha.toml", ""),
            ("nord.toml", ""),
            ("notes.txt", ""),
        ]);
        let names = names_in(Some(dir.path()));
        let built_in = builtin_names().count();
        assert_eq!(names[..built_in], builtin_names().collect::<Vec<_>>()[..]);
        assert_eq!(names[built_in..], ["alpha", "zeta"]);
    }

    #[test]
    fn the_documented_example_theme_parses() {
        let text = include_str!("../../../docs/themes/example.toml");
        assert_eq!(
            parse_theme_file(text).map(|theme| theme.bg),
            Ok(Color::Rgb(0x0f, 0x11, 0x1a))
        );
    }

    #[test]
    fn a_name_outside_the_themes_directory_is_unknown() {
        let dir = themes_dir_with(&[("mine.toml", "")]);
        let (_, warning) = Theme::from_name_in(Some(dir.path()), "../mine");
        assert!(warning.expect("warning").contains("unknown theme"));
    }
}
