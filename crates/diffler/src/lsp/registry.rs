//! Language-server lookup, Helix-style: a static table of well-known servers
//! per file extension, resolved against PATH. Nothing is downloaded; a missing
//! server surfaces its install hint in the UI.

pub struct ServerSpec {
    pub bin: &'static str,
    pub argv: &'static [&'static str],
    pub install_hint: &'static str,
}

const RUST: &[ServerSpec] = &[ServerSpec {
    bin: "rust-analyzer",
    argv: &[],
    install_hint: "rustup component add rust-analyzer",
}];
const GO: &[ServerSpec] = &[ServerSpec {
    bin: "gopls",
    argv: &[],
    install_hint: "go install golang.org/x/tools/gopls@latest",
}];
const PYTHON: &[ServerSpec] = &[
    ServerSpec {
        bin: "basedpyright-langserver",
        argv: &["--stdio"],
        install_hint: "uv tool install basedpyright",
    },
    ServerSpec {
        bin: "pyright-langserver",
        argv: &["--stdio"],
        install_hint: "npm i -g pyright",
    },
];
const TYPESCRIPT: &[ServerSpec] = &[ServerSpec {
    bin: "typescript-language-server",
    argv: &["--stdio"],
    install_hint: "npm i -g typescript-language-server typescript",
}];
const C_CPP: &[ServerSpec] = &[ServerSpec {
    bin: "clangd",
    argv: &[],
    install_hint: "brew install llvm (or apt install clangd)",
}];
const RUBY: &[ServerSpec] = &[ServerSpec {
    bin: "ruby-lsp",
    argv: &[],
    install_hint: "gem install ruby-lsp",
}];
const BASH: &[ServerSpec] = &[ServerSpec {
    bin: "bash-language-server",
    argv: &["start"],
    install_hint: "npm i -g bash-language-server",
}];

/// The one extension table: which servers can handle a file and the LSP
/// `languageId` its documents open with.
fn language(extension: &str) -> Option<(&'static [ServerSpec], &'static str)> {
    Some(match extension {
        "rs" => (RUST, "rust"),
        "go" => (GO, "go"),
        "py" | "pyi" => (PYTHON, "python"),
        "ts" | "tsx" => (TYPESCRIPT, "typescript"),
        "js" | "jsx" | "mjs" | "cjs" => (TYPESCRIPT, "javascript"),
        "c" | "h" => (C_CPP, "c"),
        "cpp" | "cc" | "hpp" => (C_CPP, "cpp"),
        "rb" => (RUBY, "ruby"),
        "sh" | "bash" => (BASH, "shellscript"),
        _ => return None,
    })
}

pub(crate) fn language_id(path: &std::path::Path) -> &'static str {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(language)
        .map_or("plaintext", |(_, id)| id)
}

pub enum Resolution {
    Found(&'static ServerSpec),
    Missing(&'static str),
    Unsupported,
}

pub fn resolve(extension: &str) -> Resolution {
    let Some((specs, _)) = language(extension) else {
        return Resolution::Unsupported;
    };
    let hint = specs.first().map_or("", |s| s.install_hint);
    let Some(path) = std::env::var_os("PATH") else {
        return Resolution::Missing(hint);
    };
    for spec in specs {
        if crate::ci::on_path(spec.bin, &path) {
            return Resolution::Found(spec);
        }
    }
    Resolution::Missing(hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_resolve_to_found_or_a_hint() {
        match resolve("rs") {
            Resolution::Found(spec) => assert_eq!(spec.bin, "rust-analyzer"),
            Resolution::Missing(hint) => assert!(hint.contains("rust-analyzer")),
            Resolution::Unsupported => panic!("rust must be supported"),
        }
        assert!(matches!(resolve("xyz"), Resolution::Unsupported));
    }
}
