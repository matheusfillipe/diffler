//! GitLab CI and merge-request adapter (CLI-only via `glab api`, REST). The
//! dependency graph is derived from pipeline stages (jobs in a stage depend on
//! all jobs in the previous stage), GitLab's default pipeline view. The exact
//! `needs` DAG is a GraphQL refinement left for later. Logs poll the job trace
//! by offset.
//!
//! Review state maps onto GitLab discussions: a thread is a discussion, its
//! notes are the comments, and an anchored note carries a `position` naming the
//! file, the line and the three shas the diff was taken against. A submitted
//! review is a batch of draft notes published at once, so the author gets one
//! notification instead of one per comment.

use async_trait::async_trait;
use serde::Deserialize;

use crate::ci::error::{CiError, Result, parse_json};
use crate::ci::exec::CommandRunner;
use crate::ci::model::{
    CiJob, CiRun, JobId, JobStatus, LogChunk, PrComment, PullRequest, RunDetail, RunExtras, RunId,
};
use crate::ci::provider::{ForgeProvider, NewPrComment, NewPrReview, ProviderKind, ReviewVerdict};

const PAGE_SIZE: usize = 100;

/// How many pages of discussions a single read walks.
const MAX_PAGES: usize = 20;

/// Talks to GitLab through `glab api`. `glab` resolves the project from the
/// repo via the `:fullpath` placeholder; an explicit `host` targets a
/// self-hosted instance. `branch` names the checked-out branch, which is how
/// the current merge request is found.
pub struct GitLabProvider {
    runner: Box<dyn CommandRunner>,
    host: Option<String>,
    branch: Option<String>,
}

impl GitLabProvider {
    pub fn new(
        runner: Box<dyn CommandRunner>,
        host: Option<String>,
        branch: Option<String>,
    ) -> Self {
        Self {
            runner,
            host,
            branch,
        }
    }

    /// `glab api <path>`, with `--hostname` when a self-hosted host is set.
    async fn api(&self, path: &str) -> Result<String> {
        self.call("GET", path, "--field", &[]).await
    }

    /// A write carrying a note's `position`, sent as multipart form fields:
    /// GitLab's REST layer unflattens bracketed keys (`position[new_line]`)
    /// into nested parameters, and rejects the same shape sent as JSON.
    async fn send_form(
        &self,
        verb: &str,
        path: &str,
        fields: &[(String, String)],
    ) -> Result<String> {
        self.call(verb, path, "--form", fields).await
    }

    /// A write of plain values, sent as JSON. `--form` reads a value beginning
    /// with `@` as a filename, which is every comment that opens with a
    /// mention, so text never travels that way.
    async fn send_json(
        &self,
        verb: &str,
        path: &str,
        fields: &[(String, String)],
    ) -> Result<String> {
        self.call(verb, path, "--raw-field", fields).await
    }

    async fn call(
        &self,
        verb: &str,
        path: &str,
        flag: &str,
        fields: &[(String, String)],
    ) -> Result<String> {
        let mut args = vec!["api".to_owned()];
        if let Some(host) = &self.host {
            args.push("--hostname".to_owned());
            args.push(host.clone());
        }
        args.push("--method".to_owned());
        args.push(verb.to_owned());
        args.push(path.to_owned());
        for (key, value) in fields {
            args.push(flag.to_owned());
            args.push(format!("{key}={value}"));
        }
        self.runner.run("glab", &args).await
    }

    async fn merge_request(&self, number: u64) -> Result<MergeRequestItem> {
        let raw = self
            .api(&format!("projects/:fullpath/merge_requests/{number}"))
            .await?;
        parse_json("glab merge request", &raw)
    }

    /// Every discussion on the merge request. A long-lived one runs past a
    /// single page, and a note left behind there is a reply or an edit that
    /// cannot find its thread. The ceiling keeps a forge that answers every
    /// page identically from looping forever.
    async fn discussions(&self, number: u64) -> Result<Vec<DiscussionItem>> {
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            let raw = self
                .api(&format!(
                    "projects/:fullpath/merge_requests/{number}/discussions?per_page={PAGE_SIZE}&page={page}"
                ))
                .await?;
            let batch: Vec<DiscussionItem> = parse_json("glab discussions", &raw)?;
            let full = batch.len() >= PAGE_SIZE;
            all.extend(batch);
            if !full {
                break;
            }
        }
        Ok(all)
    }

    /// The discussion holding note `note_id`. A reply, an edit and a delete all
    /// route through the thread, which a note id alone does not name.
    async fn discussion_of(&self, number: u64, note_id: &str) -> Result<String> {
        self.discussions(number)
            .await?
            .into_iter()
            .find(|discussion| {
                discussion
                    .notes
                    .iter()
                    .any(|note| note.id.to_string() == note_id)
            })
            .map(|discussion| discussion.id)
            .ok_or_else(|| CiError::NotFound(format!("note {note_id} on merge request !{number}")))
    }

    /// The shas a comment's position is taken against, which every anchored
    /// note has to repeat.
    async fn diff_refs(&self, number: u64) -> Result<DiffRefs> {
        self.merge_request(number)
            .await?
            .diff_refs
            .ok_or_else(|| CiError::NotFound(format!("diff refs for merge request !{number}")))
    }

    /// Best effort: a draft left behind after a failed submit would publish
    /// alongside the retry as a duplicate, and there is nothing useful to say
    /// when the cleanup itself fails.
    async fn discard_drafts(&self, number: u64, ids: &[u64]) {
        for id in ids {
            let _ = self
                .send_json(
                    "DELETE",
                    &format!("projects/:fullpath/merge_requests/{number}/draft_notes/{id}"),
                    &[],
                )
                .await;
        }
    }

    /// Open a thread on a diff line. The position has to ride a form field and
    /// the body cannot, so a body `--form` would misread lands as a stub the
    /// following edit replaces; the note keeps its anchor either way.
    async fn post_discussion(&self, new: &NewPrComment) -> Result<DiscussionItem> {
        let refs = self.diff_refs(new.number).await?;
        let staged = form_hazard(&new.body);
        let body = if staged { "…" } else { new.body.as_str() };
        let mut fields = vec![("body".to_owned(), body.to_owned())];
        fields.extend(position_fields(&refs, new));
        let raw = self
            .send_form(
                "POST",
                &format!(
                    "projects/:fullpath/merge_requests/{}/discussions",
                    new.number
                ),
                &fields,
            )
            .await?;
        let discussion: DiscussionItem = parse_json("glab post discussion", &raw)?;
        if !staged {
            return Ok(discussion);
        }
        let Some(note) = discussion.notes.first() else {
            return Ok(discussion);
        };
        let raw = self
            .send_json(
                "PUT",
                &format!(
                    "projects/:fullpath/merge_requests/{}/discussions/{}/notes/{}",
                    new.number, discussion.id, note.id
                ),
                &[("body".to_owned(), new.body.clone())],
            )
            .await?;
        let edited: NoteItem = parse_json("glab edit note", &raw)?;
        Ok(DiscussionItem {
            id: discussion.id,
            notes: vec![edited],
        })
    }
}

