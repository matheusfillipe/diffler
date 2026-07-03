//! Background enrichment of diff files: intra-line emphasis, whole-file
//! syntax highlight, and the scope index are CPU-heavy (hundreds of ms on
//! large files) and used to run inside `draw`. They now run on the blocking
//! pool; the pane renders plain until the result lands as an event.

use diffler_core::highlight::Highlighter;
use diffler_core::model::{FileDiff, HashCache, Hunk};
use diffler_core::pairing;

use super::App;
use super::diff::{FileHighlights, FileScope};

/// Everything a worker needs, detached from the model.
#[derive(Debug)]
pub struct EnrichJob {
    pub path: String,
    pub hash: String,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub hunks: Vec<Hunk>,
    pub semantic: bool,
}

/// The computed result, installed back into the caches if still current.
#[derive(Debug)]
pub struct EnrichOutcome {
    pub path: String,
    pub hash: String,
    pub hunks: Vec<Hunk>,
    pub highlights: FileHighlights,
    pub scope: FileScope,
}

/// Run one job to completion (called on the blocking pool).
pub fn run_enrich(highlighter: &Highlighter, job: EnrichJob) -> EnrichOutcome {
    let mut file = FileDiff {
        path: job.path,
        old_path: None,
        status: diffler_core::model::FileStatus::Modified,
        binary: false,
        old_text: job.old_text,
        new_text: job.new_text,
        hunks: job.hunks,
        hashes: HashCache::default(),
    };
    if !(job.semantic && highlighter.syntactic_emphasis(&mut file)) {
        pairing::enrich_file(&mut file);
    }
    let highlight = |text: &Option<String>| {
        text.as_deref()
            .map(|content| highlighter.highlight(&file.path, content))
            .unwrap_or_default()
    };
    let highlights = FileHighlights {
        hash: job.hash.clone(),
        old: highlight(&file.old_text),
        new: highlight(&file.new_text),
    };
    let scope = FileScope {
        hash: job.hash.clone(),
        index: file
            .new_text
            .as_deref()
            .map(|content| highlighter.scope_index(&file.path, content))
            .unwrap_or_default(),
    };
    EnrichOutcome {
        path: file.path,
        hash: job.hash,
        hunks: file.hunks,
        highlights,
        scope,
    }
}

impl App {
    /// Queue enrichment for the selected diff file (and its neighbours, so a
    /// j/k step usually lands on a ready file). Cheap; deduped by content.
    pub(crate) fn queue_enrich_selected(&mut self) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let selected = diff.selected;
        let file_count = diff
            .commit_model
            .as_ref()
            .unwrap_or_else(|| self.review.model())
            .files
            .len();
        let mut targets = vec![selected];
        targets.extend(selected.checked_sub(1));
        if selected + 1 < file_count {
            targets.push(selected + 1);
        }
        for index in targets {
            self.queue_enrich_file(index);
        }
        self.queue_blast_selected();
    }

    fn queue_enrich_file(&mut self, index: usize) {
        let semantic = self.config.ui.semantic_diff;
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let model = diff
            .commit_model
            .as_ref()
            .unwrap_or_else(|| self.review.model());
        let Some(file) = model.files.get(index) else {
            return;
        };
        if file.binary || file.hunks.is_empty() {
            return;
        }
        let hash = file.sides_hash();
        let ready = diff
            .highlights
            .get(&file.path)
            .is_some_and(|cached| cached.hash == hash)
            && diff.is_enriched(&file.path);
        if ready || !self.enrich_inflight.insert(hash.clone()) {
            return;
        }
        self.pending_enrich.push(EnrichJob {
            path: file.path.clone(),
            hash,
            old_text: file.old_text.clone(),
            new_text: file.new_text.clone(),
            hunks: file.hunks.clone(),
            semantic,
        });
    }

    /// Install a finished enrichment if the file still has the same content.
    pub(crate) fn on_enriched(&mut self, outcome: EnrichOutcome) {
        self.enrich_inflight.remove(&outcome.hash);
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.highlights
            .insert(outcome.path.clone(), outcome.highlights);
        diff.scopes.insert(outcome.path.clone(), outcome.scope);
        let same = |file: &FileDiff| file.path == outcome.path && file.sides_hash() == outcome.hash;
        if let Some(model) = diff.commit_model.as_mut() {
            if let Some(file) = model.files.iter_mut().find(|f| same(f)) {
                file.hunks = outcome.hunks;
                diff.mark_enriched(&outcome.path);
            }
        } else if let Some(file) = self.review.model_mut().files.iter_mut().find(|f| same(f)) {
            file.hunks = outcome.hunks;
            diff.mark_enriched(&outcome.path);
        }
    }
}

impl App {
    /// Test pump: run queued enrichment inline, as the main loop's workers
    /// would, so snapshots capture the enriched frame.
    pub fn enrich_now(&mut self) {
        let blast_jobs: Vec<super::blast::BlastJob> = self.pending_blast.drain(..).collect();
        for job in blast_jobs {
            self.on_blast(super::blast::BlastOutcome {
                path: job.path,
                hash: job.hash,
                symbols: Vec::new(),
                diff_files: job.diff_files,
            });
        }
        let jobs: Vec<EnrichJob> = self.pending_enrich.drain(..).collect();
        for job in jobs {
            let outcome = run_enrich(crate::ui::diff::highlighter(), job);
            self.on_enriched(outcome);
        }
    }
}
