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

/// Queue `file` for enrichment unless the caller's own cache says it's
/// already fresh (`ready`) or a job for the same content is already in
/// flight. Shared by every enrichment call site (the diff pane and the
/// status screen's expanded inline diffs) so the hash/inflight/push recipe
/// lives in one place; each caller keeps only its own freshness check. Takes
/// the two collections directly (rather than `&mut App`) so a caller mid-loop
/// over data borrowed from another `App` field can still call it.
pub(super) fn queue_if_stale(
    inflight: &mut std::collections::HashSet<String>,
    pending: &mut Vec<EnrichJob>,
    file: &FileDiff,
    semantic: bool,
    ready: bool,
) {
    if ready {
        return;
    }
    let hash = file.sides_hash();
    if !inflight.insert(hash.clone()) {
        return;
    }
    pending.push(EnrichJob {
        path: file.path.clone(),
        hash,
        old_text: file.old_text.clone(),
        new_text: file.new_text.clone(),
        hunks: file.hunks.clone(),
        semantic,
    });
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
        let ready = diff
            .highlights
            .get(&file.path)
            .is_some_and(|cached| cached.hash == file.sides_hash())
            && diff.is_enriched(&file.path);
        queue_if_stale(
            &mut self.enrich_inflight,
            &mut self.pending_enrich,
            file,
            semantic,
            ready,
        );
    }

    /// Install a finished enrichment wherever the file still has the same
    /// content: the diff view's model and caches, and any expanded
    /// status-section file (jobs from both screens share this worker path).
    /// A stale outcome (the file changed mid-flight) installs nothing; the
    /// next frame re-queues against the new content.
    pub(crate) fn on_enriched(&mut self, outcome: EnrichOutcome) {
        self.enrich_inflight.remove(&outcome.hash);
        self.install_status_enrichment(&outcome);
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let context = diff.context.get(&outcome.path).copied();
        let same = |file: &FileDiff| file.path == outcome.path && file.sides_hash() == outcome.hash;
        let file = match diff.commit_model.as_mut() {
            Some(model) => model.files.iter_mut().find(|f| same(f)),
            None => self.review.model_mut().files.iter_mut().find(|f| same(f)),
        };
        let Some(file) = file else {
            return;
        };
        file.hunks = outcome.hunks;
        // enrichment ships default-context hunks; reinstalling the expansion
        // reshapes them, so the row list must re-flow to match
        let reshaped = context.is_some_and(|context| super::expand::apply_context(file, context));
        diff.highlights
            .insert(outcome.path.clone(), outcome.highlights);
        diff.scopes.insert(outcome.path.clone(), outcome.scope);
        diff.mark_enriched(&outcome.path);
        if reshaped {
            diff.mark_rows_dirty();
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
                note: None,
            });
        }
        let jobs: Vec<EnrichJob> = self.pending_enrich.drain(..).collect();
        for job in jobs {
            let outcome = run_enrich(&self.highlighter, job);
            self.on_enriched(outcome);
        }
    }
}