#[async_trait]
impl ForgeProvider for GitLabProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitLab
    }

    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>> {
        let path = format!("projects/:fullpath/pipelines?per_page={limit}");
        let out = self.api(&path).await?;
        let raw: Vec<PipelineItem> = parse_json("glab pipelines", &out)?;
        Ok(raw.into_iter().map(PipelineItem::into_run).collect())
    }

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail> {
        let meta = self
            .api(&format!("projects/:fullpath/pipelines/{}", run.0))
            .await?;
        let pipeline: PipelineItem = parse_json("glab pipeline", &meta)?;
        let jobs_out = self
            .api(&format!("projects/:fullpath/pipelines/{}/jobs", run.0))
            .await?;
        let raw: Vec<JobItem> = parse_json("glab jobs", &jobs_out)?;
        Ok(RunDetail {
            run: pipeline.into_run(),
            jobs: jobs_with_stage_edges(&raw),
        })
    }

    async fn job_log(&self, _run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk> {
        let trace = self
            .api(&format!("projects/:fullpath/jobs/{}/trace", job.0))
            .await?;
        let done = self
            .api(&format!("projects/:fullpath/jobs/{}", job.0))
            .await
            .ok()
            .and_then(|raw| serde_json::from_str::<JobState>(&raw).ok())
            .is_some_and(|job| {
                matches!(
                    map_status(&job.status),
                    JobStatus::Ok | JobStatus::Failed | JobStatus::Skipped | JobStatus::Neutral
                )
            });
        // resume from the saved offset, clamped to the end and floored to a char
        // boundary, so a multibyte split or a shrunk/replaced trace yields the
        // correct tail (or empty) instead of re-emitting the whole trace
        let mut start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(trace.len());
        while start > 0 && !trace.is_char_boundary(start) {
            start -= 1;
        }
        Ok(LogChunk {
            next_offset: trace.len() as u64,
            text: trace[start..].to_owned(),
            steps: Vec::new(),
            done,
        })
    }

    /// GitLab exposes neither run artifacts nor annotations through this adapter.
    async fn run_extras(&self, _run: &RunId) -> Result<RunExtras> {
        Ok(RunExtras::default())
    }

    async fn list_prs(&self) -> Result<Vec<PullRequest>> {
        let raw = self
            .api("projects/:fullpath/merge_requests?state=opened&order_by=updated_at&per_page=20")
            .await?;
        let items: Vec<MergeRequestItem> = parse_json("glab merge requests", &raw)?;
        Ok(items.into_iter().map(MergeRequestItem::into_pr).collect())
    }

    async fn pr(&self, number: u64) -> Result<PullRequest> {
        Ok(self.merge_request(number).await?.into_pr())
    }

    async fn pr_comments(&self, number: u64) -> Result<Vec<PrComment>> {
        Ok(self
            .discussions(number)
            .await?
            .into_iter()
            .flat_map(DiscussionItem::into_comments)
            .collect())
    }

    async fn post_pr_comment(&self, new: &NewPrComment) -> Result<PrComment> {
        self.post_discussion(new)
            .await?
            .into_comments()
            .into_iter()
            .next()
            .ok_or_else(|| CiError::NotFound("the posted comment".to_owned()))
    }

    async fn reply_pr_comment(
        &self,
        number: u64,
        remote_id: &str,
        body: &str,
    ) -> Result<PrComment> {
        let discussion = self.discussion_of(number, remote_id).await?;
        let raw = self
            .send_json(
                "POST",
                &format!(
                    "projects/:fullpath/merge_requests/{number}/discussions/{discussion}/notes"
                ),
                &[("body".to_owned(), body.to_owned())],
            )
            .await?;
        let note: NoteItem = parse_json("glab reply", &raw)?;
        note.into_comment(&discussion, Some(remote_id.to_owned()))
            .ok_or_else(|| CiError::NotFound("the posted reply".to_owned()))
    }

    async fn resolve_pr_thread(&self, number: u64, thread_id: &str, resolved: bool) -> Result<()> {
        self.send_json(
            "PUT",
            &format!("projects/:fullpath/merge_requests/{number}/discussions/{thread_id}"),
            &[("resolved".to_owned(), resolved.to_string())],
        )
        .await
        .map(|_| ())
    }

    async fn update_pr_comment(&self, number: u64, remote_id: &str, body: &str) -> Result<()> {
        let discussion = self.discussion_of(number, remote_id).await?;
        self.send_json(
            "PUT",
            &format!(
                "projects/:fullpath/merge_requests/{number}/discussions/{discussion}/notes/{remote_id}"
            ),
            &[("body".to_owned(), body.to_owned())],
        )
        .await
        .map(|_| ())
    }

    async fn delete_pr_comment(&self, number: u64, remote_id: &str) -> Result<()> {
        let discussion = self.discussion_of(number, remote_id).await?;
        self.send_json(
            "DELETE",
            &format!(
                "projects/:fullpath/merge_requests/{number}/discussions/{discussion}/notes/{remote_id}"
            ),
            &[],
        )
        .await
        .map(|_| ())
    }

    /// Draft notes published in one batch: GitLab has no review object, and
    /// posting each comment on its own would send the author a notification
    /// per line. The verdict rides the approval endpoints, the only review
    /// state GitLab's REST API exposes.
    async fn submit_pr_review(&self, review: &NewPrReview) -> Result<()> {
        let number = review.number;
        let refs = self.diff_refs(number).await?;
        let drafts = format!("projects/:fullpath/merge_requests/{number}/draft_notes");
        let mut staged: Vec<u64> = Vec::new();
        for (index, comment) in review.comments.iter().enumerate() {
            // a draft's text can only be rewritten by dropping its position, so
            // a body no form field can carry posts on its own instead: one
            // extra notification beats an unanchored comment
            if form_hazard(&comment.body) {
                match self.post_discussion(comment).await {
                    Ok(_) => continue,
                    Err(err) => {
                        self.discard_drafts(number, &staged).await;
                        return Err(err);
                    }
                }
            }
            let mut fields = vec![("note".to_owned(), comment.body.clone())];
            fields.extend(position_fields(&refs, comment));
            match self.send_form("POST", &drafts, &fields).await {
                Ok(raw) => {
                    if let Ok(draft) = parse_json::<DraftNoteItem>("glab draft note", &raw) {
                        staged.push(draft.id);
                    }
                }
                // a half-staged review would publish on the next submit as a
                // duplicate, so the drafts already made go back
                Err(err) => {
                    self.discard_drafts(number, &staged).await;
                    return Err(CiError::Exec {
                        cmd: format!("draft note {} of {}", index + 1, review.comments.len()),
                        message: err.to_string(),
                    });
                }
            }
        }
        if !review.body.trim().is_empty()
            && let Err(err) = self
                .send_json("POST", &drafts, &[("note".to_owned(), review.body.clone())])
                .await
        {
            self.discard_drafts(number, &staged).await;
            return Err(err);
        }
        // a publish left half-done keeps its drafts, and GitLab refuses the
        // retry once the same comment is staged twice
        if let Err(err) = self
            .send_json("POST", &format!("{drafts}/bulk_publish"), &[])
            .await
        {
            self.discard_drafts(number, &staged).await;
            return Err(err);
        }
        let verdict = match review.verdict {
            ReviewVerdict::Approve => Some("approve"),
            // GitLab's REST API has no request-changes state; withdrawing the
            // approval is the nearest thing it can actually record
            ReviewVerdict::RequestChanges => Some("unapprove"),
            ReviewVerdict::Comment => None,
        };
        if let Some(verdict) = verdict {
            self.send_json(
                "POST",
                &format!("projects/:fullpath/merge_requests/{number}/{verdict}"),
                &[],
            )
            .await?;
        }
        Ok(())
    }

    async fn create_pr(&self, new: &crate::ci::NewPullRequest) -> Result<PullRequest> {
        let mut args = vec![
            "mr".to_owned(),
            "create".to_owned(),
            "--source-branch".to_owned(),
            new.head.clone(),
            "--target-branch".to_owned(),
            new.base.clone(),
            "--title".to_owned(),
            new.title.clone(),
            "--description".to_owned(),
            new.body.clone(),
            "--yes".to_owned(),
        ];
        if new.draft {
            args.push("--draft".to_owned());
        }
        let raw = self.runner.run("glab", &args).await?;
        let url = mr_url(&raw).ok_or_else(|| CiError::Parse {
            what: "mr create".to_owned(),
            message: format!("no merge-request url in the output: {}", raw.trim()),
        })?;
        let number = mr_iid_from_url(url).ok_or_else(|| CiError::Parse {
            what: "mr create".to_owned(),
            message: format!("no iid in {url}"),
        })?;
        Ok(PullRequest {
            number,
            title: new.title.clone(),
            url: Some(url.to_owned()),
            base_ref: new.base.clone(),
            head_ref: new.head.clone(),
            head_oid: String::new(),
            author: String::new(),
        })
    }

    async fn current_pr(&self) -> Result<Option<PullRequest>> {
        let Some(branch) = &self.branch else {
            return Ok(None);
        };
        let raw = self
            .api(&format!(
                "projects/:fullpath/merge_requests?state=opened&source_branch={branch}"
            ))
            .await?;
        let items: Vec<MergeRequestItem> = parse_json("glab merge requests", &raw)?;
        Ok(items.into_iter().next().map(MergeRequestItem::into_pr))
    }
}

