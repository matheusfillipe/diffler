//! Which bucket a changed file belongs to, from its path alone. The rule order
//! is the design: Generated outranks Tests so a generated fixture reads as
//! noise, and Build outranks Config so `Cargo.toml` reads as a manifest.
//!
//! [`Rules`] layers the two things a repo can say for itself over the built-in
//! table: the reader's own globs, then git's `linguist-*` attributes.

use std::path::Path;

use crate::syntax::registry::REGISTRY;

/// A sidebar bucket. The set is fixed across repos so the reader's muscle
/// memory carries between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Source,
    Tests,
    Docs,
    Config,
    Build,
    Generated,
    Assets,
    Other,
}

impl Kind {
    /// Display order in the sidebar: what the reader came to review first,
    /// what they came to skip last.
    pub const ALL: [Self; 8] = [
        Self::Source,
        Self::Tests,
        Self::Docs,
        Self::Config,
        Self::Build,
        Self::Generated,
        Self::Assets,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Source",
            Self::Tests => "Tests",
            Self::Docs => "Docs",
            Self::Config => "Config",
            Self::Build => "Build & CI",
            Self::Generated => "Generated",
            Self::Assets => "Assets",
            Self::Other => "Other",
        }
    }
}

/// The built-in table plus whatever the repo says for itself.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// Glob patterns per bucket, in the order they are consulted.
    overrides: Vec<(Kind, Vec<String>)>,
}

impl Rules {
    pub fn new(overrides: Vec<(Kind, Vec<String>)>) -> Self {
        Self { overrides }
    }

    /// The bucket for `path`. `declared` is what git's `linguist-*` attributes
    /// say, which the reader's own globs still outrank.
    pub fn kind(&self, path: &str, declared: Option<Kind>) -> Kind {
        for (kind, patterns) in &self.overrides {
            if patterns.iter().any(|pattern| glob_match(pattern, path)) {
                return *kind;
            }
        }
        declared.unwrap_or_else(|| classify(path))
    }
}

/// What a repo declares about a path through the `linguist-*` git attributes
/// forges already honour, given a reader for one attribute. Vendored code
/// joins Generated: both mean the reader did not write it.
pub fn declared(attr: impl Fn(&str) -> bool) -> Option<Kind> {
    if attr("linguist-generated") || attr("linguist-vendored") {
        Some(Kind::Generated)
    } else if attr("linguist-documentation") {
        Some(Kind::Docs)
    } else {
        None
    }
}

/// The built-in table: first match wins.
pub fn classify(path: &str) -> Kind {
    let lower = path.to_ascii_lowercase();
    let name = basename(&lower);
    let ext = extension(name);
    if generated(&lower, name, ext) {
        Kind::Generated
    } else if tests(&lower, name, ext, basename(path)) {
        Kind::Tests
    } else if build(&lower, name, ext) {
        Kind::Build
    } else if docs(&lower, name, ext) {
        Kind::Docs
    } else if config(name, ext) {
        Kind::Config
    } else if ASSET_EXTENSIONS.contains(&ext) {
        Kind::Assets
    } else if source(path, ext) {
        Kind::Source
    } else {
        Kind::Other
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Extension without the dot, empty for a file that has none. A dotfile with
/// no second dot (`.gitignore`) has no extension, matching how git names it.
fn extension(name: &str) -> &str {
    Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
}

fn has_segment(path: &str, wanted: &[&str]) -> bool {
    segments(path).any(|segment| wanted.contains(&segment))
}

/// Trees nobody wrote by hand: vendored dependencies, build output, and the
/// caches tools leave behind.
const GENERATED_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "third_party",
    "bower_components",
    "pods",
    "godeps",
    "__generated__",
    "__pycache__",
    "generated",
    "dist",
    "target",
    "htmlcov",
    ".sqlx",
    ".yarn",
    ".terraform",
];

/// Suffixes a code generator stamps on its output.
const GENERATED_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.css",
    ".pb.go",
    ".pb.cc",
    ".pb.h",
    "_pb2.py",
    "_pb2_grpc.py",
    "_generated.go",
    ".generated.cs",
    ".designer.cs",
    ".g.dart",
    ".freezed.dart",
    ".snap",
    ".map",
];

const GENERATED_FILES: &[&str] = &[
    "go.sum",
    "package.resolved",
    "npm-shrinkwrap.json",
    "packages.lock.json",
];

/// Every shape a dependency lockfile takes: an extension (`Cargo.lock`,
/// `bun.lockb`), an infix before another one (`pnpm-lock.yaml`,
/// `.terraform.lock.hcl`).
fn lockfile(name: &str, ext: &str) -> bool {
    matches!(ext, "lock" | "lockb") || name.contains("-lock.") || name.contains(".lock.")
}

