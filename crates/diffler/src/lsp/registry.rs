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

pub fn candidates(extension: &str) -> Option<&'static [ServerSpec]> {
    Some(match extension {
        "rs" => RUST,
        "go" => GO,
        "py" | "pyi" => PYTHON,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => TYPESCRIPT,
        "c" | "h" | "cpp" | "cc" | "hpp" => C_CPP,
        "rb" => RUBY,
        "sh" | "bash" => BASH,
        _ => return None,
    })
}

pub enum Resolution {
    Found(&'static ServerSpec),
    Missing(&'static str),
    Unsupported,
}

pub fn resolve(extension: &str) -> Resolution {
    let Some(specs) = candidates(extension) else {
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
