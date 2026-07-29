//! Color themes. `github-dark` is the default; `ui.theme` selects a built-in,
//! each pairing its palette with a matching syntax-highlight theme.

use diffler_core::highlight::SyntaxTheme;
use ratatui::style::{Color, Modifier, Style};

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

pub const NAMES: &[&str] = &[
    "github-dark",
    "catppuccin-mocha",
    "tokyo-night",
    "gruvbox-dark",
    "nord",
    "rose-pine",
    "kanagawa",
    "dracula",
    "github-light",
];

pub fn names() -> Vec<String> {
    NAMES.iter().map(|s| (*s).to_owned()).collect()
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
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
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
            syntax: SyntaxTheme::Kanagawa,
        }
    }

    /// Look up a theme by config name. Unknown names fall back to
    /// github-dark with a warning for the status bar.
    pub fn from_name(name: &str) -> (Self, Option<String>) {
        match name {
            "github-dark" => (Self::github_dark(), None),
            "github-light" => (Self::github_light(), None),
            "dracula" => (Self::dracula(), None),
            "catppuccin-mocha" => (Self::catppuccin_mocha(), None),
            "tokyo-night" => (Self::tokyo_night(), None),
            "gruvbox-dark" => (Self::gruvbox_dark(), None),
            "nord" => (Self::nord(), None),
            "rose-pine" => (Self::rose_pine(), None),
            "kanagawa" => (Self::kanagawa(), None),
            other => (
                Self::github_dark(),
                Some(format!(
                    "unknown theme \"{other}\", using github-dark (try: {})",
                    NAMES.join(", ")
                )),
            ),
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
        for name in NAMES {
            let (_, warning) = Theme::from_name(name);
            assert_eq!(warning, None, "{name} should be a known theme");
        }
    }

    #[test]
    fn each_theme_pairs_a_distinct_syntax_palette() {
        let syntaxes: std::collections::HashSet<SyntaxTheme> = NAMES
            .iter()
            .map(|name| Theme::from_name(name).0.syntax)
            .collect();
        assert_eq!(
            syntaxes.len(),
            NAMES.len(),
            "each theme has its own palette"
        );
    }
}