fn generated(path: &str, name: &str, ext: &str) -> bool {
    has_segment(path, GENERATED_DIRS)
        || lockfile(name, ext)
        || GENERATED_FILES.contains(&name)
        || GENERATED_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

const TEST_DIRS: &[&str] = &[
    "test",
    "tests",
    "spec",
    "specs",
    "__tests__",
    "__mocks__",
    "testdata",
    "e2e",
    "cypress",
];

/// Affixes that name a test in the languages that have a convention. Checked
/// against the basename with its extension stripped, so one entry covers every
/// language sharing the affix. The separator is part of the affix: without it
/// `latest.rs` and `protest.rs` read as tests.
const TEST_AFFIXES: &[&str] = &["_test", "_tests", "_spec", "-test", ".test", ".spec"];

/// The camel-cased conventions, checked against the untouched basename: the
/// lowercased one cannot see the hump that makes `UserTest` a test and
/// `latest` a word.
const TEST_CAMEL_AFFIXES: &[&str] = &["Test", "Tests"];

/// `FooSpec` is scalatest, and only there: elsewhere the camel form names an
/// API contract, as in `OpenApiSpec.ts`.
const SPEC_EXTENSIONS: &[&str] = &["scala", "kt", "groovy"];

fn stem<'a>(name: &'a str, ext: &str) -> &'a str {
    name.strip_suffix(ext)
        .map_or(name, |rest| rest.trim_end_matches('.'))
}

fn tests(path: &str, name: &str, ext: &str, raw_name: &str) -> bool {
    if has_segment(path, TEST_DIRS) {
        return true;
    }
    let lower_stem = stem(name, ext);
    if lower_stem == "test" || lower_stem == "conftest" || lower_stem.starts_with("test_") {
        return true;
    }
    if TEST_AFFIXES.iter().any(|affix| lower_stem.ends_with(affix)) {
        return true;
    }
    let raw_stem = stem(raw_name, extension(raw_name));
    TEST_CAMEL_AFFIXES
        .iter()
        .any(|affix| raw_stem.ends_with(affix))
        || (SPEC_EXTENSIONS.contains(&ext) && raw_stem.ends_with("Spec"))
}

const CI_DIRS: &[&str] = &[".github", ".gitlab", ".forgejo", ".gitea", ".circleci"];

const BUILD_FILES: &[&str] = &[
    "cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "gemfile",
    "rakefile",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "cmakelists.txt",
    "makefile",
    "gnumakefile",
    "justfile",
    "meson.build",
    "build",
    "build.bazel",
    "workspace",
    "mix.exs",
    "build.rs",
    "jenkinsfile",
    "flake.nix",
    "shell.nix",
    "default.nix",
    "procfile",
    "taskfile.yml",
    ".gitlab-ci.yml",
    ".travis.yml",
    "azure-pipelines.yml",
];

fn build(path: &str, name: &str, ext: &str) -> bool {
    has_segment(path, CI_DIRS)
        || BUILD_FILES.contains(&name)
        || name.starts_with("dockerfile")
        || name.starts_with("docker-compose.")
        || name.starts_with("requirements") && ext == "txt"
        || ext == "nix"
}

const DOC_DIRS: &[&str] = &["docs", "doc", "man"];

const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "rst", "adoc", "org", "txt", "1"];

/// The files a repo keeps at its root with no extension at all. They are
/// matched only when the extension is empty: `src/security.rs` and
/// `models/license.rb` are code that happens to share the word.
pub(crate) const DOC_NAMES: &[&str] = &[
    "readme",
    "license",
    "licence",
    "copying",
    "changelog",
    "contributing",
    "authors",
    "notice",
    "security",
];

fn docs(path: &str, name: &str, ext: &str) -> bool {
    has_segment(path, DOC_DIRS)
        || DOC_EXTENSIONS.contains(&ext)
        || (ext.is_empty()
            && DOC_NAMES
                .iter()
                .any(|stem| name == *stem || name.starts_with(&format!("{stem}-"))))
}

const CONFIG_EXTENSIONS: &[&str] = &[
    "toml",
    "yaml",
    "yml",
    "json",
    "json5",
    "jsonc",
    "ini",
    "cfg",
    "conf",
    "properties",
    "env",
    "xml",
    "plist",
];

fn config(name: &str, ext: &str) -> bool {
    CONFIG_EXTENSIONS.contains(&ext) || (name.starts_with('.') && ext.is_empty())
}

const ASSET_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "ico", "webp", "avif", "bmp", "tiff", "woff", "woff2",
    "ttf", "otf", "eot", "mp3", "mp4", "wav", "ogg", "webm", "mov", "pdf", "zip", "gz", "tar",
    "bz2", "xz", "7z",
];

