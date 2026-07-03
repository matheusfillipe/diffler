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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolImpact {
    pub name: String,
    pub total_refs: usize,
    pub outside: Vec<RefSite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBlast {
    pub hash: String,
    pub symbols: Vec<SymbolImpact>,
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
        let changed_lines: Vec<u32> = file
            .hunks
            .iter()
            .filter(|h| h.lines.iter().any(|l| l.kind != LineKind::Context))
            .flat_map(|h| &h.lines)
            .filter_map(|l| l.new_no.map(|n| n.saturating_sub(1)))
            .collect();
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
            .map(|(name, refs)| SymbolImpact {
                name,
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
            },
        );
    }

    pub fn blast_computing(&self, hash: &str) -> bool {
        self.blast_inflight.contains(hash)
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
                super::diff::DiffRow::Line { hunk, line, .. } => file
                    .hunks
                    .get(*hunk)
                    .and_then(|h| h.lines.get(*line))
                    .and_then(|l| l.new_no.or(l.old_no)),
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
        if outcome.nodes.is_empty() {
            self.info("no callers found for the symbol under the cursor");
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
        });
        let blast = app.blast.get("src/lib.rs").expect("stored");
        assert_eq!(blast.symbols[0].total_refs, 2);
        assert_eq!(blast.symbols[0].outside.len(), 1);
        assert_eq!(blast.symbols[0].outside[0].path, "src/other.rs");
        assert_eq!(blast.outside_files(), 1);
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
        });
        assert_eq!(app.screen(), crate::app::Screen::Graph);
        assert_eq!(app.impact_title.as_deref(), Some("src/lib.rs"));
        assert_eq!(
            app.impact_targets.get("src/other.rs:4"),
            Some(&("src/other.rs".to_owned(), 4))
        );
    }
    #[test]
    fn slash_search_finds_a_graph_node_and_esc_clears_before_leaving() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(Some("src/lib.rs"));
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
        assert!(app.search.is_none(), "first Esc clears the search");
        assert_eq!(app.screen(), crate::app::Screen::Graph);
        press(&mut app, KeyCode::Esc);
        assert_ne!(app.screen(), crate::app::Screen::Graph, "second Esc leaves");
    }
    #[test]
    fn extension_of_ignores_dotted_directories() {
        assert_eq!(extension_of("src/foo.d/main.rs").as_deref(), Some("rs"));
        assert_eq!(extension_of("src/foo.d/README"), None);
        assert_eq!(extension_of("Makefile"), None);
    }
}
