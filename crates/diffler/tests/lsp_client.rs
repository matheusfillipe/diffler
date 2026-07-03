//! End-to-end against a real rust-analyzer on a tiny crate: spawn, open,
//! symbols, references. Skips quietly when rust-analyzer isn't installed so
//! CI without it stays green.

#![allow(clippy::expect_used, clippy::print_stderr)]

use std::path::Path;

use diffler::lsp::{LspClient, Resolution, resolve};

fn fixture_crate(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"blast\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::write(
        root.join("src/main.rs"),
        "fn target() -> u32 { 7 }\n\nfn caller_one() -> u32 { target() }\n\nfn main() { let _ = caller_one() + target(); }\n",
    )
    .expect("source");
}

#[tokio::test]
async fn finds_references_for_a_changed_function() {
    let Resolution::Found(spec) = resolve("rs") else {
        eprintln!("rust-analyzer not installed; skipping");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    fixture_crate(dir.path());
    let root = dir.path().canonicalize().expect("canonical root");

    let mut client = LspClient::spawn(spec.bin, spec.argv, &root)
        .await
        .expect("spawn rust-analyzer");
    let source = std::fs::read_to_string(root.join("src/main.rs")).expect("read");
    client
        .sync_document(Path::new("src/main.rs"), &source)
        .await
        .expect("open");

    let symbols = client
        .document_symbols(Path::new("src/main.rs"))
        .await
        .expect("symbols");
    let target = symbols
        .iter()
        .find(|s| s.name == "target")
        .expect("target symbol");

    // indexing may lag the first query; retry briefly until refs appear
    let mut refs = Vec::new();
    for _ in 0..40 {
        refs = client
            .references(
                Path::new("src/main.rs"),
                target.select_line,
                target.select_character,
            )
            .await
            .expect("references");
        if refs.len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(refs.len(), 2, "two callers of target(): {refs:?}");
    assert!(refs.iter().all(|r| r.path == "src/main.rs"));
}