/// Code the bundled grammars do not cover: the registry answers for everything
/// diffler can highlight, this list keeps the rest out of Other.
const SOURCE_EXTENSIONS: &[&str] = &[
    "kt", "kts", "pl", "pm", "r", "jl", "erl", "hrl", "clj", "cljs", "cljc", "fs", "fsi", "fsx",
    "vb", "groovy", "proto", "graphql", "gql", "vue", "astro", "scss", "sass", "less", "styl",
    "coffee", "mm", "sol", "v", "vhd", "tcl", "f90",
];

fn source(path: &str, ext: &str) -> bool {
    SOURCE_EXTENSIONS.contains(&ext) || REGISTRY.for_path(path).is_some()
}

/// Gitignore-flavoured glob: `*` and `?` stay inside one path segment, `**`
/// spans any number of them, and a pattern with no `/` matches the basename at
/// any depth. The shapes a reader carries over from `.gitignore` are honoured
/// rather than silently matching nothing: a leading `/` is the anchoring a
/// pattern with a slash already has, and a trailing `/` names a directory's
/// whole subtree.
fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").trim_start_matches('/');
    if let Some(dir) = pattern.strip_suffix('/') {
        return glob_match(&format!("{dir}/**"), path);
    }
    if pattern.contains('/') {
        let pattern: Vec<&str> = segments(pattern).collect();
        let path: Vec<&str> = segments(path).collect();
        match_segments(&pattern, &path)
    } else {
        match_segment(pattern, basename(path))
    }
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    let Some((head, rest)) = pattern.split_first() else {
        return path.is_empty();
    };
    if *head == "**" {
        return (0..=path.len()).any(|skip| {
            path.get(skip..)
                .is_some_and(|tail| match_segments(rest, tail))
        });
    }
    match path.split_first() {
        Some((segment, tail)) if match_segment(head, segment) => match_segments(rest, tail),
        _ => false,
    }
}

