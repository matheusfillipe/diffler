//! Blast radius of the selected file: which symbols the diff touches and who
//! references them beyond the changed files. Computed against the language
//! server on the runtime side; results land as events like enrichment.

use std::collections::HashSet;

use diffler_core::model::LineKind;

use super::App;
use crate::lsp::RefSite;

pub struct ChainJob {
    pub path: String,
    pub new_text: String,
    pub cursor_line: u32,
    pub extension: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainNode {
    pub id: String,
    pub label: String,
    pub path: String,
    pub line: u32,
}

#[derive(Debug)]
pub struct ChainOutcome {
    pub file: String,
    pub nodes: Vec<ChainNode>,
    pub edges: Vec<(String, String)>,
    /// Why the trace came back empty, when the worker knows.
    pub note: Option<String>,
}

pub struct BlastJob {
    pub path: String,
    pub hash: String,
    pub new_text: String,
    pub changed_lines: Vec<u32>,
    pub extension: String,
    pub diff_files: HashSet<String>,
}

#[derive(Debug)]
pub struct BlastOutcome {
    pub path: String,
    pub hash: String,
    pub symbols: Vec<(String, Vec<RefSite>)>,
    pub diff_files: HashSet<String>,
    /// Why the scan came back empty, when it's a failure rather than a
    /// legitimately reference-free symbol.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolImpact {
    pub total_refs: usize,
    pub outside: Vec<RefSite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBlast {
    pub hash: String,
    pub symbols: Vec<SymbolImpact>,
    pub note: Option<String>,
}

impl FileBlast {
    pub fn outside_files(&self) -> usize {
        self.symbols
            .iter()
            .flat_map(|s| s.outside.iter().map(|r| r.path.as_str()))
            .collect::<HashSet<_>>()
            .len()
    }
}

/// 0-based new-side lines the diff actually touches: added lines directly,
/// deletions via the nearest preceding new-side line so a pure removal still
/// lands inside its enclosing symbol. Context lines never count.
fn changed_new_lines(hunks: &[diffler_core::model::Hunk]) -> Vec<u32> {
    let mut out = Vec::new();
    for hunk in hunks {
        let mut last_new = hunk.new_start;
        for line in &hunk.lines {
            if let Some(new_no) = line.new_no {
                if line.kind != LineKind::Context {
                    out.push(new_no.saturating_sub(1));
                }
                last_new = new_no;
            } else if line.kind == LineKind::Deleted {
                out.push(last_new.saturating_sub(1));
            }
        }
    }
    out.dedup();
    out
}

/// The new-side line for the cursor at `index` of `hunk`: the line's own
/// number, or for a deletion the nearest preceding new-side line — old-side
/// numbers don't index the new text the server sees.
fn new_side_line(hunk: &diffler_core::model::Hunk, index: usize) -> Option<u32> {
    let line = hunk.lines.get(index)?;
    line.new_no.or_else(|| {
        hunk.lines
            .get(..index)
            .into_iter()
            .flatten()
            .rev()
            .find_map(|l| l.new_no)
            .or(Some(hunk.new_start))
    })
}

fn extension_of(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_owned)
}

impl App {
    pub(crate) fn queue_blast_selected(&mut self) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let model = diff
            .commit_model
            .as_ref()
            .unwrap_or_else(|| self.review.model());
        let Some(file) = model.files.get(diff.selected) else {
            return;
        };
        let Some(extension) = extension_of(&file.path) else {
            return;
        };
        let Some(new_text) = file.new_text.clone() else {
            return;
        };
        let hash = file.sides_hash();
        let cached = self
            .blast
            .get(&file.path)
            .is_some_and(|blast| blast.hash == hash);
        if cached || !self.blast_inflight.insert(hash.clone()) {
            return;
        }
        let changed_lines = changed_new_lines(&file.hunks);
        if changed_lines.is_empty() {
            self.blast_inflight.remove(&hash);
            return;
        }
        let diff_files = model.files.iter().map(|f| f.path.clone()).collect();
        self.pending_blast.push(BlastJob {
            path: file.path.clone(),
            hash,
            new_text,
            changed_lines,
            extension,
            diff_files,
        });
    }

