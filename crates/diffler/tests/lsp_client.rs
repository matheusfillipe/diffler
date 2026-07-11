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

// Windows LSP paths (drive letters, verbatim prefixes) aren't exercised in
// CI; the protocol logic is covered on the unix runners.
#[cfg(unix)]
#[tokio::test]
async fn finds_references_for_a_changed_function() {
    let Resolution::Found(spec) = resolve("rs") else {
        eprintln!("rust-analyzer not installed; skipping");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    fixture_crate(dir.path());
    let root = dir.path().canonicalize().expect("canonical root");

    // rustup ships a rust-analyzer proxy even when the component is absent:
    // it spawns, then dies at initialize — treat that as not installed
    let Ok(mut client) = LspClient::spawn(spec.bin, spec.argv, &root).await else {
        eprintln!("rust-analyzer on PATH but not runnable; skipping");
        return;
    };
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

    // the call-hierarchy index can lag the reference index on slow runners,
    // and rust-analyzer answers with a transient `ContentModified` error
    // while it settles: poll through both until the full caller set appears
    let mut callers = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for _ in 0..40 {
        if let Ok(result) = client
            .incoming_calls(
                Path::new("src/main.rs"),
                target.select_line,
                target.select_character,
            )
            .await
        {
            callers = result;
            names = callers.iter().map(|c| c.name.clone()).collect();
            names.sort_unstable();
            if names == ["caller_one", "main"] {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(names, ["caller_one", "main"], "direct callers of target()");

    let one = callers
        .iter()
        .find(|c| c.name == "caller_one")
        .expect("caller_one");
    let mut second = Vec::new();
    for _ in 0..20 {
        if let Ok(result) = client
            .incoming_calls(Path::new(&one.path), one.select_line, one.select_character)
            .await
        {
            second = result;
            if second.len() == 1 {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(second.len(), 1, "one caller of caller_one: {second:?}");
    assert_eq!(second[0].name, "main", "the chain continues past depth 1");
}
