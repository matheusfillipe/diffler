//! Forgejo/Codeberg adapter. Forgejo exposes a GitHub-shaped Actions REST API,
//! fetched with `curl` through the same `CommandRunner` seam the other adapters
//! use: a public repo needs no token; a PAT is read from the environment.
//! Job logs and the dependency DAG aren't wired yet; `Capabilities` says so.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::Deserialize;

use crate::ci::error::{CiError, Result, parse_json};
use crate::ci::exec::CommandRunner;
use crate::ci::model::{
    CiJob, CiRun, JobId, JobStatus, LogChunk, PrComment, PullRequest, RunDetail, RunExtras, RunId,
};
use crate::ci::provider::{ForgeProvider, NewPrComment, NewPrReview, ProviderKind, ReviewVerdict};

pub struct ForgejoProvider {
    runner: Box<dyn CommandRunner>,
    /// `None` when no host could be resolved (no configured `[ci.forgejo]
    /// host` and no parseable remote); every call then fails closed instead
    /// of guessing a host to send the token to.
    host: Option<String>,
    /// `owner/name`.
    repo: String,
    token: Option<String>,
    branch: Option<String>,
}

impl ForgejoProvider {
    pub fn new(
        runner: Box<dyn CommandRunner>,
        host: Option<String>,
        repo: String,
        token: Option<String>,
        branch: Option<String>,
    ) -> Self {
        Self {
            runner,
            host,
            repo,
            token,
            branch,
        }
    }

    async fn get(&self, path: &str) -> Result<String> {
        self.call(path, &[]).await
    }

    /// POST `body` as JSON. Forgejo answers with the created resource.
    async fn post(&self, path: &str, body: &serde_json::Value) -> Result<String> {
        self.send("POST", path, body).await
    }

    /// Send `body` as JSON with `verb`. The response is returned unparsed, so a
    /// caller expecting an empty 204 can drop it.
    async fn send(&self, verb: &str, path: &str, body: &serde_json::Value) -> Result<String> {
        self.call(
            path,
            &[
                "-X".to_owned(),
                verb.to_owned(),
                "-H".to_owned(),
                "Content-Type: application/json".to_owned(),
                "--data-binary".to_owned(),
                body.to_string(),
            ],
        )
        .await
    }

    /// One forge comment by its id, carrying the review, path and anchor a
    /// reply or a delete has to repeat. Forgejo exposes no per-comment
    /// endpoint, so this reads the PR's comments and picks the row.
    async fn find_pr_comment(&self, number: u64, remote_id: &str) -> Result<PrComment> {
        self.pr_comments(number)
            .await?
            .into_iter()
            .find(|comment| comment.id == remote_id)
            .ok_or_else(|| CiError::NotFound(format!("comment {remote_id} on PR #{number}")))
    }

    async fn call(&self, path: &str, extra: &[String]) -> Result<String> {
        let host = self
            .host
            .as_deref()
            .ok_or_else(|| CiError::NotFound("no Forgejo host configured".to_owned()))?;
        let mut args = vec![
            "-sS".to_owned(),
            // not `--fail`: a 422's reason lives in the response body, which
            // that flag throws away
            "--fail-with-body".to_owned(),
            "--max-time".to_owned(),
            "20".to_owned(),
            "-H".to_owned(),
            "Accept: application/json".to_owned(),
        ];
        if let Some(token) = &self.token {
            args.push("-H".to_owned());
            args.push(format!("Authorization: token {token}"));
        }
        args.extend_from_slice(extra);
        args.push(format!("https://{host}/api/v1/repos/{}/{path}", self.repo));
        // a failed exec embeds the argv in the error, which the status bar
        // renders: never let the token through
        self.runner.run("curl", &args).await.map_err(|err| {
            let Some(token) = &self.token else { return err };
            match err {
                CiError::Exec { cmd, message } => CiError::Exec {
                    cmd: cmd.replace(token.as_str(), "***"),
                    message: message.replace(token.as_str(), "***"),
                },
                other => other,
            }
        })
    }
}