    pub(crate) fn on_blast_event(&mut self, outcome: BlastOutcome) -> super::Flow {
        self.on_blast(outcome);
        super::Flow::Continue
    }

    pub(crate) fn on_blast(&mut self, outcome: BlastOutcome) {
        self.blast_inflight.remove(&outcome.hash);
        let changed = &outcome.diff_files;
        let symbols = outcome
            .symbols
            .into_iter()
            .map(|(_, refs)| SymbolImpact {
                total_refs: refs.len(),
                outside: refs
                    .into_iter()
                    .filter(|r| !changed.contains(&r.path))
                    .collect(),
            })
            .collect();
        self.blast.insert(
            outcome.path,
            FileBlast {
                hash: outcome.hash,
                symbols,
                note: outcome.note,
            },
        );
    }

    pub(crate) fn open_impact(&mut self) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let model = diff
            .commit_model
            .as_ref()
            .unwrap_or_else(|| self.review.model());
        let Some(file) = model.files.get(diff.selected) else {
            return;
        };
        let cursor_line = diff
            .rows
            .get(diff.cursor)
            .and_then(|row| match row {
                super::diff::DiffRow::Line { hunk, line, .. } => {
                    file.hunks.get(*hunk).and_then(|h| new_side_line(h, *line))
                }
                _ => None,
            })
            .unwrap_or(1)
            .saturating_sub(1);
        let (Some(extension), Some(new_text)) = (extension_of(&file.path), file.new_text.clone())
        else {
            self.info("no reference data for this file");
            return;
        };
        match crate::lsp::resolve(&extension) {
            crate::lsp::Resolution::Found(_) => {}
            crate::lsp::Resolution::Missing(hint) => {
                self.info(format!("language server not on PATH — install: {hint}"));
                return;
            }
            crate::lsp::Resolution::Unsupported => {
                self.info(format!("no language server known for .{extension} files"));
                return;
            }
        }
        self.chain_inflight = Some(file.path.clone());
        self.pending_chain = Some(ChainJob {
            path: file.path.clone(),
            new_text,
            cursor_line,
            extension,
        });
        self.info("tracing who calls this…");
    }

    pub(crate) fn on_chain_event(&mut self, outcome: ChainOutcome) -> super::Flow {
        self.on_chain(outcome);
        super::Flow::Continue
    }

    pub(crate) fn on_chain(&mut self, outcome: ChainOutcome) {
        use crate::graph::{Edge, Model, Node, NodeId, NodeStatus, RankDir};
        if self.chain_inflight.take().as_deref() != Some(outcome.file.as_str()) {
            return;
        }
        let showing_other_graph =
            self.screen() == super::Screen::Graph && self.impact_title.is_none();
        if showing_other_graph {
            self.info("caller trace ready — press x again from the diff");
            return;
        }
        if outcome.nodes.is_empty() {
            let note = outcome
                .note
                .unwrap_or_else(|| "no callers found for the symbol under the cursor".to_owned());
            self.info(note);
            return;
        }
        let mut model = Model::new(RankDir::LeftRight);
        self.impact_targets.clear();
        for (index, node) in outcome.nodes.iter().enumerate() {
            self.impact_targets
                .insert(node.id.clone(), (node.path.clone(), node.line));
            model.nodes.push(Node {
                id: NodeId::new(node.id.clone()),
                label: node.label.clone(),
                status: if index == 0 {
                    NodeStatus::Ok
                } else {
                    NodeStatus::Neutral
                },
                group: None,
                foldable: None,
            });
        }
        for (from, to) in &outcome.edges {
            model.edges.push(Edge {
                from: NodeId::new(from.clone()),
                to: NodeId::new(to.clone()),
                label: None,
            });
        }
        self.impact_title = Some(outcome.file);
        let mut view = crate::graph::GraphView::new();
        view.set_model(model);
        self.graph = Some(view);
        if self.screen() != super::Screen::Graph {
            self.push_screen(super::Screen::Graph);
        }
    }
}