/// A value `glab` would read as something other than itself: `--form` takes a
/// leading `@` for a filename and a bare `-` for standard input.
fn form_hazard(value: &str) -> bool {
    value.starts_with('@') || value == "-"
}

/// The `position[...]` fields an anchored note carries: the file, the shas the
/// diff was taken against, and the line on whichever side the comment sits.
/// A line both sides share names both, which is the only way GitLab resolves
/// an unchanged line to a diff position.
fn position_fields(refs: &DiffRefs, new: &NewPrComment) -> Vec<(String, String)> {
    let side = if new.new_side { "new" } else { "old" };
    let line_key = format!("position[{side}_line]");
    let mut fields = vec![
        ("position[position_type]".to_owned(), "text".to_owned()),
        ("position[base_sha]".to_owned(), refs.base_sha.clone()),
        ("position[start_sha]".to_owned(), refs.start_sha.clone()),
        ("position[head_sha]".to_owned(), refs.head_sha.clone()),
        ("position[new_path]".to_owned(), new.path.clone()),
        ("position[old_path]".to_owned(), new.path.clone()),
        (line_key, new.line.to_string()),
    ];
    if let Some(counterpart) = new.counterpart {
        let other = if new.new_side { "old" } else { "new" };
        fields.push((format!("position[{other}_line]"), counterpart.to_string()));
    }
    if let Some(start) = new.start_line {
        fields.extend([
            (
                format!("position[line_range][start][{side}_line]"),
                start.to_string(),
            ),
            (
                "position[line_range][start][type]".to_owned(),
                side.to_owned(),
            ),
            (
                format!("position[line_range][end][{side}_line]"),
                new.line.to_string(),
            ),
            (
                "position[line_range][end][type]".to_owned(),
                side.to_owned(),
            ),
        ]);
    }
    fields
}