#[async_trait]
impl ForgeProvider for ForgejoProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Forgejo
    }

    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>> {
        let body = self.get(&format!("actions/runs?limit={limit}")).await?;
        let resp: RunsResponse = parse_json("forgejo runs", &body)?;
        Ok(resp
            .workflow_runs
            .into_iter()
            .map(RunItem::into_run)
            .filter(|run| {
                self.branch
                    .as_deref()
                    .is_none_or(|branch| run.branch == branch)
            })
            .collect())
    }

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail> {
        let found = self
            .list_runs(50)
            .await?
            .into_iter()
            .find(|r| &r.id == run)
            .ok_or_else(|| CiError::NotFound(format!("run {}", run.0)))?;
        // no run-jobs endpoint on current Forgejo: this run's jobs are the
        // tasks sharing its run number
        let body = self.get("actions/tasks?limit=50").await?;
        let tasks: TasksResponse = parse_json("forgejo tasks", &body)?;
        let jobs: Vec<CiJob> = tasks
            .workflow_runs
            .iter()
            .filter(|t| t.run_number.map(|n| n.to_string()).as_deref() == Some(run.0.as_str()))
            .map(|t| CiJob {
                id: JobId(t.id.to_string()),
                name: t.name.clone(),
                status: map_status(&t.status, t.conclusion.as_deref()),
                needs: Vec::new(),
            })
            .collect();
        Ok(RunDetail { run: found, jobs })
    }

    async fn job_log(&self, _run: &RunId, _job: &JobId, _offset: u64) -> Result<LogChunk> {
        Err(CiError::Unsupported("forgejo job logs"))
    }

    async fn run_extras(&self, _run: &RunId) -> Result<RunExtras> {
        Ok(RunExtras::default())
    }

    async fn list_prs(&self) -> Result<Vec<PullRequest>> {
        let raw = self.get("pulls?state=open&limit=50").await?;
        let pulls: Vec<PullItem> = parse_json("pr list", &raw)?;
        Ok(pulls.into_iter().map(PullItem::into_pr).collect())
    }

    async fn create_pr(&self, new: &crate::ci::NewPullRequest) -> Result<PullRequest> {
        // Forgejo marks a draft with a WIP: title prefix
        let title = if new.draft {
            format!("WIP: {}", new.title)
        } else {
            new.title.clone()
        };
        let payload = serde_json::json!({
            "base": new.base,
            "head": new.head,
            "title": title,
            "body": new.body,
        });
        let raw = self.post("pulls", &payload).await?;
        let pull: PullItem = parse_json("pr create", &raw)?;
        Ok(pull.into_pr())
    }

    async fn pr(&self, number: u64) -> Result<PullRequest> {
        let raw = self.get(&format!("pulls/{number}")).await?;
        let pull: PullItem = parse_json("pr", &raw)?;
        Ok(pull.into_pr())
    }

    async fn pr_comments(&self, number: u64) -> Result<Vec<PrComment>> {
        // the session keeps exactly what comes back, so a page left unread
        // deletes those comments from the review
        let mut reviews: Vec<ReviewItem> = Vec::new();
        for page in 1.. {
            let raw = self
                .get(&format!(
                    "pulls/{number}/reviews?limit={PAGE_SIZE}&page={page}"
                ))
                .await?;
            let batch: Vec<ReviewItem> = parse_json("pr reviews", &raw)?;
            let full = batch.len() >= PAGE_SIZE;
            reviews.extend(batch);
            if !full {
                break;
            }
        }
        // a `REQUEST_REVIEW` row is a review request, not a review
        let paths: Vec<String> = reviews
            .iter()
            .filter(|review| review.comments_count > 0 && review.state != "REQUEST_REVIEW")
            .flat_map(|review| {
                let pages = review.comments_count.div_ceil(PAGE_SIZE as u64).max(1);
                (1..=pages)
                    .map(move |page| {
                        format!(
                            "pulls/{number}/reviews/{}/comments?limit={PAGE_SIZE}&page={page}",
                            review.id
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut items = Vec::new();
        // one call per review: serially they'd stall the pane
        for page in futures_util::future::join_all(paths.iter().map(|path| self.get(path))).await {
            let page: Vec<ReviewCommentItem> = parse_json("pr review comments", &page?)?;
            items.extend(page);
        }
        Ok(into_threads(items))
    }

    async fn post_pr_comment(&self, new: &NewPrComment) -> Result<PrComment> {
        let payload = serde_json::json!({
            "event": "COMMENT",
            "commit_id": new.head_oid,
            "comments": [anchored(&new.path, &new.body, new.line, new.start_line, new.new_side)],
        });
        let raw = self
            .post(&format!("pulls/{}/reviews", new.number), &payload)
            .await?;
        // the submit answers with the review, not the comment it created
        let review: ReviewItem = parse_json("pr review", &raw)?;
        let raw = self
            .get(&format!(
                "pulls/{}/reviews/{}/comments",
                new.number, review.id
            ))
            .await?;
        let items: Vec<ReviewCommentItem> = parse_json("pr review comments", &raw)?;
        items
            .into_iter()
            .next()
            .map(ReviewCommentItem::into_comment)
            .ok_or_else(|| CiError::NotFound("the posted comment".to_owned()))
    }

    async fn reply_pr_comment(
        &self,
        number: u64,
        remote_id: &str,
        body: &str,
    ) -> Result<PrComment> {
        let parent = self.find_pr_comment(number, remote_id).await?;
        let (review, line) = (parent.thread_id.clone(), parent.line);
        let (Some(review), Some(line)) = (review, line) else {
            return Err(CiError::NotFound(format!(
                "an anchored thread for comment {remote_id}"
            )));
        };
        // a thread has no handle of its own: a reply is a new comment on the
        // parent's review repeating the parent's anchor
        let payload = anchored(&parent.path, body, line, parent.start_line, parent.new_side);
        let raw = self
            .post(
                &format!("pulls/{number}/reviews/{review}/comments"),
                &payload,
            )
            .await?;
        let item: ReviewCommentItem = parse_json("pr comment reply", &raw)?;
        Ok(item.into_comment())
    }

    async fn submit_pr_review(&self, review: &NewPrReview) -> Result<()> {
        self.post(
            &format!("pulls/{}/reviews", review.number),
            &review_payload(review),
        )
        .await
        .map(|_| ())
    }

    async fn update_pr_comment(&self, _number: u64, remote_id: &str, body: &str) -> Result<()> {
        self.send(
            "PATCH",
            &format!("issues/comments/{remote_id}"),
            &serde_json::json!({ "body": body }),
        )
        .await
        .map(|_| ())
    }

    async fn delete_pr_comment(&self, number: u64, remote_id: &str) -> Result<()> {
        let comment = self.find_pr_comment(number, remote_id).await?;
        let review = comment
            .thread_id
            .ok_or_else(|| CiError::NotFound(format!("the review owning comment {remote_id}")))?;
        // `DELETE /issues/comments/{id}` answers 204 and leaves a code comment
        // in place; only the review-scoped route actually removes one
        self.call(
            &format!("pulls/{number}/reviews/{review}/comments/{remote_id}"),
            &["-X".to_owned(), "DELETE".to_owned()],
        )
        .await
        .map(|_| ())
    }

    async fn current_pr(&self) -> Result<Option<PullRequest>> {
        let Some(branch) = &self.branch else {
            return Ok(None);
        };
        let raw = self.get("pulls?state=open&limit=50").await?;
        // a malformed response must propagate, same as `list_prs`: treating
        // it as "no PR" would look like a normal, PR-less branch
        let pulls: Vec<PullItem> = parse_json("pr list", &raw)?;
        Ok(pulls
            .into_iter()
            .find(|p| p.head.r#ref == *branch)
            .map(PullItem::into_pr))
    }
}

#[derive(Deserialize)]
struct PullItem {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    html_url: String,
    head: PullSide,
    base: PullSide,
    #[serde(default)]
    user: ForgejoUser,
}

#[derive(Deserialize, Default)]
struct ForgejoUser {
    #[serde(default)]
    login: String,
}

impl PullItem {
    fn into_pr(self) -> PullRequest {
        PullRequest {
            number: self.number,
            title: self.title,
            url: (!self.html_url.is_empty()).then_some(self.html_url),
            base_ref: self.base.r#ref,
            head_ref: self.head.r#ref,
            head_oid: self.head.sha,
            author: self.user.login,
        }
    }
}

#[derive(Deserialize)]
struct PullSide {
    #[serde(default)]
    r#ref: String,
    #[serde(default)]
    sha: String,
}

/// Forgejo caps a page at its instance `max_response_items`, 50 on Codeberg.
const PAGE_SIZE: usize = 50;

/// One review from `/pulls/{n}/reviews`.
#[derive(Deserialize)]
struct ReviewItem {
    id: u64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    comments_count: u64,
}

/// One code comment from `/pulls/{n}/reviews/{id}/comments`. `position` is the
/// absolute new-file line and `original_position` the old-file one, `0` meaning
/// "not this side"; `extra_lines_count` counts the lines after the anchor.
#[derive(Deserialize)]
struct ReviewCommentItem {
    id: u64,
    #[serde(default)]
    pull_request_review_id: u64,
    #[serde(default)]
    path: String,
    #[serde(default)]
    position: u32,
    #[serde(default)]
    original_position: u32,
    #[serde(default)]
    extra_lines_count: u32,
    #[serde(default)]
    body: String,
    #[serde(default)]
    user: ForgejoUser,
    /// Who resolved the thread this comment roots; `null` while it is open.
    #[serde(default)]
    resolver: Option<ForgejoUser>,
    #[serde(default)]
    created_at: String,
}

impl ReviewCommentItem {
    fn into_comment(self) -> PrComment {
        let new_side = self.original_position == 0;
        let anchor = if new_side {
            self.position
        } else {
            self.original_position
        };
        PrComment {
            id: self.id.to_string(),
            path: self.path,
            line: (anchor > 0).then_some(anchor + self.extra_lines_count),
            start_line: (self.extra_lines_count > 0).then_some(anchor),
            new_side,
            body: self.body,
            author: self.user.login,
            reply_to: None,
            thread_id: Some(self.pull_request_review_id.to_string()),
            resolved: self.resolver.is_some(),
            at: parse_ts(&self.created_at)
                .and_then(|at| u64::try_from(at.unix_timestamp()).ok())
                .unwrap_or(0),
        }
    }
}

/// Attach each comment to its thread. Forgejo carries no parent id: a thread is
/// the comments sharing a review, a path and a signed line, and its root is the
/// lowest id among them. The API's own order is nondeterministic across groups.
fn into_threads(mut items: Vec<ReviewCommentItem>) -> Vec<PrComment> {
    items.sort_by_key(|item| item.id);
    let mut roots: HashMap<(u64, String, u32, u32, u32), u64> = HashMap::new();
    let mut comments = Vec::with_capacity(items.len());
    for item in items {
        // the span belongs in the key: a reply repeats its parent's anchor
        // exactly, so two rows differing in span are separate comments that
        // happen to start on one line
        let key = (
            item.pull_request_review_id,
            item.path.clone(),
            item.position,
            item.original_position,
            item.extra_lines_count,
        );
        let id = item.id;
        let root = *roots.entry(key).or_insert(id);
        let mut comment = item.into_comment();
        if root != id {
            comment.reply_to = Some(root.to_string());
        }
        comments.push(comment);
    }
    comments
}

/// One comment in Forgejo's wire shape. The anchor is an absolute 1-based file
/// line on exactly one side (`0` means "not this side") plus the count of lines
/// *after* it, where diffler's `line` is the range's last line.
fn anchored(
    path: &str,
    body: &str,
    line: u32,
    start_line: Option<u32>,
    new_side: bool,
) -> serde_json::Value {
    let first = start_line.unwrap_or(line);
    serde_json::json!({
        "path": path,
        "body": body,
        "new_position": if new_side { first } else { 0 },
        "old_position": if new_side { 0 } else { first },
        "extra_lines_count": line.saturating_sub(first),
    })
}

fn review_payload(review: &NewPrReview) -> serde_json::Value {
    let event = match review.verdict {
        // the API spells approval `APPROVED`; an unrecognised event quietly
        // creates a pending, invisible review
        ReviewVerdict::Approve => "APPROVED",
        ReviewVerdict::RequestChanges => "REQUEST_CHANGES",
        ReviewVerdict::Comment => "COMMENT",
    };
    let comments: Vec<serde_json::Value> = review
        .comments
        .iter()
        .map(|c| anchored(&c.path, &c.body, c.line, c.start_line, c.new_side))
        .collect();
    serde_json::json!({
        "event": event,
        "body": review.body,
        "commit_id": review.head_oid,
        "comments": comments,
    })
}

#[derive(Deserialize)]
struct RunsResponse {
    #[serde(default)]
    workflow_runs: Vec<RunItem>,
}

/// One run from `/actions/runs`. `index_in_repo` is the human run number the
/// tasks reference and the web URL uses; it becomes the `RunId`.
#[derive(Deserialize)]
struct RunItem {
    index_in_repo: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    workflow_id: String,
    #[serde(default)]
    prettyref: String,
    #[serde(default)]
    commit_sha: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    created: Option<String>,
}

impl RunItem {
    fn into_run(self) -> CiRun {
        CiRun {
            id: RunId(self.index_in_repo.to_string()),
            name: self.workflow_id,
            title: self.title,
            branch: self.prettyref,
            commit: self.commit_sha,
            author: String::new(),
            created: self.created.as_deref().and_then(parse_ts),
            status: map_status(&self.status, None),
            url: (!self.html_url.is_empty()).then_some(self.html_url),
            remote: None,
        }
    }
}

#[derive(Deserialize)]
struct TasksResponse {
    #[serde(default)]
    workflow_runs: Vec<WorkflowRun>,
}

/// One run from `/actions/tasks` (GitHub `workflow_run`-shaped). Every field is
/// optional so a forge that omits one degrades to a blank, not a parse failure.
#[derive(Deserialize)]
struct WorkflowRun {
    id: u64,
    #[serde(default)]
    run_number: Option<u64>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
}

fn parse_ts(iso: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(iso, &time::format_description::well_known::Rfc3339).ok()
}

fn map_status(status: &str, conclusion: Option<&str>) -> JobStatus {
    // Forgejo's Actions API mirrors GitHub's `conclusion` vocabulary
    // (`crate::ci::map_conclusion` covers both); only the in-progress/no-conclusion
    // status strings are forge-specific
    crate::ci::map_conclusion(conclusion).unwrap_or(match status {
        "running" | "in_progress" => JobStatus::Running,
        "success" => JobStatus::Ok,
        "failure" => JobStatus::Failed,
        _ => JobStatus::Queued,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;

    #[tokio::test]
    async fn list_runs_parses_the_tasks_envelope() {
        let json = r#"{"workflow_runs":[
            {"id":900,"index_in_repo":7,"workflow_id":"ci.yml","title":"fix things",
             "prettyref":"main","commit_sha":"abc1234","status":"success",
             "html_url":"https://codeberg.org/acme/widgets/actions/runs/7",
             "created":"2026-06-26T10:00:00Z"}]}"#;
        let runs = ForgejoProvider::new(
            Box::new(RecordingRunner::new(&[("actions/runs", json)])),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            None,
            None,
        )
        .list_runs(10)
        .await
        .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("7".into()));
        assert_eq!(runs[0].branch, "main");
        assert_eq!(runs[0].commit, "abc1234");
        assert_eq!(runs[0].status, JobStatus::Ok);
        assert!(runs[0].created.is_some());
    }

    #[tokio::test]
    async fn a_failed_call_never_leaks_the_token() {
        struct FailingRunner;
        #[async_trait::async_trait]
        impl crate::ci::exec::CommandRunner for FailingRunner {
            async fn run(&self, program: &'static str, args: &[String]) -> Result<String> {
                Err(CiError::Exec {
                    cmd: format!("{program} {}", args.join(" ")),
                    message: "curl: (22) The requested URL returned error: 401".into(),
                })
            }
        }
        let err = ForgejoProvider::new(
            Box::new(FailingRunner),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            Some("sekret-token".into()),
            None,
        )
        .list_runs(10)
        .await
        .expect_err("fails");
        let text = err.to_string();
        assert!(!text.contains("sekret-token"), "token redacted: {text}");
        assert!(text.contains("***"));
    }

    #[tokio::test]
    async fn list_runs_scopes_to_the_branch() {
        let json = r#"{"workflow_runs":[
            {"id":900,"index_in_repo":7,"workflow_id":"ci.yml","prettyref":"main","status":"success"},
            {"id":901,"index_in_repo":8,"workflow_id":"ci.yml","prettyref":"feat/x","status":"success"}]}"#;
        let runs = ForgejoProvider::new(
            Box::new(RecordingRunner::new(&[("actions/runs", json)])),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            None,
            Some("feat/x".into()),
        )
        .list_runs(10)
        .await
        .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("8".into()));
    }

    #[tokio::test]
    async fn no_host_fails_closed_without_ever_calling_curl() {
        let runner = std::sync::Arc::new(RecordingRunner::new(&[(
            "actions/runs",
            r#"{"workflow_runs":[]}"#,
        )]));
        let err = ForgejoProvider::new(
            Box::new(runner.clone()),
            None,
            "acme/widgets".into(),
            Some("sekret-token".into()),
            None,
        )
        .list_runs(10)
        .await
        .expect_err("no host to target");
        assert!(matches!(err, CiError::NotFound(_)));
        assert!(
            runner.calls().is_empty(),
            "an unresolved host must never reach curl, e.g. a hardcoded default"
        );
    }

    #[tokio::test]
    async fn current_pr_propagates_a_parse_failure_like_list_prs() {
        let err = ForgejoProvider::new(
            Box::new(RecordingRunner::new(&[("pulls", "not json")])),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            None,
            Some("feat/x".into()),
        )
        .current_pr()
        .await
        .expect_err("malformed body must not silently read as \"no PR\"");
        assert!(matches!(err, CiError::Parse { .. }));
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;
    use std::sync::Arc;

    const REVIEWS: &str = r#"[
        {"id":500,"state":"COMMENT","comments_count":4},
        {"id":501,"state":"REQUEST_REVIEW","comments_count":0}]"#;

    /// Two threads on one review plus a reply, deliberately out of id order:
    /// the API's group order is nondeterministic.
    const COMMENTS: &str = r#"[
        {"id":31,"pull_request_review_id":500,"path":"src.rs","position":5,
         "original_position":0,"extra_lines_count":1,"body":"the range",
         "user":{"login":"reviewer"},"resolver":null,"created_at":"2026-08-01T18:23:45Z"},
        {"id":12,"pull_request_review_id":500,"path":"src.rs","position":2,
         "original_position":0,"extra_lines_count":0,"body":"the root",
         "user":{"login":"reviewer"},"resolver":{"login":"reviewer"},
         "created_at":"2026-08-01T18:23:45Z"},
        {"id":40,"pull_request_review_id":500,"path":"src.rs","position":0,
         "original_position":2,"extra_lines_count":0,"body":"the old side",
         "user":{"login":"reviewer"},"created_at":"2026-08-01T18:23:45Z"},
        {"id":20,"pull_request_review_id":500,"path":"src.rs","position":2,
         "original_position":0,"extra_lines_count":0,"body":"the reply",
         "user":{"login":"agent"},"created_at":"2026-08-01T18:24:00Z"}]"#;

    fn provider(runner: &Arc<RecordingRunner>) -> ForgejoProvider {
        ForgejoProvider::new(
            Box::new(Arc::clone(runner)),
            Some("codeberg.org".to_owned()),
            "acme/widgets".to_owned(),
            None,
            None,
        )
    }

    fn listing_runner() -> Arc<RecordingRunner> {
        Arc::new(RecordingRunner::new(&[
            ("reviews/500/comments", COMMENTS),
            ("reviews?limit", REVIEWS),
        ]))
    }

    fn comment(number: u64, line: u32, start_line: Option<u32>, new_side: bool) -> NewPrComment {
        NewPrComment {
            number,
            head_oid: "abc".to_owned(),
            path: "src.rs".to_owned(),
            line,
            start_line,
            new_side,
            counterpart: None,
            body: "a note".to_owned(),
        }
    }

    #[tokio::test]
    async fn a_comment_anchor_survives_the_round_trip() {
        let comments = provider(&listing_runner())
            .pr_comments(7)
            .await
            .expect("comments");
        let by_id = |id: &str| {
            comments
                .iter()
                .find(|c| c.id == id)
                .cloned()
                .expect("comment")
        };

        let single = by_id("12");
        assert_eq!((single.line, single.start_line), (Some(2), None));
        assert!(single.new_side);
        assert!(single.resolved, "a resolver marks the thread resolved");

        // `extra_lines_count` counts after the anchor, so 5 + 1 ends at 6
        let range = by_id("31");
        assert_eq!((range.line, range.start_line), (Some(6), Some(5)));
        assert!(range.new_side);

        let old = by_id("40");
        assert_eq!((old.line, old.start_line), (Some(2), None));
        assert!(!old.new_side, "old_position wins when it is set");
    }

    #[tokio::test]
    async fn a_thread_roots_at_its_lowest_id_and_carries_the_review() {
        let comments = provider(&listing_runner())
            .pr_comments(7)
            .await
            .expect("comments");
        let threaded: Vec<(&str, Option<&str>)> = comments
            .iter()
            .map(|c| (c.id.as_str(), c.reply_to.as_deref()))
            .collect();
        assert_eq!(
            threaded,
            [("12", None), ("20", Some("12")), ("31", None), ("40", None)],
            "same review, path and line: the later id replies to the earlier"
        );
        assert!(
            comments
                .iter()
                .all(|c| c.thread_id.as_deref() == Some("500")),
            "a reply needs the review id"
        );
    }

    #[tokio::test]
    async fn a_review_request_row_is_not_fetched_as_a_review() {
        let runner = listing_runner();
        let _ = provider(&runner).pr_comments(7).await;
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.contains("reviews/501/comments")),
            "a REQUEST_REVIEW row has no comments to fetch"
        );
    }

    #[tokio::test]
    async fn submitting_spells_approval_the_way_the_api_does() {
        let runner = Arc::new(RecordingRunner::new(&[("reviews", "{}")]));
        let _ = provider(&runner)
            .submit_pr_review(&NewPrReview {
                number: 7,
                head_oid: "abc".to_owned(),
                verdict: ReviewVerdict::Approve,
                body: "looks good".to_owned(),
                comments: vec![comment(7, 6, Some(5), true), comment(7, 2, None, false)],
            })
            .await;
        let call = runner.calls().remove(0);
        assert!(call.contains(r#""event":"APPROVED""#), "{call}");
        assert!(call.contains(r#""commit_id":"abc""#), "{call}");
        // the range anchors at its first line with the rest counted after it
        assert!(
            call.contains(r#""extra_lines_count":1,"new_position":5,"old_position":0"#),
            "{call}"
        );
        // exactly one side is set
        assert!(
            call.contains(r#""extra_lines_count":0,"new_position":0,"old_position":2"#),
            "{call}"
        );
    }

    #[tokio::test]
    async fn a_reply_repeats_the_parents_anchor_on_the_parents_review() {
        let runner = Arc::new(RecordingRunner::new(&[
            ("reviews/500/comments", COMMENTS),
            ("reviews?limit", REVIEWS),
        ]));
        let _ = provider(&runner).reply_pr_comment(7, "31", "a reply").await;
        let posted = runner
            .calls()
            .into_iter()
            .find(|call| call.contains("-X POST"))
            .expect("a reply was posted");
        assert!(posted.contains("pulls/7/reviews/500/comments"), "{posted}");
        assert!(
            posted.contains(r#""extra_lines_count":1,"new_position":5,"old_position":0"#),
            "{posted}"
        );
    }

    #[tokio::test]
    async fn deleting_uses_the_review_scoped_route() {
        let runner = listing_runner();
        let _ = provider(&runner).delete_pr_comment(7, "31").await;
        let deleted = runner
            .calls()
            .into_iter()
            .find(|call| call.contains("-X DELETE"))
            .expect("a delete was sent");
        // `issues/comments/{id}` answers 204 and deletes nothing
        assert!(
            deleted.contains("pulls/7/reviews/500/comments/31"),
            "{deleted}"
        );
    }

    #[tokio::test]
    async fn editing_patches_the_issue_comment() {
        let runner = Arc::new(RecordingRunner::new(&[("issues/comments", "{}")]));
        let _ = provider(&runner)
            .update_pr_comment(7, "31", "new text")
            .await;
        let call = runner.calls().remove(0);
        assert!(call.contains("-X PATCH"), "{call}");
        assert!(call.contains("issues/comments/31"), "{call}");
        assert!(call.contains(r#"{"body":"new text"}"#), "{call}");
    }

    #[tokio::test]
    async fn a_failed_write_never_leaks_the_token() {
        struct FailingRunner;
        #[async_trait::async_trait]
        impl crate::ci::exec::CommandRunner for FailingRunner {
            async fn run(&self, program: &'static str, args: &[String]) -> Result<String> {
                Err(CiError::Exec {
                    cmd: format!("{program} {}", args.join(" ")),
                    // a forge can echo the request back, token and all
                    message: format!(
                        "curl: (22) error 422: {{\"message\":\"rejected {}\"}}",
                        args.join(" ")
                    ),
                })
            }
        }
        let err = ForgejoProvider::new(
            Box::new(FailingRunner),
            Some("codeberg.org".into()),
            "acme/widgets".into(),
            Some("sekret-token".into()),
            None,
        )
        .submit_pr_review(&NewPrReview {
            number: 7,
            head_oid: "abc".to_owned(),
            verdict: ReviewVerdict::Comment,
            body: String::new(),
            comments: vec![comment(7, 2, None, true)],
        })
        .await
        .expect_err("fails");
        let text = err.to_string();
        assert!(!text.contains("sekret-token"), "token redacted: {text}");
        assert!(text.contains("rejected"), "the forge's reason survives");
        assert!(
            text.contains("***"),
            "the redaction reached the body: {text}"
        );
    }
}

#[cfg(test)]
mod create_pr_tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;
    use crate::ci::provider::NewPullRequest;
    use std::sync::Arc;

    fn request(draft: bool) -> NewPullRequest {
        NewPullRequest {
            base: "main".to_owned(),
            head: "feat/x".to_owned(),
            title: "a title".to_owned(),
            body: "a body".to_owned(),
            draft,
        }
    }

    fn provider(runner: &Arc<RecordingRunner>) -> ForgejoProvider {
        ForgejoProvider::new(
            Box::new(Arc::clone(runner)),
            Some("codeberg.org".to_owned()),
            "acme/widgets".to_owned(),
            None,
            None,
        )
    }

    const CREATED: &str = r#"{"number":7,"title":"a title","html_url":"https://codeberg.org/acme/widgets/pulls/7",
        "head":{"ref":"feat/x","sha":"abc"},"base":{"ref":"main","sha":"def"},"user":{"login":"reviewer"}}"#;

    #[tokio::test]
    async fn create_posts_json_and_reads_the_pull_back() {
        let runner = Arc::new(RecordingRunner::new(&[("pulls", CREATED)]));
        let pr = provider(&runner)
            .create_pr(&request(false))
            .await
            .expect("created");
        assert_eq!((pr.number, pr.head_ref.as_str()), (7, "feat/x"));
        let call = runner.calls().remove(0);
        assert!(call.contains("-X POST"), "{call}");
        assert!(call.contains(r#""title":"a title""#), "{call}");
    }

    #[tokio::test]
    async fn a_draft_becomes_a_wip_title() {
        let runner = Arc::new(RecordingRunner::new(&[("pulls", CREATED)]));
        let _ = provider(&runner).create_pr(&request(true)).await;
        let call = runner.calls().remove(0);
        assert!(call.contains(r#""title":"WIP: a title""#), "{call}");
    }
}
