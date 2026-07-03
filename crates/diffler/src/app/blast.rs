//! Blast radius of the selected file: which symbols the diff touches and who
//! references them beyond the changed files. Computed against the language
//! server on the runtime side; results land as events like enrichment.

use std::collections::HashSet;

use diffler_core::model::LineKind;

use super::App;
use crate::lsp::RefSite;

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
        let Some(extension) = file.path.rsplit_once('.').map(|(_, e)| e.to_owned()) else {
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
        use crate::graph::{Edge, Model, Node, NodeId, NodeStatus, RankDir};
        let Some(path) = self
            .diff
            .as_ref()
            .and_then(|d| d.selected_path(&self.review))
        else {
            return;
        };
        let Some(blast) = self.blast.get(&path) else {
            if self.blast_inflight.is_empty() {
                self.info("no references for this file (unsupported language or no changes)");
            } else {
                self.info("still scanning references — the language server is indexing");
            }
            return;
        };
        let mut model = Model::new(RankDir::LeftRight);
        self.impact_targets.clear();
        for symbol in &blast.symbols {
            let sym_id = format!("sym:{}", symbol.name);
            model.nodes.push(Node {
                id: NodeId::new(sym_id.clone()),
                label: format!("{} ({} refs)", symbol.name, symbol.total_refs),
                status: if symbol.outside.is_empty() {
                    NodeStatus::Neutral
                } else {
                    NodeStatus::Ok
                },
                group: None,
                foldable: None,
            });
            let mut per_file: Vec<(&str, u32, usize)> = Vec::new();
            for site in &symbol.outside {
                match per_file.iter_mut().find(|(p, ..)| *p == site.path) {
                    Some((.., count)) => *count += 1,
                    None => per_file.push((&site.path, site.line, 1)),
                }
            }
            for (ref_path, line, count) in per_file {
                let node_id = format!("ref:{sym_id}:{ref_path}");
                self.impact_targets
                    .insert(node_id.clone(), (ref_path.to_owned(), line));
                model.nodes.push(Node {
                    id: NodeId::new(node_id.clone()),
                    label: format!("{ref_path} ({count})"),
                    status: NodeStatus::Neutral,
                    group: None,
                    foldable: None,
                });
                model.edges.push(Edge {
                    from: NodeId::new(sym_id.clone()),
                    to: NodeId::new(node_id),
                    label: None,
                });
            }
        }
        if model.nodes.is_empty() {
            self.info("no changed symbols with references");
            return;
        }
        self.impact_title = Some(path);
        let mut view = crate::graph::GraphView::new();
        view.set_model(model);
        self.graph = Some(view);
        self.push_screen(super::Screen::Graph);
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
    fn open_impact_builds_the_reference_graph() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(Some("src/lib.rs"));
        app.blast.insert(
            "src/lib.rs".into(),
            FileBlast {
                hash: "h".into(),
                symbols: vec![SymbolImpact {
                    name: "answer".into(),
                    total_refs: 3,
                    outside: vec![RefSite {
                        path: "src/other.rs".into(),
                        line: 4,
                    }],
                }],
            },
        );
        app.open_impact();
        assert_eq!(app.screen(), crate::app::Screen::Graph);
        assert_eq!(app.impact_title.as_deref(), Some("src/lib.rs"));
        let target = app.impact_targets.values().next().expect("jump target");
        assert_eq!(target, &("src/other.rs".to_owned(), 4));
    }
}
