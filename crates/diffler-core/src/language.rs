//! What language a path is written in, the colour GitHub paints it, and the
//! comment syntax the line counter needs.
//!
//! Detection is a pure path function: the syntax registry already maps every
//! extension it can highlight to a grammar, so that mapping stays the one
//! source of truth and this module adds the languages diffler counts but does
//! not highlight. Colours are Linguist's own hexes, the ones a reader knows
//! from a repository page, lifted toward the foreground when the terminal's
//! background would swallow them.

use crate::syntax::registry::REGISTRY;

pub type Rgb = (u8, u8, u8);

/// A language diffler can name, with everything the breakdown needs about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// Display label, spelled the way a repository page spells it.
    pub name: &'static str,
    /// Linguist's colour for the language, untouched.
    pub color: Rgb,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
}

impl Language {
    /// Whether `line`, already trimmed, opens a comment that ends on the same
    /// line, and what remains open after it.
    fn classify(self, line: &str, in_block: bool) -> (LineKind, bool) {
        if in_block {
            let closed = self
                .block_comment
                .is_some_and(|(_, end)| line.contains(end));
            return (LineKind::Comment, !closed);
        }
        // the block opener is tested first because it can start with the line
        // token itself: Lua's `--[[` opens a block, `--` only a line
        if let Some((start, end)) = self.block_comment
            && line.starts_with(start)
        {
            let closed = line[start.len()..].contains(end);
            return (LineKind::Comment, !closed);
        }
        if self
            .line_comments
            .iter()
            .any(|token| line.starts_with(token))
        {
            return (LineKind::Comment, false);
        }
        (LineKind::Code, false)
    }
}

enum LineKind {
    Code,
    Comment,
}

/// The `(code, comments, blanks)` a source text holds. A line counts as a
/// comment when it opens with one, so a trailing `// note` after code reads as
/// code, the same call `scc` and `cloc` make.
#[must_use]
pub fn count_lines(text: &str, language: Option<Language>) -> (usize, usize, usize) {
    let (mut code, mut comments, mut blanks) = (0, 0, 0);
    let mut in_block = false;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            blanks += 1;
            continue;
        }
        // a shebang is the file's first instruction, and `#` would otherwise
        // swallow it
        if index == 0 && line.starts_with("#!") {
            code += 1;
            continue;
        }
        let Some(language) = language else {
            code += 1;
            continue;
        };
        let (kind, still_open) = language.classify(line, in_block);
        in_block = still_open;
        match kind {
            LineKind::Code => code += 1,
            LineKind::Comment => comments += 1,
        }
    }
    (code, comments, blanks)
}

/// The language of `path`, by extension or by whole filename.
#[must_use]
pub fn of_path(path: &str) -> Option<Language> {
    // the highlighter's registry owns the extension table for everything it can
    // parse; only what it cannot appears in EXTRA below
    if let Some(entry) = REGISTRY.for_path(path)
        && let Some(language) = by_key(entry.name)
    {
        return Some(language);
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if let Some(key) = EXTRA_FILENAMES
        .iter()
        .find_map(|(filename, key)| (*filename == name).then_some(key))
    {
        return by_key(key);
    }
    if let Some(extension) = name.rsplit_once('.').map(|(_, ext)| ext)
        && let Some(key) = EXTRA_EXTENSIONS
            .iter()
            .find_map(|(ext, key)| (*ext == extension).then_some(key))
    {
        return by_key(key);
    }
    let key = EXTRA_PREFIXES
        .iter()
        .find_map(|(prefix, key)| name.starts_with(prefix).then_some(key))?;
    by_key(key)
}

fn by_key(key: &str) -> Option<Language> {
    TABLE
        .iter()
        .find_map(|(name, language)| (*name == key).then_some(*language))
}

/// Lift `color` until it separates from `bg`. Linguist's palette is tuned for
/// a white page, so a few entries (JSON's `#292929`, C's `#555555`) vanish on a
/// dark terminal and a few of the bright ones wash out on a light one.
#[must_use]
pub fn readable_on(color: Rgb, bg: Rgb) -> Rgb {
    const TARGET: f32 = 3.0;
    let toward = if luminance(bg) > 0.5 { 0 } else { 255 };
    let mut out = color;
    // each step moves a tenth of what is left, and never by less than one
    // channel value, so this reaches the end of the ramp and stops
    for _ in 0..40 {
        if contrast(out, bg) >= TARGET {
            break;
        }
        out = step_toward(out, toward);
    }
    out
}

fn step_toward(color: Rgb, toward: u8) -> Rgb {
    let blend = |channel: u8| {
        let (from, to) = (i32::from(channel), i32::from(toward));
        let by = (to - from) / 10;
        let stepped = from + if by == 0 { (to - from).signum() } else { by };
        u8::try_from(stepped.clamp(0, 255)).unwrap_or(toward)
    };
    (blend(color.0), blend(color.1), blend(color.2))
}

fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (high, low) = {
        let (la, lb) = (luminance(a), luminance(b));
        if la > lb { (la, lb) } else { (lb, la) }
    };
    (high + 0.05) / (low + 0.05)
}