/// The merge-request url in `output`, which carries trailing chatter of its own.
fn mr_url(output: &str) -> Option<&str> {
    output
        .split_whitespace()
        .rev()
        .find(|token| token.contains("/merge_requests/"))
}

/// The iid trailing a merge-request url (`.../-/merge_requests/7`).
fn mr_iid_from_url(url: &str) -> Option<u64> {
    let (_, tail) = url.rsplit_once("/merge_requests/")?;
    tail.trim_end_matches('/').parse().ok()
}

/// Order jobs into stages (by first appearance) and link each job to every job
/// in the previous stage, GitLab's stage-sequenced pipeline graph.
fn jobs_with_stage_edges(raw: &[JobItem]) -> Vec<CiJob> {
    let mut stage_order: Vec<String> = Vec::new();
    for job in raw {
        if !stage_order.contains(&job.stage) {
            stage_order.push(job.stage.clone());
        }
    }
    let ids_in = |stage: &str| -> Vec<JobId> {
        raw.iter()
            .filter(|j| j.stage == stage)
            .map(|j| JobId(j.id.to_string()))
            .collect()
    };
    raw.iter()
        .map(|job| {
            let stage_idx = stage_order.iter().position(|s| *s == job.stage);
            let needs = stage_idx
                .and_then(|i| i.checked_sub(1))
                .and_then(|prev| stage_order.get(prev))
                .map(|stage| ids_in(stage))
                .unwrap_or_default();
            CiJob {
                id: JobId(job.id.to_string()),
                name: job.name.clone(),
                status: map_status(&job.status),
                duration_secs: job.duration.map(|secs| secs.round() as i64),
                needs,
            }
        })
        .collect()
}

fn map_status(status: &str) -> JobStatus {
    match status {
        "success" => JobStatus::Ok,
        "failed" => JobStatus::Failed,
        "running" => JobStatus::Running,
        "canceled" | "canceling" => JobStatus::Neutral,
        "skipped" | "manual" => JobStatus::Skipped,
        _ => JobStatus::Queued,
    }
}