/// One slot per language server, shared by every blast/chain worker. The
/// outer map lock is held only to fetch a slot, so one language's slow spawn
/// or long query never blocks another's.
type LspSlot = std::sync::Arc<tokio::sync::Mutex<Option<crate::lsp::LspClient>>>;
pub type LspPool =
    std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<&'static str, LspSlot>>>;

/// The language's slot, created empty on first use.
async fn slot_for(pool: &LspPool, bin: &'static str) -> LspSlot {
    pool.lock().await.entry(bin).or_default().clone()
}

/// Spawn into an empty slot, reuse an existing client otherwise, then load
/// the file's symbols. A transport failure (dead or wedged server — every
/// request is capped by the client's timeout) empties the slot so the next
/// job respawns fresh, and the real [`crate::lsp::LspError`] comes back so the
/// caller can distinguish e.g. a spawn failure from a request timeout.
async fn file_symbols(
    pool: &LspPool,
    spec: &'static crate::lsp::ServerSpec,
    root: &std::path::Path,
    path: &std::path::Path,
    text: &str,
) -> Result<(LspSlot, Vec<crate::lsp::Symbol>), crate::lsp::LspError> {
    let slot = slot_for(pool, spec.bin).await;
    let mut guard = slot.lock().await;
    if guard.is_none() {
        *guard = Some(crate::lsp::LspClient::spawn(spec.bin, spec.argv, root).await?);
    }
    // populated just above (or already present) under the held lock, so the
    // slot cannot be empty here
    #[allow(clippy::expect_used)]
    let client = guard.as_mut().expect("lsp slot populated above");
    let symbols = match client.sync_document(path, text).await {
        Ok(()) => client.document_symbols(path).await,
        Err(err) => Err(err),
    };
    match symbols {
        Ok(symbols) => {
            drop(guard);
            Ok((slot, symbols))
        }
        Err(err) => {
            *guard = None;
            Err(err)
        }
    }
}

/// Innermost changed symbol under the cursor, then its callers, then theirs:
/// a breadth-first walk of `incomingCalls` up to a small depth and node cap.
/// Failures come back as an empty outcome so the press still resolves.
pub async fn chain_calls(pool: &LspPool, root: &std::path::Path, job: &ChainJob) -> ChainOutcome {
    walk_chain(pool, root, job)
        .await
        .unwrap_or_else(|note| ChainOutcome {
            file: job.path.clone(),
            nodes: Vec::new(),
            edges: Vec::new(),
            note: Some(note),
        })
}

