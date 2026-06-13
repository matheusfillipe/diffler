//! Color theme. GitHub-dark is the only built-in theme; the `ui.theme`
//! config key exists so more can land without a config break.

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
    /// Diffstat added foreground (a readable green for `+N` counts). Deletes
    /// reuse `error_fg`, the same red the status accent uses for them.
    pub added: Color,
    /// Status-bar message severities (GitHub-dark attention/danger).
    pub warn_fg: Color,
    pub error_fg: Color,
    /// Status-bar mode chip (e.g. ` STATUS `).
    pub chip: Style,
}

impl Theme {
    pub fn github_dark() -> Self {
        let bg = Color::Rgb(0x0d, 0x11, 0x17);
        let accent = Color::Rgb(0x58, 0xa6, 0xff);
        Self {
            bg,
            panel: Color::Rgb(0x16, 0x1b, 0x22),
            cursor_line: Color::Rgb(0x21, 0x26, 0x2d),
            fg: Color::Rgb(0xe6, 0xed, 0xf3),
            dim: Color::Rgb(0x8b, 0x94, 0x9e),
            accent,
            purple: Color::Rgb(0xbc, 0x8c, 0xff),
            border: Color::Rgb(0x30, 0x36, 0x3d),
            del_line_bg: Color::Rgb(0x3c, 0x16, 0x18),
            add_line_bg: Color::Rgb(0x12, 0x35, 0x2a),
            del_emph_bg: Color::Rgb(0x8b, 0x2c, 0x2f),
            add_emph_bg: Color::Rgb(0x1f, 0x6f, 0x48),
            added: Color::Rgb(0x3f, 0xb9, 0x50),
            warn_fg: Color::Rgb(0xd2, 0x99, 0x22),
            error_fg: Color::Rgb(0xf8, 0x51, 0x49),
            chip: Style::new().fg(bg).bg(accent).add_modifier(Modifier::BOLD),
        }
    }

    /// Look up a theme by config name. Unknown names fall back to
    /// github-dark with a warning for the status bar.
    pub fn from_name(name: &str) -> (Self, Option<String>) {
        match name {
            "github-dark" => (Self::github_dark(), None),
            other => (
                Self::github_dark(),
                Some(format!("unknown theme \"{other}\", using github-dark")),
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
        let (theme, warning) = Theme::from_name("solarized");
        assert_eq!(theme, Theme::github_dark());
        let warning = warning.expect("warning");
        assert!(warning.contains("solarized"));
        assert!(warning.contains("github-dark"));
    }
}