/// WCAG relative luminance.
fn luminance(color: Rgb) -> f32 {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.0) + 0.7152 * channel(color.1) + 0.0722 * channel(color.2)
}

/// Everything the sidebar's grammar names map to, plus the languages diffler
/// counts without highlighting. Keys match `syntax::registry` names where a
/// grammar exists; colours are Linguist's.
const TABLE: &[(&str, Language)] = &[
    lang(
        "rust",
        "Rust",
        (0xde, 0xa5, 0x84),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "python",
        "Python",
        (0x35, 0x72, 0xa5),
        &["#"],
        // a docstring is the module's or function's comment, and both fences
        // are the same token, which `classify` handles by looking past the
        // opening one
        Some(("\"\"\"", "\"\"\"")),
    ),
    lang(
        "javascript",
        "JavaScript",
        (0xf1, 0xe0, 0x5a),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "typescript",
        "TypeScript",
        (0x31, 0x78, 0xc6),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "tsx",
        "TSX",
        (0x31, 0x78, 0xc6),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang("go", "Go", (0x00, 0xad, 0xd8), &["//"], Some(("/*", "*/"))),
    lang("c", "C", (0x55, 0x55, 0x55), &["//"], Some(("/*", "*/"))),
    lang(
        "cpp",
        "C++",
        (0xf3, 0x4b, 0x7d),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "java",
        "Java",
        (0xb0, 0x72, 0x19),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "c-sharp",
        "C#",
        (0x17, 0x86, 0x00),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "ruby",
        "Ruby",
        (0x70, 0x15, 0x16),
        &["#"],
        Some(("=begin", "=end")),
    ),
    lang(
        "php",
        "PHP",
        (0x4f, 0x5d, 0x95),
        &["//", "#"],
        Some(("/*", "*/")),
    ),
    lang("bash", "Shell", (0x89, 0xe0, 0x51), &["#"], None),
    lang("json", "JSON", (0x29, 0x29, 0x29), &[], None),
    lang(
        "html",
        "HTML",
        (0xe3, 0x4c, 0x26),
        &[],
        Some(("<!--", "-->")),
    ),
    lang("css", "CSS", (0x66, 0x33, 0x99), &[], Some(("/*", "*/"))),
    lang("yaml", "YAML", (0xcb, 0x17, 0x1e), &["#"], None),
    lang(
        "sql",
        "SQL",
        (0xe3, 0x8c, 0x00),
        &["--"],
        Some(("/*", "*/")),
    ),
    lang(
        "markdown",
        "Markdown",
        (0x08, 0x3f, 0xa1),
        &[],
        Some(("<!--", "-->")),
    ),
    lang("toml", "TOML", (0x9c, 0x42, 0x21), &["#"], None),
    lang(
        "hcl",
        "HCL",
        (0x84, 0x4f, 0xba),
        &["#", "//"],
        Some(("/*", "*/")),
    ),
    lang("dockerfile", "Dockerfile", (0x38, 0x4d, 0x54), &["#"], None),
    lang("make", "Makefile", (0x42, 0x78, 0x19), &["#"], None),
    lang(
        "lua",
        "Lua",
        (0x00, 0x00, 0x80),
        &["--"],
        Some(("--[[", "]]")),
    ),
    lang("nix", "Nix", (0x7e, 0x7e, 0xff), &["#"], Some(("/*", "*/"))),
    lang("xml", "XML", (0x00, 0x60, 0xac), &[], Some(("<!--", "-->"))),
    lang(
        "swift",
        "Swift",
        (0xf0, 0x51, 0x38),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "scala",
        "Scala",
        (0xc2, 0x2d, 0x40),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "elixir",
        "Elixir",
        (0x6e, 0x4a, 0x7e),
        &["#"],
        Some(("@doc \"\"\"", "\"\"\"")),
    ),
    lang("zig", "Zig", (0xec, 0x91, 0x5c), &["//"], None),
    lang(
        "haskell",
        "Haskell",
        (0x5e, 0x50, 0x86),
        &["--"],
        Some(("{-", "-}")),
    ),
    lang(
        "dart",
        "Dart",
        (0x00, 0xb4, 0xab),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang(
        "powershell",
        "PowerShell",
        (0x01, 0x24, 0x56),
        &["#"],
        Some(("<#", "#>")),
    ),
    lang(
        "svelte",
        "Svelte",
        (0xff, 0x3e, 0x00),
        &[],
        Some(("<!--", "-->")),
    ),
    // no grammar bundled for these; EXTRA_* below routes paths to them
    lang(
        "kotlin",
        "Kotlin",
        (0xa9, 0x7b, 0xff),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang("vue", "Vue", (0x41, 0xb8, 0x83), &[], Some(("<!--", "-->"))),
    lang("r", "R", (0x19, 0x8c, 0xe7), &["#"], None),
    lang("perl", "Perl", (0x02, 0x98, 0xc3), &["#"], None),
    lang(
        "protobuf",
        "Protocol Buffer",
        (0xe3, 0xc5, 0x8e),
        &["//"],
        Some(("/*", "*/")),
    ),
    lang("ini", "INI", (0xd1, 0xdb, 0xe0), &[";", "#"], None),
    lang("csv", "CSV", (0x23, 0x73, 0x46), &[], None),
    lang(
        "scheme",
        "Scheme",
        (0x1e, 0x4a, 0xec),
        &[";"],
        Some(("#|", "|#")),
    ),
    lang("text", "Text", (0x8b, 0x94, 0x9e), &[], None),
];

/// Extensions the highlighter has no grammar for.
const EXTRA_EXTENSIONS: &[(&str, &str)] = &[
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("vue", "vue"),
    ("r", "r"),
    ("pl", "perl"),
    ("pm", "perl"),
    ("proto", "protobuf"),
    ("ini", "ini"),
    ("cfg", "ini"),
    ("conf", "ini"),
    ("properties", "ini"),
    ("csv", "csv"),
    ("tsv", "csv"),
    ("txt", "text"),
    ("scm", "scheme"),
    ("ss", "scheme"),
];

const EXTRA_FILENAMES: &[(&str, &str)] = &[
    (".gitignore", "ini"),
    (".gitattributes", "ini"),
    (".editorconfig", "ini"),
];

/// Filenames that carry their language in a prefix, consulted after the
/// extensions so `LICENSE.md` stays Markdown: a repo ships `LICENSE-APACHE`
/// beside `LICENSE-MIT`, and both are prose.
const EXTRA_PREFIXES: &[(&str, &str)] = &[
    ("license", "text"),
    ("licence", "text"),
    ("copying", "text"),
    ("notice", "text"),
];

const fn lang(
    key: &'static str,
    name: &'static str,
    color: Rgb,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
) -> (&'static str, Language) {
    (
        key,
        Language {
            name,
            color,
            line_comments,
            block_comment,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_resolve_through_the_highlighters_own_table() {
        assert_eq!(of_path("src/main.rs").map(|l| l.name), Some("Rust"));
        assert_eq!(of_path("a/b/setup.py").map(|l| l.name), Some("Python"));
        // the registry maps sh, bash and zsh to one grammar; all read as Shell
        assert_eq!(of_path("scripts/release.sh").map(|l| l.name), Some("Shell"));
        assert_eq!(of_path("infra/main.tf").map(|l| l.name), Some("HCL"));
        assert_eq!(of_path("Makefile").map(|l| l.name), Some("Makefile"));
    }

    #[test]
    fn a_license_is_prose_whatever_it_is_suffixed_with() {
        assert_eq!(of_path("LICENSE-APACHE").map(|l| l.name), Some("Text"));
        assert_eq!(of_path("LICENSE-MIT").map(|l| l.name), Some("Text"));
        // the extension still wins, so a markdown licence stays markdown
        assert_eq!(of_path("LICENSE.md").map(|l| l.name), Some("Markdown"));
    }

    #[test]
    fn a_python_docstring_counts_as_a_comment() {
        let python = of_path("x.py").expect("python");
        let source = "\"\"\"What this does.\n\nAnd why.\n\"\"\"\nimport os\n";
        assert_eq!(count_lines(source, Some(python)), (1, 3, 1));
        // one line holding both fences opens nothing
        let inline = "def f():\n    \"\"\"Note.\"\"\"\n    return 1\n";
        assert_eq!(count_lines(inline, Some(python)), (2, 1, 0));
    }

    #[test]
    fn languages_without_a_grammar_still_resolve() {
        assert_eq!(of_path("app/Main.kt").map(|l| l.name), Some("Kotlin"));
        assert_eq!(
            of_path("api/user.proto").map(|l| l.name),
            Some("Protocol Buffer")
        );
        assert_eq!(of_path("LICENSE").map(|l| l.name), Some("Text"));
        assert_eq!(of_path("deploy/app.ini").map(|l| l.name), Some("INI"));
    }

    #[test]
    fn an_unknown_path_has_no_language() {
        assert_eq!(of_path("data.bin"), None);
        assert_eq!(of_path("no-extension-here"), None);
    }

    #[test]
    fn counting_splits_code_comments_and_blanks() {
        let rust = of_path("x.rs").expect("rust");
        let source = "fn main() {\n\n    // why\n    let x = 1; // trailing\n}\n";
        assert_eq!(count_lines(source, Some(rust)), (3, 1, 1));
    }

    #[test]
    fn a_block_comment_runs_until_its_end_token() {
        let rust = of_path("x.rs").expect("rust");
        let source = "/* one\n   two\n   three */\nfn main() {}\n";
        assert_eq!(count_lines(source, Some(rust)), (1, 3, 0));
    }

    #[test]
    fn a_block_that_opens_and_closes_on_one_line_leaves_nothing_open() {
        let rust = of_path("x.rs").expect("rust");
        let source = "/* note */\nfn main() {}\nlet y = 2;\n";
        assert_eq!(count_lines(source, Some(rust)), (2, 1, 0));
    }

    #[test]
    fn a_shebang_counts_as_code_even_where_its_token_opens_comments() {
        let shell = of_path("run.sh").expect("shell");
        assert_eq!(
            count_lines("#!/bin/sh\n# note\necho hi\n", Some(shell)),
            (2, 1, 0)
        );
    }

    /// Lua opens a block with `--[[` and a line with `--`, so the longer token
    /// has to be tried first or every block reads as one line comment.
    #[test]
    fn a_block_opener_that_starts_with_the_line_token_still_opens_a_block() {
        let lua = of_path("init.lua").expect("lua");
        let source = "--[[\n  long note\n  more\n]]\nprint(1)\n";
        assert_eq!(count_lines(source, Some(lua)), (1, 4, 0));
        // a plain line comment still ends at its own line
        assert_eq!(count_lines("-- note\nprint(1)\n", Some(lua)), (1, 1, 0));
    }

    #[test]
    fn an_unknown_language_counts_every_filled_line_as_code() {
        assert_eq!(count_lines("a\n\nb\n", None), (2, 0, 1));
    }

    #[test]
    fn a_dark_ground_lifts_the_colours_that_would_vanish_on_it() {
        let terminal = (0x0d, 0x11, 0x17);
        let json = of_path("a.json").expect("json").color;
        assert_eq!(json, (0x29, 0x29, 0x29), "the table keeps Linguist's hex");
        let lifted = readable_on(json, terminal);
        assert!(
            luminance(lifted) > luminance(json),
            "JSON's near-black lifts off the terminal ground: {lifted:?}"
        );
        assert!(contrast(lifted, terminal) >= 3.0);
    }

    #[test]
    fn a_colour_that_already_reads_is_left_alone() {
        let terminal = (0x0d, 0x11, 0x17);
        let rust = of_path("a.rs").expect("rust").color;
        assert_eq!(readable_on(rust, terminal), rust);
    }

    #[test]
    fn a_light_ground_darkens_instead_of_lifting() {
        let page = (0xff, 0xff, 0xff);
        let shell = of_path("a.sh").expect("shell").color;
        let fitted = readable_on(shell, page);
        assert!(
            luminance(fitted) < luminance(shell),
            "Linguist's bright green has to darken for a light theme: {fitted:?}"
        );
        assert!(contrast(fitted, page) >= 3.0);
    }
}