async fn walk_chain(
    pool: &LspPool,
    root: &std::path::Path,
    job: &ChainJob,
) -> Result<ChainOutcome, String> {
    let crate::lsp::Resolution::Found(spec) = crate::lsp::resolve(&job.extension) else {
        return Err("no language server for this file type".to_owned());
    };
    let path = std::path::Path::new(&job.path);
    // the real LspError (spawn failure, transport timeout, server error, …)
    // surfaces as-is instead of a single generic "isn't responding" message
    let (slot, symbols) = file_symbols(pool, spec, root, path, &job.new_text)
        .await
        .map_err(|err| err.to_string())?;
    let mut guard = slot.lock().await;
    let Some(target) = symbols
        .iter()
        .filter(|s| (s.start_line..=s.end_line).contains(&job.cursor_line))
        .min_by_key(|s| s.end_line - s.start_line)
        .cloned()
    else {
        return Err("no function under the cursor".to_owned());
    };

    let root_id = format!("{}:{}", job.path, target.select_line);
    let mut nodes = vec![ChainNode {
        id: root_id.clone(),
        label: format!("{} — {}", target.name, job.path),
        path: job.path.clone(),
        line: target.select_line,
    }];
    let mut edges = Vec::new();
    let mut frontier = vec![(
        root_id,
        job.path.clone(),
        target.select_line,
        target.select_character,
    )];
    let mut seen: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    'walk: for _depth in 0..3 {
        let mut next = Vec::new();
        for (callee_id, callee_path, line, character) in frontier {
            let Some(client) = guard.as_mut() else {
                break 'walk;
            };
            let Ok(callers) = client
                .incoming_calls(std::path::Path::new(&callee_path), line, character)
                .await
            else {
                *guard = None;
                break 'walk;
            };
            for caller in callers {
                if nodes.len() >= 40 {
                    break 'walk;
                }
                let id = format!("{}:{}", caller.path, caller.select_line);
                edges.push((callee_id.clone(), id.clone()));
                if !seen.insert(id.clone()) {
                    continue;
                }
                nodes.push(ChainNode {
                    id: id.clone(),
                    label: format!("{} — {}", caller.name, caller.path),
                    path: caller.path.clone(),
                    line: caller.line,
                });
                next.push((id, caller.path, caller.select_line, caller.select_character));
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(ChainOutcome {
        file: job.path.clone(),
        nodes,
        edges,
        note: None,
    })
}

/// Changed symbols → their references, batch-retried while the server is
/// still indexing. An unsupported extension completes with an empty, silent
/// outcome (there was never a server to ask); every other failure carries a
/// note so a dead or missing server doesn't cache as an indistinguishable
/// "0 references".
pub async fn blast_refs(pool: &LspPool, root: &std::path::Path, job: BlastJob) -> BlastOutcome {
    let (symbols, note) = match blast_symbols(pool, root, &job).await {
        Ok(symbols) => (symbols, None),
        Err(note) => (Vec::new(), Some(note)),
    };
    BlastOutcome {
        path: job.path,
        hash: job.hash,
        symbols,
        diff_files: job.diff_files,
        note,
    }
}

async fn blast_symbols(
    pool: &LspPool,
    root: &std::path::Path,
    job: &BlastJob,
) -> Result<Vec<(String, Vec<RefSite>)>, String> {
    let spec = match crate::lsp::resolve(&job.extension) {
        crate::lsp::Resolution::Found(spec) => spec,
        crate::lsp::Resolution::Unsupported => return Ok(Vec::new()),
        crate::lsp::Resolution::Missing(hint) => {
            return Err(format!("language server not on PATH — install: {hint}"));
        }
    };
    let path = std::path::Path::new(&job.path);
    let (slot, symbols) = file_symbols(pool, spec, root, path, &job.new_text)
        .await
        .map_err(|err| err.to_string())?;
    let touched: Vec<_> = symbols
        .into_iter()
        .filter(|s| {
            job.changed_lines
                .iter()
                .any(|l| (s.start_line..=s.end_line).contains(l))
        })
        .take(8)
        .collect();
    if touched.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for attempt in 0..30 {
        out.clear();
        let mut guard = slot.lock().await;
        for symbol in &touched {
            let Some(client) = guard.as_mut() else {
                return Err(format!("{} connection was lost", spec.bin));
            };
            let Ok(refs) = client
                .references(path, symbol.select_line, symbol.select_character)
                .await
            else {
                *guard = None;
                return Err(format!("{} failed to answer a references query", spec.bin));
            };
            out.push((symbol.name.clone(), refs));
        }
        drop(guard);
        if out.iter().any(|(_, refs)| !refs.is_empty()) {
            break;
        }
        if attempt < 29 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    #[test]
    fn on_blast_splits_refs_inside_and_outside_the_diff() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        app.on_blast(BlastOutcome {
            path: "src/lib.rs".into(),
            hash: "h".into(),
            diff_files: ["src/lib.rs".to_owned()].into(),
            symbols: vec![(
                "answer".into(),
                vec![
                    RefSite {
                        path: "src/lib.rs".into(),
                        line: 1,
                    },
                    RefSite {
                        path: "src/other.rs".into(),
                        line: 9,
                    },
                ],
            )],
            note: None,
        });
        let blast = app.blast.get("src/lib.rs").expect("stored");
        assert_eq!(blast.symbols[0].total_refs, 2);
        assert_eq!(blast.symbols[0].outside.len(), 1);
        assert_eq!(blast.symbols[0].outside[0].path, "src/other.rs");
        assert_eq!(blast.outside_files(), 1);
        assert!(blast.note.is_none());
    }

    #[test]
    fn on_blast_preserves_a_failure_note_instead_of_a_bare_empty_result() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        app.on_blast(BlastOutcome {
            path: "src/lib.rs".into(),
            hash: "h".into(),
            diff_files: ["src/lib.rs".to_owned()].into(),
            symbols: Vec::new(),
            note: Some("rust-analyzer connection was lost".to_owned()),
        });
        let blast = app.blast.get("src/lib.rs").expect("stored");
        assert!(blast.symbols.is_empty());
        assert_eq!(
            blast.note.as_deref(),
            Some("rust-analyzer connection was lost")
        );
    }

    #[tokio::test]
    async fn blast_refs_stays_quiet_for_an_unsupported_extension() {
        let pool = LspPool::default();
        let root = std::env::temp_dir();
        let job = BlastJob {
            path: "notes.made-up-ext".into(),
            hash: "h".into(),
            new_text: String::new(),
            changed_lines: vec![0],
            extension: "made-up-ext".into(),
            diff_files: HashSet::new(),
        };
        let outcome = blast_refs(&pool, &root, job).await;
        assert!(outcome.symbols.is_empty());
        assert!(
            outcome.note.is_none(),
            "an unsupported extension was never going to have references, not a failure"
        );
    }

    #[test]
    fn x_queues_a_chain_job_and_the_outcome_opens_the_graph() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(Some("src/lib.rs"));
        app.open_impact();
        // queueing needs the language server on PATH; without one the press
        // degrades to the install hint instead
        if matches!(crate::lsp::resolve("rs"), crate::lsp::Resolution::Found(_)) {
            let job = app.pending_chain.as_ref().expect("chain queued");
            assert_eq!(job.path, "src/lib.rs");
        } else {
            assert!(app.pending_chain.is_none());
        }

        app.chain_inflight = Some("src/lib.rs".into());
        app.on_chain(ChainOutcome {
            file: "src/lib.rs".into(),
            nodes: vec![
                ChainNode {
                    id: "src/lib.rs:0".into(),
                    label: "answer — src/lib.rs".into(),
                    path: "src/lib.rs".into(),
                    line: 0,
                },
                ChainNode {
                    id: "src/other.rs:4".into(),
                    label: "caller — src/other.rs".into(),
                    path: "src/other.rs".into(),
                    line: 4,
                },
            ],
            edges: vec![("src/other.rs:4".into(), "src/lib.rs:0".into())],
            note: None,
        });
        assert_eq!(app.screen(), crate::app::Screen::Graph);
        assert_eq!(app.impact_title.as_deref(), Some("src/lib.rs"));
        assert_eq!(
            app.impact_targets.get("src/other.rs:4"),
            Some(&("src/other.rs".to_owned(), 4))
        );
    }
    #[test]
    fn graph_movement_routes_through_the_keymap() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(Some("src/lib.rs"));
        app.chain_inflight = Some("src/lib.rs".into());
        app.on_chain(ChainOutcome {
            file: "src/lib.rs".into(),
            nodes: vec![
                ChainNode {
                    id: "root".into(),
                    label: "answer — src/lib.rs".into(),
                    path: "src/lib.rs".into(),
                    line: 0,
                },
                ChainNode {
                    id: "caller".into(),
                    label: "caller — src/other.rs".into(),
                    path: "src/other.rs".into(),
                    line: 4,
                },
            ],
            edges: vec![("root".into(), "caller".into())],
            note: None,
        });
        let selected = |app: &App| {
            app.graph
                .as_ref()
                .and_then(|g| g.selected())
                .map(|id| id.0.clone())
        };
        let press = |app: &mut App, c: char| {
            app.handle(crate::event::AppEvent::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        };
        assert_eq!(selected(&app).as_deref(), Some("root"));
        press(&mut app, 'l');
        assert_eq!(selected(&app).as_deref(), Some("caller"), "l moves right");
        press(&mut app, 'h');
        assert_eq!(selected(&app).as_deref(), Some("root"), "h moves back");
        press(&mut app, 'n');
        assert_eq!(
            selected(&app).as_deref(),
            Some("caller"),
            "n follows the edge when no search is up"
        );
    }

    #[test]
    fn slash_search_finds_a_graph_node_and_esc_clears_before_leaving() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(Some("src/lib.rs"));
        app.chain_inflight = Some("src/lib.rs".into());
        app.on_chain(ChainOutcome {
            file: "src/lib.rs".into(),
            nodes: vec![
                ChainNode {
                    id: "src/lib.rs:0".into(),
                    label: "answer — src/lib.rs".into(),
                    path: "src/lib.rs".into(),
                    line: 0,
                },
                ChainNode {
                    id: "src/other.rs:4".into(),
                    label: "caller — src/other.rs".into(),
                    path: "src/other.rs".into(),
                    line: 4,
                },
            ],
            edges: vec![("src/lib.rs:0".into(), "src/other.rs:4".into())],
            note: None,
        });
        let press = |app: &mut App, code: KeyCode| {
            app.handle(crate::event::AppEvent::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )));
        };
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.graph
                .as_ref()
                .and_then(|g| g.selected())
                .map(|id| id.0.as_str()),
            Some("src/other.rs:4"),
            "search lands on the matching node"
        );
        assert_eq!(
            app.search.as_ref().map(crate::search::Search::count),
            Some((1, 1))
        );

        press(&mut app, KeyCode::Esc);
        assert!(app.search.is_none(), "Esc clears the search");
        assert_eq!(app.screen(), crate::app::Screen::Graph);
        press(&mut app, KeyCode::Char('q'));
        assert_ne!(app.screen(), crate::app::Screen::Graph, "q leaves");
    }
    fn hunk(
        new_start: u32,
        lines: Vec<(LineKind, Option<u32>, Option<u32>)>,
    ) -> diffler_core::model::Hunk {
        diffler_core::model::Hunk {
            id: diffler_core::model::HunkId("h".into()),
            old_start: 1,
            old_lines: 0,
            new_start,
            new_lines: 0,
            context: String::new(),
            lines: lines
                .into_iter()
                .map(|(kind, old_no, new_no)| {
                    diffler_core::model::DiffLine::new(kind, old_no, new_no, String::new())
                })
                .collect(),
        }
    }

    #[test]
    fn changed_new_lines_skips_context_and_anchors_deletions() {
        use LineKind::{Added, Context, Deleted};
        let hunks = [hunk(
            10,
            vec![
                (Context, Some(9), Some(10)),
                (Added, None, Some(11)),
                (Deleted, Some(11), None),
                (Context, Some(12), Some(12)),
            ],
        )];
        // 0-based: the added line 11 and the deletion anchor to the same
        // line (deduped); the context lines 10 and 12 never count
        assert_eq!(changed_new_lines(&hunks), vec![10]);
    }

    #[test]
    fn new_side_line_maps_deletions_to_the_preceding_new_line() {
        use LineKind::{Context, Deleted};
        let h = hunk(
            100,
            vec![
                (Context, Some(149), Some(100)),
                (Deleted, Some(150), None),
                (Deleted, Some(151), None),
            ],
        );
        assert_eq!(new_side_line(&h, 0), Some(100));
        assert_eq!(new_side_line(&h, 1), Some(100), "deletion, not old_no 150");
        assert_eq!(new_side_line(&h, 2), Some(100));
    }

    #[test]
    fn extension_of_ignores_dotted_directories() {
        assert_eq!(extension_of("src/foo.d/main.rs").as_deref(), Some("rs"));
        assert_eq!(extension_of("src/foo.d/README"), None);
        assert_eq!(extension_of("Makefile"), None);
    }
}
