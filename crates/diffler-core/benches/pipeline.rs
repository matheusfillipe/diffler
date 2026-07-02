//! Hot-path benches for the diff pipeline: what runs when the user opens or
//! switches files in the diff screen. Run `just bench`; CI records main-branch
//! results so regressions show against history.

use criterion::{Criterion, criterion_group, criterion_main};
use diffler_core::highlight::{Highlighter, SyntaxTheme};
use diffler_core::model::{DiffLine, FileDiff, FileStatus, HashCache, Hunk, LineKind, hunk_id};
use diffler_core::pairing;
use similar::TextDiff;

/// Synthetic rust-like source: `lines` lines across repeated small fns.
fn source(lines: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for i in 0..lines / 5 {
        let _ = writeln!(
            out,
            "fn item_{i}(x: u32) -> u32 {{\n    let y = x + {i};\n    let z = y * 2;\n    z + 1\n}}"
        );
    }
    out
}

/// The same source with an edit every `every` lines.
fn edited(src: &str, every: usize) -> String {
    src.lines()
        .enumerate()
        .map(|(i, line)| {
            if i % every == every - 1 && line.contains("let y") {
                line.replace("x +", "x.wrapping_add(1) +")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// A `FileDiff` equivalent to what git.rs builds: full texts + line hunks.
fn file_diff(old: &str, new: &str) -> FileDiff {
    let diff = TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    let (mut old_no, mut new_no) = (1u32, 1u32);
    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches('\n').to_owned();
        let (kind, o, n) = match change.tag() {
            similar::ChangeTag::Equal => {
                let r = (LineKind::Context, Some(old_no), Some(new_no));
                old_no += 1;
                new_no += 1;
                r
            }
            similar::ChangeTag::Delete => {
                let r = (LineKind::Deleted, Some(old_no), None);
                old_no += 1;
                r
            }
            similar::ChangeTag::Insert => {
                let r = (LineKind::Added, None, Some(new_no));
                new_no += 1;
                r
            }
        };
        lines.push(DiffLine::new(kind, o, n, text));
    }
    let id = hunk_id("bench.rs", &lines);
    FileDiff {
        path: "bench.rs".to_owned(),
        old_path: None,
        status: FileStatus::Modified,
        binary: false,
        old_text: Some(old.to_owned()),
        new_text: Some(new.to_owned()),
        hunks: vec![Hunk {
            id,
            old_start: 1,
            old_lines: old.lines().count() as u32,
            new_start: 1,
            new_lines: new.lines().count() as u32,
            context: String::new(),
            lines,
        }],
        hashes: HashCache::default(),
    }
}

fn bench_pipeline(c: &mut Criterion) {
    let highlighter = Highlighter::new(SyntaxTheme::OneHalfDark);
    for lines in [1_000usize, 5_000, 20_000] {
        let old = source(lines);
        let new = edited(&old, 50);
        let base = file_diff(&old, &new);

        c.bench_function(&format!("syndiff_emphasis/{lines}"), |b| {
            b.iter_batched(
                || base.clone(),
                |mut f| highlighter.syntactic_emphasis(&mut f),
                criterion::BatchSize::LargeInput,
            );
        });
        c.bench_function(&format!("pairing_fallback/{lines}"), |b| {
            b.iter_batched(
                || base.clone(),
                |mut f| pairing::enrich_file(&mut f),
                criterion::BatchSize::LargeInput,
            );
        });
        c.bench_function(&format!("highlight_whole_file/{lines}"), |b| {
            b.iter(|| highlighter.highlight("bench.rs", &new));
        });
        c.bench_function(&format!("scope_index/{lines}"), |b| {
            b.iter(|| highlighter.scope_index("bench.rs", &new));
        });
    }
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