/// Wildcard match within one segment, backtracking on `*` so `a*b*c` behaves.
fn match_segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut retry) = (None, 0usize);
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                retry = t;
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(c) if Some(c) == text.get(t) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some(at) => {
                    p = at + 1;
                    retry += 1;
                    t = retry;
                }
                None => return false,
            },
        }
    }
    pattern
        .get(p..)
        .is_none_or(|rest| rest.iter().all(|c| *c == '*'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn assert_kind(path: &str, expected: Kind) {
        assert_eq!(classify(path), expected, "{path}");
    }

    #[test]
    fn code_lands_in_source() {
        assert_kind("crates/diffler/src/ui/diff.rs", Kind::Source);
        assert_kind("app/main.py", Kind::Source);
        assert_kind("web/src/App.tsx", Kind::Source);
        assert_kind("cmd/server/main.go", Kind::Source);
        assert_kind("android/Main.kt", Kind::Source);
        assert_kind("api/schema.proto", Kind::Source);
        assert_kind("web/styles/app.scss", Kind::Source);
    }

    #[test]
    fn every_language_convention_for_a_test_is_recognised() {
        for path in [
            "tests/e2e/test_status.py",
            "src/foo_test.go",
            "src/foo_test.rs",
            "web/src/App.test.tsx",
            "web/src/App.spec.ts",
            "src/UserTest.java",
            "src/UserTests.cs",
            "spec/models/user_spec.rb",
            "test/support/helper.ex",
            "testdata/golden.json",
            "conftest.py",
            "cypress/e2e/login.cy.js",
        ] {
            assert_kind(path, Kind::Tests);
        }
    }

    #[test]
    fn a_word_that_ends_in_test_is_not_a_test() {
        assert_kind("src/latest.rs", Kind::Source);
        assert_kind("src/protest.rs", Kind::Source);
        assert_kind("src/contests.rs", Kind::Source);
        assert_kind("src/manifest.rs", Kind::Source);
        assert_kind("web/src/OpenApiSpec.ts", Kind::Source);
        assert_kind("src/main/scala/UserSpec.scala", Kind::Tests);
    }

    #[test]
    fn a_source_file_named_like_a_doc_stays_source() {
        assert_kind("src/security.rs", Kind::Source);
        assert_kind("app/models/license.rb", Kind::Source);
        assert_kind("pkg/changelog.go", Kind::Source);
        assert_kind("LICENSE-MIT", Kind::Docs);
        assert_kind("AUTHORS", Kind::Docs);
    }

    #[test]
    fn generated_output_and_lockfiles_are_one_bucket() {
        for path in [
            "Cargo.lock",
            "pnpm-lock.yaml",
            "go.sum",
            "node_modules/left-pad/index.js",
            "vendor/github.com/pkg/errors/errors.go",
            "api/service.pb.go",
            "api/service_pb2.py",
            "web/dist/bundle.min.js",
            "crates/diffler/src/ui/snapshots/a_pane.snap",
            "target/debug/build.rs",
        ] {
            assert_kind(path, Kind::Generated);
        }
    }

    #[test]
    fn generated_outranks_tests_so_a_snapshot_is_not_a_test() {
        assert_kind("tests/snapshots/render.snap", Kind::Generated);
        assert_kind("tests/fixtures/node_modules/dep/index.js", Kind::Generated);
    }

    #[test]
    fn manifests_and_pipelines_are_build() {
        for path in [
            "Cargo.toml",
            "package.json",
            "go.mod",
            "pyproject.toml",
            "Makefile",
            "justfile",
            "Dockerfile",
            "docker-compose.yml",
            ".github/workflows/ci.yml",
            ".forgejo/workflows/ci.yml",
            ".gitlab-ci.yml",
            "flake.nix",
            "requirements.txt",
            "crates/diffler/build.rs",
        ] {
            assert_kind(path, Kind::Build);
        }
    }

    #[test]
    fn prose_is_docs_wherever_it_sits() {
        assert_kind("README.md", Kind::Docs);
        assert_kind("LICENSE", Kind::Docs);
        assert_kind("CHANGELOG.md", Kind::Docs);
        assert_kind("docs/config.example.toml", Kind::Docs);
        assert_kind("notes/design.rst", Kind::Docs);
    }

    #[test]
    fn settings_are_config_and_binaries_are_assets() {
        assert_kind(".diffler/config.toml", Kind::Config);
        assert_kind(".gitignore", Kind::Config);
        assert_kind("tsconfig.json", Kind::Config);
        assert_kind("assets/demo.gif", Kind::Assets);
        assert_kind("showcase/img/theme.png", Kind::Assets);
    }

    #[test]
    fn an_unreadable_extension_falls_through_to_other() {
        assert_kind("data/model.bin", Kind::Other);
        assert_kind("weird", Kind::Other);
    }

    #[test]
    fn a_reader_glob_outranks_the_table_and_the_repo() {
        let rules = Rules::new(vec![
            (Kind::Tests, vec!["e2e/**".to_owned()]),
            (
                Kind::Docs,
                vec!["notes/**".to_owned(), "*_note.rs".to_owned()],
            ),
        ]);
        assert_eq!(rules.kind("e2e/harness.rs", None), Kind::Tests);
        assert_eq!(rules.kind("notes/plan.rs", None), Kind::Docs);
        assert_eq!(rules.kind("src/a_note.rs", None), Kind::Docs);
        assert_eq!(rules.kind("src/lib.rs", None), Kind::Source);
        assert_eq!(
            rules.kind("e2e/harness.rs", Some(Kind::Generated)),
            Kind::Tests,
            "the reader's own globs win over the repo's attributes"
        );
    }

    #[test]
    fn the_repo_attributes_outrank_the_table() {
        let rules = Rules::default();
        assert_eq!(
            rules.kind("src/lib.rs", Some(Kind::Generated)),
            Kind::Generated
        );
        assert_eq!(rules.kind("src/lib.rs", None), Kind::Source);
    }

    #[test]
    fn globs_respect_segment_boundaries() {
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/app/lib.rs"));
        assert!(glob_match("src/**/*.rs", "src/app/diff/lib.rs"));
        assert!(glob_match("src/**", "src/a/b/c.rs"));
        assert!(glob_match("src/**", "src"));
        assert!(glob_match("*.rs", "deep/nested/lib.rs"));
        assert!(!glob_match("*.rs", "deep/lib.rsx"));
        assert!(glob_match("a*b*c.rs", "axxbyyc.rs"));
        assert!(!glob_match("a*b*c.rs", "axxbyy.rs"));
        assert!(glob_match("t?st.rs", "test.rs"));
        assert!(!glob_match("t?st.rs", "toast.rs"), "? is exactly one char");
    }

    #[test]
    fn globs_honour_the_shapes_a_gitignore_reader_writes() {
        assert!(glob_match("/src/**", "src/lib.rs"), "leading slash anchors");
        assert!(glob_match("/src/*.rs", "src/lib.rs"));
        assert!(
            glob_match("src/", "src/a/b.rs"),
            "trailing slash is a subtree"
        );
        assert!(!glob_match("src/", "srcx/a.rs"));
        assert!(glob_match("**/fixtures/**", "crates/x/fixtures/a.json"));
    }
}
