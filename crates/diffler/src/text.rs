//! Text shaping shared across the UI and the graph engine, both of which
//! truncate author-controlled strings into fixed terminal-column budgets.

use unicode_width::UnicodeWidthChar;

/// Truncate `s` to `max` display columns, trailing it with `…` when it does
/// not fit. `max` counts terminal cells, not characters, so a wide glyph
/// (CJK, most emoji) counts twice and the cut never lands mid-glyph.
pub fn elide(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let total: usize = s.chars().filter_map(UnicodeWidthChar::width).sum();
    if total <= max {
        return s.to_owned();
    }
    let budget = max - 1;
    let mut kept = String::new();
    let mut width = 0;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if width + w > budget {
            break;
        }
        kept.push(ch);
        width += w;
    }
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::elide;

    #[test]
    fn an_ascii_string_under_budget_is_untouched() {
        assert_eq!(elide("hello", 10), "hello");
    }

    #[test]
    fn an_ascii_string_over_budget_is_cut_and_marked() {
        assert_eq!(elide("hello world", 8), "hello w…");
    }

    #[test]
    fn a_wide_glyph_string_is_cut_by_display_width_not_char_count() {
        // "你好世界" is 4 chars but 8 terminal columns; a 5-column budget
        // (4 for content, 1 for the ellipsis) fits two wide glyphs, not four
        assert_eq!(elide("你好世界", 5), "你好…");
    }

    #[test]
    fn a_wide_glyph_under_budget_is_untouched() {
        assert_eq!(elide("你好", 4), "你好");
    }

    #[test]
    fn zero_budget_elides_to_nothing() {
        assert_eq!(elide("anything", 0), "");
    }
}