fn parse_created(raw: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

#[derive(Deserialize)]
struct PipelineItem {
    id: u64,
    status: String,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    sha: Option<String>,
    source: Option<String>,
    created_at: Option<String>,
    web_url: Option<String>,
}

impl PipelineItem {
    fn into_run(self) -> CiRun {
        CiRun {
            id: RunId(self.id.to_string()),
            name: self.source.unwrap_or_else(|| "pipeline".to_owned()),
            title: String::new(),
            branch: self.git_ref.unwrap_or_default(),
            commit: self.sha.unwrap_or_default(),
            author: String::new(),
            created: self.created_at.as_deref().and_then(parse_created),
            status: map_status(&self.status),
            url: self.web_url,
            remote: None,
        }
    }
}

#[derive(Deserialize)]
struct JobItem {
    id: u64,
    name: String,
    stage: String,
    status: String,
    /// Seconds the job ran, as GitLab reports it; absent before it starts.
    #[serde(default)]
    duration: Option<f64>,
}

#[derive(Deserialize)]
struct JobState {
    status: String,
}

#[derive(Deserialize)]
struct MergeRequestItem {
    iid: u64,
    title: String,
    web_url: Option<String>,
    source_branch: String,
    target_branch: String,
    sha: Option<String>,
    author: Option<GitLabUser>,
    diff_refs: Option<DiffRefs>,
}

impl MergeRequestItem {
    fn into_pr(self) -> PullRequest {
        PullRequest {
            number: self.iid,
            title: self.title,
            url: self.web_url,
            base_ref: self.target_branch,
            head_ref: self.source_branch,
            head_oid: self.sha.unwrap_or_default(),
            author: self.author.map(|a| a.username).unwrap_or_default(),
        }
    }
}

/// The three commits a merge request's diff is taken against, which every
/// anchored note has to repeat back.
#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::struct_field_names)]
struct DiffRefs {
    base_sha: String,
    start_sha: String,
    head_sha: String,
}

#[derive(Deserialize)]
struct GitLabUser {
    username: String,
}

#[derive(Deserialize)]
struct DraftNoteItem {
    id: u64,
}

#[derive(Deserialize)]
struct DiscussionItem {
    id: String,
    notes: Vec<NoteItem>,
}

impl DiscussionItem {
    /// The discussion's anchored notes, root first, each carrying the thread
    /// handle. Notes GitLab wrote itself (a resolution, a force-push record)
    /// are not review comments, and an unanchored note belongs to the merge
    /// request rather than to a line.
    fn into_comments(self) -> Vec<PrComment> {
        let mut root: Option<String> = None;
        let mut comments = Vec::new();
        for note in self.notes {
            let reply_to = root.clone();
            let Some(comment) = note.into_comment(&self.id, reply_to) else {
                continue;
            };
            root.get_or_insert_with(|| comment.id.clone());
            comments.push(comment);
        }
        comments
    }
}

#[derive(Deserialize)]
struct NoteItem {
    id: u64,
    body: String,
    author: Option<GitLabUser>,
    created_at: Option<String>,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    resolved: bool,
    position: Option<NotePosition>,
}

impl NoteItem {
    fn into_comment(self, thread_id: &str, reply_to: Option<String>) -> Option<PrComment> {
        if self.system {
            return None;
        }
        let position = self.position?;
        let new_side = position.new_line.is_some();
        let line = if new_side {
            position.new_line
        } else {
            position.old_line
        };
        let start_line = position.line_range.and_then(|range| {
            let start = range.start?;
            if new_side {
                start.new_line
            } else {
                start.old_line
            }
        });
        Some(PrComment {
            id: self.id.to_string(),
            path: position.new_path.or(position.old_path).unwrap_or_default(),
            line,
            // a single-line note repeats its line in the range; only a real
            // span is a range
            start_line: start_line.filter(|start| Some(*start) != line),
            new_side,
            body: self.body,
            author: self.author.map(|a| a.username).unwrap_or_default(),
            reply_to,
            thread_id: Some(thread_id.to_owned()),
            resolved: self.resolved,
            at: self
                .created_at
                .as_deref()
                .and_then(parse_created)
                .and_then(|at| u64::try_from(at.unix_timestamp()).ok())
                .unwrap_or(0),
        })
    }
}

#[derive(Deserialize)]
struct NotePosition {
    new_path: Option<String>,
    old_path: Option<String>,
    new_line: Option<u32>,
    old_line: Option<u32>,
    line_range: Option<LineRange>,
}

#[derive(Deserialize)]
struct LineRange {
    start: Option<LineEnd>,
}

#[derive(Deserialize)]
struct LineEnd {
    new_line: Option<u32>,
    old_line: Option<u32>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;
    use crate::ci::provider::NewPullRequest;

    fn provider(responses: &[(&'static str, &str)]) -> GitLabProvider {
        GitLabProvider::new(Box::new(RecordingRunner::new(responses)), None, None)
    }

    fn provider_on(
        responses: &[(&'static str, &str)],
        branch: &str,
    ) -> (Arc<RecordingRunner>, GitLabProvider) {
        let runner = Arc::new(RecordingRunner::new(responses));
        let provider =
            GitLabProvider::new(Box::new(Arc::clone(&runner)), None, Some(branch.to_owned()));
        (runner, provider)
    }

    #[tokio::test]
    async fn list_runs_parses_pipelines() {
        let json = r#"[
          {"id":100,"status":"running","ref":"main","sha":"deadbeef","source":"push",
           "created_at":"2026-06-18T10:00:00Z","web_url":"https://gl/p/100"}
        ]"#;
        let runs = provider(&[("pipelines", json)])
            .list_runs(10)
            .await
            .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("100".into()));
        assert_eq!(runs[0].branch, "main");
        assert_eq!(runs[0].status, JobStatus::Running);
    }

    #[tokio::test]
    async fn run_detail_links_stages_into_a_dag() {
        let pipeline = r#"{"id":100,"status":"running","ref":"main","sha":"d","source":"push",
          "created_at":"2026-06-18T10:00:00Z","web_url":"u"}"#;
        let jobs = r#"[
          {"id":1,"name":"build","stage":"build","status":"success"},
          {"id":2,"name":"test","stage":"test","status":"running"},
          {"id":3,"name":"lint","stage":"test","status":"success"}
        ]"#;
        // "pipelines/100/jobs" must be matched before "pipelines/100"
        let detail = provider(&[("/jobs", jobs), ("pipelines/100", pipeline)])
            .run_detail(&RunId("100".into()))
            .await
            .expect("detail");
        assert_eq!(detail.jobs.len(), 3);
        // build is in the first stage → no upstream
        assert!(detail.jobs[0].needs.is_empty());
        // test-stage jobs depend on the build-stage job
        assert_eq!(detail.jobs[1].needs, vec![JobId("1".into())]);
        assert_eq!(detail.jobs[2].needs, vec![JobId("1".into())]);
        assert_eq!(detail.jobs[1].status, JobStatus::Running);
    }

    #[tokio::test]
    async fn job_log_marks_a_finished_job_done() {
        let chunk = provider(&[
            ("/trace", "all good"),
            ("jobs/7", r#"{"status":"success"}"#),
        ])
        .job_log(&RunId("1".into()), &JobId("7".into()), 0)
        .await
        .expect("log");
        assert!(chunk.done, "finished job stops the poll loop");

        let chunk = provider(&[("/trace", "..."), ("jobs/7", r#"{"status":"running"}"#)])
            .job_log(&RunId("1".into()), &JobId("7".into()), 0)
            .await
            .expect("log");
        assert!(!chunk.done);
    }

    #[tokio::test]
    async fn job_log_returns_the_tail_from_offset() {
        let chunk = provider(&[("/trace", "hello world")])
            .job_log(&RunId("100".into()), &JobId("1".into()), 6)
            .await
            .expect("log");
        assert_eq!(chunk.text, "world");
        assert_eq!(chunk.next_offset, 11);
    }

    #[tokio::test]
    async fn job_log_past_the_end_yields_empty_not_a_duplicate() {
        // a shrunk/replaced trace (offset > len) must not re-emit the whole trace
        let chunk = provider(&[("/trace", "short")])
            .job_log(&RunId("100".into()), &JobId("1".into()), 999)
            .await
            .expect("log");
        assert_eq!(chunk.text, "");
        assert_eq!(chunk.next_offset, 5);
    }

    #[tokio::test]
    async fn create_sends_the_branch_pair_and_reads_the_iid() {
        let runner = Arc::new(RecordingRunner::new(&[(
            "mr create",
            "Creating merge request\nhttps://gitlab.com/acme/widgets/-/merge_requests/7\n",
        )]));
        let provider = GitLabProvider::new(Box::new(Arc::clone(&runner)), None, None);
        let pr = provider
            .create_pr(&NewPullRequest {
                base: "main".to_owned(),
                head: "feat/x".to_owned(),
                title: "a title".to_owned(),
                body: "a body".to_owned(),
                draft: false,
            })
            .await
            .expect("created");
        assert_eq!(pr.number, 7);
        assert_eq!(
            pr.url.as_deref(),
            Some("https://gitlab.com/acme/widgets/-/merge_requests/7")
        );
        let call = runner.calls().remove(0);
        assert!(call.contains("--source-branch feat/x"), "{call}");
        assert!(call.contains("--target-branch main"), "{call}");
    }

    /// One merge request, shaped as the API answers it.
    const MR: &str = r#"{
      "iid": 1, "title": "Guard divide", "state": "opened",
      "source_branch": "feat/guard", "target_branch": "main",
      "sha": "aa11bb2", "web_url": "https://gitlab.com/acme/widgets/-/merge_requests/1",
      "author": {"username": "reviewer"},
      "diff_refs": {"base_sha": "cc33dd4", "start_sha": "cc33dd4", "head_sha": "aa11bb2"}
    }"#;

    /// A resolved two-note thread on a line, plus the system note GitLab writes
    /// when a thread is resolved.
    const DISCUSSIONS: &str = r#"[
      {"id": "966dff67", "individual_note": false, "notes": [
        {"id": 3657648075, "type": "DiffNote", "body": "needs a message",
         "author": {"username": "reviewer"}, "created_at": "2026-08-07T10:49:01.252Z",
         "system": false, "resolved": true,
         "position": {"new_path": "calc.py", "old_path": "calc.py", "new_line": 7,
                      "old_line": null, "line_range": null}},
        {"id": 3657648594, "type": "DiffNote", "body": "adding one",
         "author": {"username": "author"}, "created_at": "2026-08-07T10:49:30.000Z",
         "system": false, "resolved": true,
         "position": {"new_path": "calc.py", "old_path": "calc.py", "new_line": 7,
                      "old_line": null, "line_range": null}}
      ]},
      {"id": "439f49d7", "individual_note": true, "notes": [
        {"id": 3657648626, "body": "resolved all threads", "system": true,
         "created_at": "2026-08-07T10:49:40.000Z", "position": null}
      ]}
    ]"#;

    fn new_comment(line: u32, start_line: Option<u32>, new_side: bool) -> NewPrComment {
        NewPrComment {
            number: 1,
            head_oid: "aa11bb2".to_owned(),
            path: "calc.py".to_owned(),
            line,
            start_line,
            new_side,
            counterpart: None,
            body: "a remark".to_owned(),
        }
    }

    #[tokio::test]
    async fn a_line_both_sides_share_is_named_from_both() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("discussions", r#"{"id":"abc","notes":[]}"#),
            ],
            "feat/guard",
        );
        let mut comment = new_comment(7, None, true);
        comment.counterpart = Some(5);
        let _ = provider.post_pr_comment(&comment).await;

        let post = runner.calls().remove(1);
        assert!(post.contains("position[new_line]=7"), "{post}");
        assert!(
            post.contains("position[old_line]=5"),
            "an unchanged line needs both or GitLab cannot place it: {post}"
        );
    }

    #[tokio::test]
    async fn a_body_glab_would_read_as_a_filename_is_staged_then_written() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                (
                    "notes/5",
                    r#"{"id":5,"body":"@reviewer take a look","system":false,
                       "author":{"username":"reviewer"},"created_at":"2026-08-07T11:00:00.000Z",
                       "position":{"new_path":"calc.py","new_line":7}}"#,
                ),
                (
                    "discussions",
                    r#"{"id":"abc","notes":[{"id":5,"body":"…","system":false,
                       "author":{"username":"reviewer"},"created_at":"2026-08-07T11:00:00.000Z",
                       "position":{"new_path":"calc.py","new_line":7}}]}"#,
                ),
            ],
            "feat/guard",
        );
        let mut comment = new_comment(7, None, true);
        comment.body = "@reviewer take a look".to_owned();
        let posted = provider.post_pr_comment(&comment).await.expect("posted");

        let calls = runner.calls();
        assert!(
            calls[1].contains("--form") && !calls[1].contains("--form body=@reviewer"),
            "the mention never reaches a form field: {}",
            calls[1]
        );
        assert!(
            calls[2].contains("--raw-field body=@reviewer take a look"),
            "the real body follows as JSON: {}",
            calls[2]
        );
        assert_eq!(posted.body, "@reviewer take a look");
        assert_eq!(posted.line, Some(7), "the anchor survives the edit");
    }

    #[tokio::test]
    async fn a_plain_body_posts_in_one_call() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("discussions", r#"{"id":"abc","notes":[]}"#),
            ],
            "feat/guard",
        );
        let _ = provider.post_pr_comment(&new_comment(7, None, true)).await;
        assert_eq!(runner.calls().len(), 2, "diff refs, then the post");
    }

    #[tokio::test]
    async fn replies_and_edits_never_travel_as_form_fields() {
        let (runner, provider) = provider_on(
            &[
                ("discussions?", DISCUSSIONS),
                (
                    "notes",
                    r#"{"id":42,"body":"@reviewer sure","system":false,
                       "author":{"username":"reviewer"},"created_at":"2026-08-07T11:00:00.000Z",
                       "position":{"new_path":"calc.py","new_line":7}}"#,
                ),
            ],
            "feat/guard",
        );
        let _ = provider
            .reply_pr_comment(1, "3657648594", "@reviewer sure")
            .await;
        let reply = runner.calls().remove(1);
        assert!(reply.contains("--raw-field body=@reviewer sure"), "{reply}");
    }

    #[tokio::test]
    async fn a_failed_publish_takes_its_drafts_back() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("bulk_publish", "ERR:500 Internal Server Error"),
                ("draft_notes", r#"{"id": 77}"#),
            ],
            "feat/guard",
        );
        let err = provider
            .submit_pr_review(&NewPrReview {
                number: 1,
                head_oid: "aa11bb2".to_owned(),
                verdict: ReviewVerdict::Comment,
                body: String::new(),
                comments: vec![new_comment(7, None, true)],
            })
            .await;

        assert!(err.is_err());
        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("draft_notes/77") && call.contains("--method DELETE")),
            "a staged draft never outlives its failed publish: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_review_comment_no_form_field_can_carry_posts_on_its_own() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                (
                    "notes/5",
                    r#"{"id":5,"body":"@reviewer look","system":false,
                   "author":{"username":"reviewer"},"created_at":"2026-08-07T11:00:00.000Z",
                   "position":{"new_path":"calc.py","new_line":7}}"#,
                ),
                (
                    "discussions",
                    r#"{"id":"abc","notes":[{"id":5,"body":"…","system":false,
                   "author":{"username":"reviewer"},"created_at":"2026-08-07T11:00:00.000Z",
                   "position":{"new_path":"calc.py","new_line":7}}]}"#,
                ),
                ("draft_notes", r#"{"id": 77}"#),
            ],
            "feat/guard",
        );
        let (mut plain, mut mention) = (new_comment(7, None, true), new_comment(12, None, true));
        plain.body = "plain".to_owned();
        mention.body = "@reviewer look".to_owned();
        provider
            .submit_pr_review(&NewPrReview {
                number: 1,
                head_oid: "aa11bb2".to_owned(),
                verdict: ReviewVerdict::Comment,
                body: String::new(),
                comments: vec![plain, mention],
            })
            .await
            .expect("submitted");

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("draft_notes") && call.contains("note=plain")),
            "the plain comment is a draft: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|call| call.contains("/discussions") && call.contains("--method POST")),
            "the mention opens its own thread: {calls:?}"
        );
        assert!(
            calls.iter().any(|call| call.contains("bulk_publish")),
            "the drafts still publish: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_draft_takes_the_earlier_ones_back() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("note=first", r#"{"id": 77}"#),
                ("note=second", "ERR:the forge said no"),
            ],
            "feat/guard",
        );
        let (mut first, mut second) = (new_comment(7, None, true), new_comment(12, None, true));
        first.body = "first".to_owned();
        second.body = "second".to_owned();
        let err = provider
            .submit_pr_review(&NewPrReview {
                number: 1,
                head_oid: "aa11bb2".to_owned(),
                verdict: ReviewVerdict::Comment,
                body: String::new(),
                comments: vec![first, second],
            })
            .await;

        assert!(err.is_err(), "the submit reports the failure");
        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("draft_notes/77") && call.contains("--method DELETE")),
            "the staged draft is discarded: {calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.contains("bulk_publish")),
            "nothing is published: {calls:?}"
        );
    }

    #[tokio::test]
    async fn discussions_are_read_to_the_last_page() {
        let full: String = format!(
            "[{}]",
            (0..PAGE_SIZE)
                .map(|n| format!(
                    r#"{{"id":"d{n}","notes":[{{"id":{n},"body":"b","system":false,
                       "author":{{"username":"reviewer"}},"created_at":"2026-08-07T11:00:00.000Z",
                       "position":{{"new_path":"calc.py","new_line":1}}}}]}}"#
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        // `per_page=100` ends in `page=1`, so the page key needs its separator
        let (runner, provider) =
            provider_on(&[("&page=1", &full), ("&page=2", "[]")], "feat/guard");
        let comments = provider.pr_comments(1).await.expect("comments");

        assert_eq!(comments.len(), PAGE_SIZE, "the full page is kept");
        assert_eq!(
            runner.calls().len(),
            2,
            "a full page is followed by another"
        );
    }

    #[tokio::test]
    async fn a_forge_answering_every_page_alike_still_stops() {
        let full: String = format!(
            "[{}]",
            (0..PAGE_SIZE)
                .map(|n| format!(
                    r#"{{"id":"d{n}","notes":[{{"id":{n},"body":"b","system":false,
                       "author":{{"username":"reviewer"}},"created_at":"2026-08-07T11:00:00.000Z",
                       "position":{{"new_path":"calc.py","new_line":1}}}}]}}"#
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let (runner, provider) = provider_on(&[("discussions", &full)], "feat/guard");
        let comments = provider.pr_comments(1).await.expect("comments");

        assert_eq!(runner.calls().len(), MAX_PAGES, "the walk has a ceiling");
        assert_eq!(comments.len(), PAGE_SIZE * MAX_PAGES);
    }

    #[tokio::test]
    async fn a_missing_glab_says_so() {
        let provider = GitLabProvider::new(
            Box::new(crate::ci::exec::RealRunner),
            None,
            Some("feat/guard".to_owned()),
        );
        // RealRunner spawns the process, so an absent CLI is the one failure
        // reachable without a network: the message has to name the tool
        let err = provider
            .list_runs(1)
            .await
            .expect_err("no glab in the test environment");
        assert!(
            matches!(err, CiError::CliMissing("glab") | CiError::Exec { .. }),
            "a missing CLI is reported as such: {err}"
        );
    }

    #[tokio::test]
    async fn discussions_become_threaded_comments() {
        let comments = provider(&[("discussions", DISCUSSIONS)])
            .pr_comments(1)
            .await
            .expect("comments");

        assert_eq!(comments.len(), 2, "the system note is not a comment");
        let root = &comments[0];
        assert_eq!(root.id, "3657648075");
        assert_eq!(root.path, "calc.py");
        assert_eq!(root.line, Some(7));
        assert_eq!(root.start_line, None);
        assert!(root.new_side);
        assert_eq!(root.author, "reviewer");
        assert_eq!(root.reply_to, None);
        assert_eq!(root.thread_id.as_deref(), Some("966dff67"));
        assert!(root.resolved);
        assert_eq!(comments[1].reply_to.as_deref(), Some("3657648075"));
        assert_eq!(comments[1].thread_id.as_deref(), Some("966dff67"));
    }

    #[tokio::test]
    async fn posting_anchors_the_comment_to_the_merge_requests_shas() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("discussions", r#"{"id":"abc","notes":[]}"#),
            ],
            "feat/guard",
        );
        let _ = provider.post_pr_comment(&new_comment(7, None, true)).await;

        let post = runner.calls().remove(1);
        assert!(post.contains("--method POST"), "{post}");
        assert!(post.contains("merge_requests/1/discussions"), "{post}");
        assert!(post.contains("position[position_type]=text"), "{post}");
        assert!(post.contains("position[base_sha]=cc33dd4"), "{post}");
        assert!(post.contains("position[head_sha]=aa11bb2"), "{post}");
        assert!(post.contains("position[new_path]=calc.py"), "{post}");
        assert!(post.contains("position[new_line]=7"), "{post}");
        assert!(!post.contains("line_range"), "single line: {post}");
    }

    #[tokio::test]
    async fn a_range_comment_carries_its_span_and_an_old_side_one_flips_the_key() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("discussions", r#"{"id":"abc","notes":[]}"#),
            ],
            "feat/guard",
        );
        let _ = provider
            .post_pr_comment(&new_comment(7, Some(5), true))
            .await;
        let ranged = runner.calls().remove(1);
        assert!(
            ranged.contains("position[line_range][start][new_line]=5"),
            "{ranged}"
        );
        assert!(
            ranged.contains("position[line_range][end][new_line]=7"),
            "{ranged}"
        );

        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("discussions", r#"{"id":"abc","notes":[]}"#),
            ],
            "feat/guard",
        );
        let _ = provider.post_pr_comment(&new_comment(4, None, false)).await;
        let old_side = runner.calls().remove(1);
        assert!(old_side.contains("position[old_line]=4"), "{old_side}");
        assert!(!old_side.contains("position[new_line]"), "{old_side}");
    }

    #[tokio::test]
    async fn a_reply_goes_to_the_thread_holding_the_parent_note() {
        let (runner, provider) = provider_on(
            &[
                ("discussions?", DISCUSSIONS),
                (
                    "notes",
                    r#"{"id": 42, "body": "sure", "system": false,
                   "author": {"username": "reviewer"}, "created_at": "2026-08-07T11:00:00.000Z",
                   "position": {"new_path": "calc.py", "new_line": 7}}"#,
                ),
            ],
            "feat/guard",
        );
        let reply = provider
            .reply_pr_comment(1, "3657648594", "sure")
            .await
            .expect("reply");

        assert_eq!(reply.id, "42");
        assert_eq!(reply.reply_to.as_deref(), Some("3657648594"));
        let post = runner.calls().remove(1);
        assert!(
            post.contains("discussions/966dff67/notes"),
            "the parent's thread, not the note id: {post}"
        );
    }

    #[tokio::test]
    async fn resolving_puts_the_flag_on_the_thread() {
        let (runner, provider) = provider_on(&[("discussions/966dff67", "{}")], "feat/guard");
        provider
            .resolve_pr_thread(1, "966dff67", true)
            .await
            .expect("resolved");
        let call = runner.calls().remove(0);
        assert!(call.contains("--method PUT"), "{call}");
        assert!(call.contains("discussions/966dff67"), "{call}");
        assert!(call.contains("resolved=true"), "{call}");
    }

    #[tokio::test]
    async fn a_submitted_review_drafts_every_comment_then_publishes_once() {
        let (runner, provider) = provider_on(
            &[
                ("GET projects/:fullpath/merge_requests/1", MR),
                ("draft_notes", "{}"),
                ("bulk_publish", ""),
                ("approve", "{}"),
            ],
            "feat/guard",
        );
        provider
            .submit_pr_review(&NewPrReview {
                number: 1,
                head_oid: "aa11bb2".to_owned(),
                verdict: ReviewVerdict::Approve,
                body: "looks good".to_owned(),
                comments: vec![new_comment(7, None, true), new_comment(12, None, true)],
            })
            .await
            .expect("submitted");

        let calls = runner.calls();
        assert_eq!(
            calls.len(),
            6,
            "refs, two line drafts, body draft, publish, approve: {calls:?}"
        );
        assert!(calls[1].contains("position[new_line]=7"), "{}", calls[1]);
        assert!(calls[2].contains("position[new_line]=12"), "{}", calls[2]);
        assert!(
            calls[3].contains("draft_notes") && calls[3].contains("looks good"),
            "{}",
            calls[3]
        );
        assert!(calls[4].contains("bulk_publish"), "{}", calls[4]);
        assert!(calls[5].ends_with("/approve"), "{}", calls[5]);
    }

    #[tokio::test]
    async fn the_current_merge_request_is_the_one_off_the_checked_out_branch() {
        let (runner, provider) =
            provider_on(&[("merge_requests", &format!("[{MR}]"))], "feat/guard");
        let pr = provider
            .current_pr()
            .await
            .expect("lookup")
            .expect("open mr");
        assert_eq!(pr.number, 1);
        assert_eq!(pr.head_ref, "feat/guard");
        assert_eq!(pr.base_ref, "main");
        assert_eq!(pr.head_oid, "aa11bb2");
        assert_eq!(pr.author, "reviewer");
        assert!(
            runner.calls()[0].contains("source_branch=feat/guard"),
            "{:?}",
            runner.calls()
        );
    }

    #[tokio::test]
    async fn a_detached_head_has_no_current_merge_request() {
        assert!(
            provider(&[]).current_pr().await.expect("lookup").is_none(),
            "no branch, no lookup"
        );
    }
}
