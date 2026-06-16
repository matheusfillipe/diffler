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

    /// Look up a theme by config name. Unknown names fall back to
    /// github-dark with a warning for the status bar.
    pub fn from_name(name: &str) -> (Self, Option<String>) {
        match name {
            "github-dark" => (Self::github_dark(), None),
            "github-light" => (Self::github_light(), None),
            "dracula" => (Self::dracula(), None),
            other => (
                Self::github_dark(),
                Some(format!(
                    "unknown theme \"{other}\", using github-dark (try github-light, dracula)"
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
    fn built_in_themes_resolve_without_warning() {
        for name in ["github-dark", "github-light", "dracula"] {
            let (_, warning) = Theme::from_name(name);
            assert_eq!(warning, None, "{name} should be a known theme");
        }
    }

    #[test]
    fn each_theme_pairs_its_syntax_palette() {
        assert_eq!(Theme::github_dark().syntax, SyntaxTheme::OneHalfDark);
        assert_eq!(Theme::github_light().syntax, SyntaxTheme::OneHalfLight);
        assert_eq!(Theme::dracula().syntax, SyntaxTheme::Dracula);
    }
}
