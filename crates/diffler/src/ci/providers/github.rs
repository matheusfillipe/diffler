//! GitHub Actions adapter (via `gh`). The dependency DAG comes from a run's
//! workflow YAML `jobs.<id>.needs` (the run API omits it); status overlays from
//! `gh run view`. Logs, steps, artifacts, and annotations all come from the REST
//! API via `gh api`. The job-log archive 404s until the job finishes, so an
//! in-progress job returns its live step states with the content still empty.
//! A `uses:` job calls a reusable workflow whose jobs the caller YAML doesn't
//! list; that workflow is fetched and inlined so its jobs appear with real edges.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ci::error::{CiError, Result, parse_json};
use crate::ci::exec::CommandRunner;
use crate::ci::model::{
    Annotation, AnnotationLevel, Artifact, CiJob, CiRun, JobId, JobStatus, LogChunk, LogStepMeta,
    PrComment, PullRequest, RunDetail, RunExtras, RunId, ts_sort_key,
};
use crate::ci::provider::{ForgeProvider, ProviderKind};

/// Talks to GitHub Actions through `gh`. The runs list is scoped to the current
/// `branch` (across all of its workflows); each run's DAG comes from whichever
/// of the repo's `workflows` YAMLs matches that run's workflow name.
pub type YamlCache = std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>;

/// The last response GitHub gave for one endpoint, kept so the next poll can
/// ask conditionally. A `304` answer costs no rate limit, which is what lets
/// the runs list stay on a live cadence.
#[derive(Debug, Clone)]
pub struct Conditional {
    pub etag: String,
    pub body: String,
}

/// Per-endpoint conditional state, shared across provider rebuilds the way
/// [`YamlCache`] is.
pub type EtagCache = std::sync::Arc<std::sync::Mutex<HashMap<String, Conditional>>>;

pub struct GitHubProvider {
    runner: Box<dyn CommandRunner>,
    etags: EtagCache,
    /// Fetched reusable-workflow bodies keyed by contents path (which embeds
    /// the ref, so entries are immutable). Shared across provider rebuilds so
    /// the graph poll doesn't refetch per cycle.
    yaml_cache: YamlCache,
    /// Every `.github/workflows/*.yml` body, so a run's DAG is built from its own
    /// workflow (matched by the YAML `name:`), not a single guessed file.
    workflows: Vec<String>,
    /// The checked-out branch, scoping the runs list; `None` on detached HEAD.
    branch: Option<String>,
    /// `owner/name` of the remote diffler picked, from its URL. `gh` resolves
    /// a fork to its parent when nobody says otherwise, so every call names
    /// the repo explicitly.
    slug: Option<String>,
}

impl GitHubProvider {
    pub fn new(
        runner: Box<dyn CommandRunner>,
        workflows: Vec<String>,
        branch: Option<String>,
        yaml_cache: YamlCache,
        etags: EtagCache,
        slug: Option<String>,
    ) -> Self {
        Self {
            runner,
            etags,
            yaml_cache,
            workflows,
            branch,
            slug,
        }
    }

    /// `path` with `{owner}`/`{repo}` filled in from the remote diffler picked.
    /// Left to `gh` when the remote named no repo it could parse.
    fn scoped(&self, path: &str) -> String {
        let Some((owner, name)) = self.slug.as_deref().and_then(|s| s.split_once('/')) else {
            return path.to_owned();
        };
        path.replace("{owner}", owner).replace("{repo}", name)
    }

    /// `gh <args>` against the picked repo: the subcommands take `-R`, unlike
    /// `gh api`, whose paths go through [`Self::scoped`] instead.
    async fn gh(&self, args: &[String]) -> Result<String> {
        let mut args = args.to_vec();
        if let Some(slug) = self.slug.clone() {
            args.push("-R".to_owned());
            args.push(slug);
        }
        self.runner.run("gh", &args).await
    }

    /// `gh api <path>` asking GitHub to answer only if the resource changed.
    /// An unchanged answer is a `304`, which costs no rate limit, so a live
    /// poll of a quiet repo is effectively free. `gh` exits non-zero on a
    /// `304`, so the status comes from the response rather than the exit code.
    async fn conditional_api(&self, path: &str) -> Result<String> {
        let known = self
            .etags
            .lock()
            .ok()
            .and_then(|cache| cache.get(path).cloned());
        let mut args = vec!["api".to_owned(), "-i".to_owned(), self.scoped(path)];
        if let Some(cached) = &known {
            args.push("-H".to_owned());
            args.push(format!("If-None-Match: {}", cached.etag));
        }
        let raw = self.runner.run_ignoring_status("gh", &args).await?;
        let (status, headers, body) = split_response(&raw);
        match status {
            Some(304) => known
                .map(|cached| cached.body)
                .ok_or_else(|| CiError::Parse {
                    what: "gh api".to_owned(),
                    message: "304 without a cached body".to_owned(),
                }),
            Some(200) => {
                if let (Some(etag), Ok(mut cache)) = (header(&headers, "etag"), self.etags.lock()) {
                    cache.insert(
                        path.to_owned(),
                        Conditional {
                            etag,
                            body: body.clone(),
                        },
                    );
                }
                Ok(body)
            }
            _ => Err(CiError::Exec {
                cmd: format!("gh api {path}"),
                message: raw.lines().next().unwrap_or("no response").to_owned(),
            }),
        }
    }

    /// `gh api <path>`; `{owner}`/`{repo}` in `path` resolve to the current repo.
    async fn api(&self, path: &str) -> Result<String> {
        self.runner
            .run("gh", &["api".to_owned(), self.scoped(path)])
            .await
    }

    /// Thread handle and resolution per review-comment database id, from the
    /// GraphQL `reviewThreads` connection (REST exposes neither).
    async fn review_threads(&self, number: u64) -> Result<HashMap<u64, (String, bool)>> {
        let (owner, name) = self.repo_slug().await?;
        let query = "query($owner:String!,$name:String!,$number:Int!,$cursor:String){\
            repository(owner:$owner,name:$name){pullRequest(number:$number){\
            reviewThreads(first:100,after:$cursor){pageInfo{hasNextPage endCursor}\
            nodes{id isResolved comments(first:100){nodes{databaseId}}}}}}}";
        let mut map = HashMap::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut args = vec![
                "api".to_owned(),
                "graphql".to_owned(),
                "-f".to_owned(),
                format!("query={query}"),
                "-f".to_owned(),
                format!("owner={owner}"),
                "-f".to_owned(),
                format!("name={name}"),
                "-F".to_owned(),
                format!("number={number}"),
            ];
            if let Some(cursor) = &cursor {
                args.push("-f".to_owned());
                args.push(format!("cursor={cursor}"));
            }
            let raw = self.runner.run("gh", &args).await?;
            let page: ThreadsPage = parse_json("review threads", &raw)?;
            let threads = page.data.repository.pull_request.review_threads;
            for node in threads.nodes {
                for comment in node.comments.nodes {
                    map.insert(comment.database_id, (node.id.clone(), node.is_resolved));
                }
            }
            // a next page without a cursor cannot advance; bail rather than
            // refetch page one forever inside the poll task
            if !threads.page_info.has_next_page || threads.page_info.end_cursor.is_none() {
                return Ok(map);
            }
            cursor = threads.page_info.end_cursor;
        }
    }

    /// The current repo's `owner`/`name`, for GraphQL calls where `gh` does
    /// not expand `{owner}`/`{repo}` placeholders.
    async fn repo_slug(&self) -> Result<(String, String)> {
        if let Some((owner, name)) = self.slug.as_deref().and_then(|s| s.split_once('/')) {
            return Ok((owner.to_owned(), name.to_owned()));
        }
        let raw = self
            .runner
            .run(
                "gh",
                &[
                    "repo".to_owned(),
                    "view".to_owned(),
                    "--json".to_owned(),
                    "owner,name".to_owned(),
                ],
            )
            .await?;
        let slug: RepoSlug = parse_json("repo slug", &raw)?;
        Ok((slug.owner.login, slug.name))
    }

    /// `gh api` returning the raw file body (not the base64 contents envelope).
    async fn api_raw(&self, path: &str) -> Result<String> {
        self.runner
            .run(
                "gh",
                &[
                    "api".to_owned(),
                    "-H".to_owned(),
                    "Accept: application/vnd.github.raw".to_owned(),
                    self.scoped(path),
                ],
            )
            .await
    }

    /// Fetch and parse the workflow a `uses:` points at: a local `./path` (read
    /// at the run's commit) or a remote `owner/repo/path@ref`.
    async fn fetch_reusable(&self, uses: &str, head_sha: &str) -> Result<Vec<JobSpec>> {
        let path = reusable_contents_path(uses, head_sha).ok_or_else(|| CiError::Parse {
            what: "reusable uses".into(),
            message: uses.to_owned(),
        })?;
        let cached = self
            .yaml_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&path).cloned());
        let body = if let Some(body) = cached {
            body
        } else {
            let body = self.api_raw(&path).await?;
            if let Ok(mut cache) = self.yaml_cache.lock() {
                cache.insert(path, body.clone());
            }
            body
        };
        parse_workflow(&body)
    }

    /// Inline each reusable `uses:` job's fetched children with edges rewired
    /// across the boundary; one that can't be fetched stays a single node.
    async fn expand_jobs(
        &self,
        specs: &[JobSpec],
        run_jobs: &[RunJob],
        head_sha: &str,
    ) -> Vec<CiJob> {
        let now = time::OffsetDateTime::now_utc();
        // child node ids scope by the caller's label (the value here), not its
        // id: that's what GitHub prefixes run-job names with, so the ids stay
        // matchable for status and log lookup
        let mut children: HashMap<&str, (&str, Vec<JobSpec>)> = HashMap::new();
        for spec in specs {
            if let Some(uses) = &spec.uses
                && let Ok(fetched) = self.fetch_reusable(uses, head_sha).await
                && !fetched.is_empty()
            {
                children.insert(spec.id.as_str(), (spec.label.as_str(), fetched));
            }
        }

        let mut jobs = Vec::new();
        for spec in specs {
            match children.get(spec.id.as_str()) {
                Some((_, kids)) => {
                    for kid in kids {
                        let id = scope(&spec.label, &kid.id);
                        let status_label = scope(&spec.label, &kid.label);
                        let needs = if kid.needs.is_empty() {
                            spec.needs
                                .iter()
                                .flat_map(|d| resolve_dep(d, &children))
                                .map(JobId)
                                .collect()
                        } else {
                            kid.needs
                                .iter()
                                .map(|n| JobId(scope(&spec.label, n)))
                                .collect()
                        };
                        jobs.push(CiJob {
                            name: child_display(&id, &status_label, run_jobs),
                            status: aggregate_status(&id, &status_label, run_jobs),
                            duration_secs: aggregate_duration(&id, &status_label, run_jobs, now),
                            id: JobId(id),
                            needs,
                        });
                    }
                }
                None => jobs.push(CiJob {
                    id: JobId(spec.id.clone()),
                    name: spec.label.clone(),
                    status: aggregate_status(&spec.id, &spec.label, run_jobs),
                    duration_secs: aggregate_duration(&spec.id, &spec.label, run_jobs, now),
                    needs: spec
                        .needs
                        .iter()
                        .flat_map(|d| resolve_dep(d, &children))
                        .map(JobId)
                        .collect(),
                }),
            }
        }
        jobs
    }

    /// Every name a job node answers to: its own id, plus the `name:` any
    /// bundled workflow gives that id. Two workflows can share an id, and a
    /// label from the wrong one simply matches nothing.
    fn labels_of(&self, id: &str) -> Vec<String> {
        let mut labels = vec![id.to_owned()];
        labels.extend(
            self.workflows
                .iter()
                .filter_map(|yaml| parse_workflow(yaml).ok())
                .flatten()
                .filter(|spec| spec.id == id && spec.label != id)
                .map(|spec| spec.label),
        );
        labels
    }

    async fn artifacts(&self, run: &RunId) -> Result<Vec<Artifact>> {
        let raw = self
            .api(&format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{}/artifacts",
                run.0
            ))
            .await?;
        let list: ArtifactList = parse_json("gh api artifacts", &raw)?;
        Ok(list.artifacts.into_iter().map(ArtifactItem::into).collect())
    }

    async fn annotations(&self, run: &RunId) -> Result<Vec<Annotation>> {
        let raw = self
            .api(&format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{}/jobs",
                run.0
            ))
            .await?;
        let jobs: JobsApi = parse_json("gh api jobs", &raw)?;
        let mut annotations = Vec::new();
        for job in jobs.jobs {
            // one job's annotations 404ing (a GC'd check run) or rate-limiting
            // must not drop every other job's: skip it and keep going
            let Ok(raw) = self
                .api(&format!("{}/annotations", job.check_run_url))
                .await
            else {
                continue;
            };
            let Ok(items) = serde_json::from_str::<Vec<AnnotationItem>>(&raw) else {
                continue;
            };
            annotations.extend(items.into_iter().map(AnnotationItem::into));
        }
        Ok(annotations)
    }
}

#[async_trait]
impl ForgeProvider for GitHubProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitHub
    }

    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>> {
        let branch = self
            .branch
            .as_ref()
            .map(|branch| format!("&branch={branch}"))
            .unwrap_or_default();
        let path = format!("repos/{{owner}}/{{repo}}/actions/runs?per_page={limit}{branch}");
        let out = self.conditional_api(&path).await?;
        let raw: RunsApi = parse_json("gh api actions/runs", &out)?;
        Ok(raw
            .workflow_runs
            .into_iter()
            .map(RunApi::into_run)
            .collect())
    }

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail> {
        let args = [
            "run",
            "view",
            &run.0,
            "--json",
            "jobs,displayTitle,headBranch,headSha,status,conclusion,workflowName,createdAt,url",
        ]
        .map(str::to_owned);
        let out = self.gh(&args).await?;
        let view: RunView = parse_json("gh run view", &out)?;

        // build the DAG from the run's own workflow, matched by the YAML `name:`
        // against the run's `workflowName`; an unmatched run falls back to flat
        let specs = self
            .workflows
            .iter()
            .find(|yaml| workflow_name(yaml).as_deref() == Some(view.workflow_name.as_str()))
            .and_then(|yaml| parse_workflow(yaml).ok())
            .unwrap_or_default();
        let jobs = if specs.is_empty() {
            // no workflow file: a flat, edgeless node per run job
            let now = time::OffsetDateTime::now_utc();
            view.jobs
                .iter()
                .map(|j| CiJob {
                    id: JobId(j.name.clone()),
                    name: j.name.clone(),
                    status: map_status(&j.status, j.conclusion.as_deref()),
                    duration_secs: j.duration_secs(now),
                    needs: Vec::new(),
                })
                .collect()
        } else {
            self.expand_jobs(&specs, &view.jobs, &view.head_sha).await
        };
        Ok(RunDetail {
            run: view.into_run(run.clone()),
            jobs,
        })
    }

    async fn job_log(&self, run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk> {
        // resolve the run-job (matrix jobs expand into several legs; the first
        // matching leg is shown) and its live step states straight from the API
        let out = self
            .api(&format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{}/jobs",
                run.0
            ))
            .await?;
        let view: JobList = parse_json("gh api jobs", &out)?;
        // a node is keyed by its YAML id while the API names the job by its
        // `name:`, so the run job answers to either
        let labels = self.labels_of(&job.0);
        let job = view
            .jobs
            .iter()
            .find(|j| {
                labels
                    .iter()
                    .any(|label| j.name == *label || job_matches(&j.name, &job.0, label))
            })
            .ok_or_else(|| CiError::NotFound(format!("job {} in run {}", job.0, run.0)))?;
        let steps = job.steps.iter().map(RunStep::to_meta).collect();
        // route through the same classifier `steps` was just built from
        // (rather than a raw status literal), so "done" agrees with every
        // other reading of this job's state
        let done = job_finished(&job.status, job.conclusion.as_deref());

        // the log archive (`jobs/{id}/logs`) only exists once the job finishes.
        // it 404s while running. so for an in-progress job, return the live step
        // states with no text and keep polling; the content fills in on completion
        let log_path = format!(
            "repos/{{owner}}/{{repo}}/actions/jobs/{}/logs",
            job.database_id
        );
        match self.api(&log_path).await {
            Ok(full) => {
                // honor `offset` so a re-poll racing `done` yields the tail
                // (empty), never a duplicated transcript
                let mut start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(full.len());
                while start > 0 && !full.is_char_boundary(start) {
                    start -= 1;
                }
                let next_offset = full.len() as u64;
                Ok(LogChunk {
                    text: full.get(start..).unwrap_or_default().to_owned(),
                    steps,
                    next_offset,
                    done,
                })
            }
            Err(_) if !done => Ok(LogChunk {
                text: String::new(),
                steps,
                next_offset: offset,
                done: false,
            }),
            Err(err) => Err(err),
        }
    }

    async fn run_extras(&self, run: &RunId) -> Result<RunExtras> {
        // the extras panel is auxiliary: a forge hiccup degrades a section to
        // empty rather than failing the graph page (and, since the host re-polls
        // extras only while they're absent, rather than re-fetching forever)
        Ok(RunExtras {
            artifacts: self.artifacts(run).await.unwrap_or_default(),
            annotations: self.annotations(run).await.unwrap_or_default(),
        })
    }

    async fn list_prs(&self) -> Result<Vec<PullRequest>> {
        let args = [
            "pr",
            "list",
            "--limit",
            "50",
            "--json",
            "number,title,url,baseRefName,headRefName,headRefOid,author",
        ]
        .map(str::to_owned);
        let raw = self.gh(&args).await?;
        let items: Vec<PrListItem> = parse_json("pr list", &raw)?;
        Ok(items.into_iter().map(PrListItem::into_pr).collect())
    }

    async fn pr_comments(&self, number: u64) -> Result<Vec<PrComment>> {
        let args = [
            "api".to_owned(),
            "--paginate".to_owned(),
            self.scoped(&format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments")),
        ];
        let raw = self.runner.run("gh", &args).await?;
        // `--paginate` concatenates one JSON array per page; stream-parse the
        // documents. A parse error must propagate: an empty fallback would
        // read as "the forge deleted every comment" and wipe synced state.
        let mut items: Vec<ReviewCommentApi> = Vec::new();
        for page in serde_json::Deserializer::from_str(&raw).into_iter::<Vec<ReviewCommentApi>>() {
            items.extend(page.map_err(|err| CiError::Parse {
                what: "pr comments".to_owned(),
                message: err.to_string(),
            })?);
        }
        if items.is_empty() {
            return Ok(Vec::new());
        }
        // thread handles and resolution live only in GraphQL; losing them
        // (query failure) degrades to unresolvable threads, not an error
        let threads = self.review_threads(number).await.unwrap_or_default();
        Ok(items
            .into_iter()
            .map(|item| {
                let thread = threads.get(&item.id).cloned();
                let mut comment = item.into_comment();
                if let Some((thread_id, resolved)) = thread {
                    comment.thread_id = Some(thread_id);
                    comment.resolved = resolved;
                }
                comment
            })
            .collect())
    }

    async fn post_pr_comment(&self, new: &crate::ci::NewPrComment) -> Result<PrComment> {
        let mut args = vec![
            "api".to_owned(),
            "-X".to_owned(),
            "POST".to_owned(),
            self.scoped(&format!(
                "repos/{{owner}}/{{repo}}/pulls/{}/comments",
                new.number
            )),
            "-f".to_owned(),
            format!("body={}", new.body),
            "-f".to_owned(),
            format!("commit_id={}", new.head_oid),
            "-f".to_owned(),
            format!("path={}", new.path),
            "-F".to_owned(),
            format!("line={}", new.line),
            "-f".to_owned(),
            format!("side={}", if new.new_side { "RIGHT" } else { "LEFT" }),
        ];
        if let Some(start) = new.start_line {
            args.push("-F".to_owned());
            args.push(format!("start_line={start}"));
            args.push("-f".to_owned());
            args.push(format!(
                "start_side={}",
                if new.new_side { "RIGHT" } else { "LEFT" }
            ));
        }
        let raw = self.runner.run("gh", &args).await?;
        parse_posted(&raw)
    }

    async fn reply_pr_comment(
        &self,
        number: u64,
        remote_id: &str,
        body: &str,
    ) -> Result<PrComment> {
        let args = [
            "api".to_owned(),
            "-X".to_owned(),
            "POST".to_owned(),
            self.scoped(&format!(
                "repos/{{owner}}/{{repo}}/pulls/{number}/comments/{remote_id}/replies"
            )),
            "-f".to_owned(),
            format!("body={body}"),
        ];
        let raw = self.runner.run("gh", &args).await?;
        parse_posted(&raw)
    }

    async fn submit_pr_review(&self, review: &crate::ci::NewPrReview) -> Result<()> {
        let payload = review_payload(review);
        // gh reads nested JSON bodies from a file; argv fields can't express
        // arrays of objects
        let input = std::env::temp_dir().join(format!("diffler-review-{}.json", review.number));
        std::fs::write(&input, payload.to_string()).map_err(|err| crate::ci::CiError::Exec {
            cmd: "write review payload".to_owned(),
            message: err.to_string(),
        })?;
        let args = [
            "api".to_owned(),
            "-X".to_owned(),
            "POST".to_owned(),
            self.scoped(&format!(
                "repos/{{owner}}/{{repo}}/pulls/{}/reviews",
                review.number
            )),
            "--input".to_owned(),
            input.to_string_lossy().into_owned(),
        ];
        let result = self.runner.run("gh", &args).await;
        let _ = std::fs::remove_file(&input);
        result.map(|_| ())
    }

    async fn resolve_pr_thread(&self, _number: u64, thread_id: &str, resolved: bool) -> Result<()> {
        // resolution is GraphQL-only; REST has no endpoint for it
        let mutation = if resolved {
            "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{id}}}"
        } else {
            "mutation($id:ID!){unresolveReviewThread(input:{threadId:$id}){thread{id}}}"
        };
        let args = [
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("query={mutation}"),
            "-f".to_owned(),
            format!("id={thread_id}"),
        ];
        self.runner.run("gh", &args).await.map(|_| ())
    }

    async fn update_pr_comment(&self, _number: u64, remote_id: &str, body: &str) -> Result<()> {
        let args = [
            "api".to_owned(),
            "-X".to_owned(),
            "PATCH".to_owned(),
            self.scoped(&format!(
                "repos/{{owner}}/{{repo}}/pulls/comments/{remote_id}"
            )),
            "-f".to_owned(),
            format!("body={body}"),
        ];
        self.runner.run("gh", &args).await.map(|_| ())
    }

    async fn delete_pr_comment(&self, _number: u64, remote_id: &str) -> Result<()> {
        let args = [
            "api".to_owned(),
            "-X".to_owned(),
            "DELETE".to_owned(),
            self.scoped(&format!(
                "repos/{{owner}}/{{repo}}/pulls/comments/{remote_id}"
            )),
        ];
        self.runner.run("gh", &args).await.map(|_| ())
    }

    async fn pr(&self, number: u64) -> Result<PullRequest> {
        let args = [
            "pr".to_owned(),
            "view".to_owned(),
            number.to_string(),
            "--json".to_owned(),
            "number,title,url,baseRefName,headRefName,headRefOid,author".to_owned(),
        ];
        let raw = self.gh(&args).await?;
        let pr: PrListItem = parse_json("pr", &raw)?;
        Ok(pr.into_pr())
    }

    async fn create_pr(&self, new: &crate::ci::NewPullRequest) -> Result<PullRequest> {
        let mut args = vec![
            "pr".to_owned(),
            "create".to_owned(),
            "--base".to_owned(),
            new.base.clone(),
            "--head".to_owned(),
            new.head.clone(),
            "--title".to_owned(),
            new.title.clone(),
            "--body".to_owned(),
            new.body.clone(),
        ];
        if new.draft {
            args.push("--draft".to_owned());
        }
        let raw = self.gh(&args).await?;
        // the command answers with the new PR's url and nothing machine-readable
        let number = pr_number_from_url(&raw).ok_or_else(|| CiError::Parse {
            what: "pr create".to_owned(),
            message: format!("no pull-request url in the output: {}", raw.trim()),
        })?;
        self.pr(number).await
    }

    async fn current_pr(&self) -> Result<Option<PullRequest>> {
        let Some(branch) = &self.branch else {
            return Ok(None);
        };
        // `gh pr view` exits non-zero when the branch has no PR; that's a normal
        // state, not an error, so a failed call resolves to "no PR"
        let args = [
            "pr",
            "view",
            branch,
            "--json",
            "number,title,url,baseRefName,headRefOid",
        ]
        .map(str::to_owned);
        let Ok(raw) = self.gh(&args).await else {
            return Ok(None);
        };
        // a malformed response must propagate, same as `pr`/`list_prs`:
        // treating it as "no PR" would look like a normal, PR-less branch
        let pr: PrView = parse_json("pr view", &raw)?;
        Ok(Some(PullRequest {
            number: pr.number,
            title: pr.title,
            url: (!pr.url.is_empty()).then_some(pr.url),
            base_ref: pr.base_ref_name,
            head_ref: pr.head_ref_name,
            head_oid: pr.head_ref_oid,
            author: String::new(),
        }))
    }
}

/// A workflow job's structure from the YAML. `uses` is set when the job calls a
/// reusable workflow instead of running steps.
struct JobSpec {
    id: String,
    label: String,
    needs: Vec<String>,
    uses: Option<String>,
}

/// The workflow's display `name:` (what `gh run list` reports as `workflowName`),
/// used to match a run to the YAML that defines its DAG.
fn workflow_name(yaml: &str) -> Option<String> {
    let value: serde_norway::Value = serde_norway::from_str(yaml).ok()?;
    value
        .get("name")
        .and_then(serde_norway::Value::as_str)
        .map(str::to_owned)
}

/// Parse `jobs.<id>` into specs, preserving declaration order; `needs` is a
/// scalar or a sequence of upstream job ids.
fn parse_workflow(yaml: &str) -> Result<Vec<JobSpec>> {
    let value: serde_norway::Value = serde_norway::from_str(yaml).map_err(|e| CiError::Parse {
        what: "workflow YAML".into(),
        message: e.to_string(),
    })?;
    let jobs = value
        .get("jobs")
        .and_then(serde_norway::Value::as_mapping)
        .ok_or_else(|| CiError::Parse {
            what: "workflow YAML".into(),
            message: "no `jobs` mapping".into(),
        })?;

    let mut specs = Vec::new();
    for (key, job) in jobs {
        let Some(id) = key.as_str() else { continue };
        let label = job
            .get("name")
            .and_then(serde_norway::Value::as_str)
            .unwrap_or(id)
            .to_owned();
        specs.push(JobSpec {
            id: id.to_owned(),
            label,
            needs: needs_of(job),
            uses: job
                .get("uses")
                .and_then(serde_norway::Value::as_str)
                .map(str::to_owned),
        });
    }
    Ok(specs)
}

fn needs_of(job: &serde_norway::Value) -> Vec<String> {
    match job.get("needs") {
        Some(serde_norway::Value::String(one)) => vec![one.clone()],
        Some(serde_norway::Value::Sequence(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// GitHub's naming for a reusable workflow's job, under its caller.
fn scope(caller: &str, child: &str) -> String {
    format!("{caller} / {child}")
}

/// The `gh api` contents path for a `uses:` target: a local `./path` resolved
/// at the run's commit, or a remote `owner/repo/path@ref`. `None` if malformed.
fn reusable_contents_path(uses: &str, head_sha: &str) -> Option<String> {
    if let Some(local) = uses.strip_prefix("./") {
        return Some(format!(
            "repos/{{owner}}/{{repo}}/contents/{local}?ref={head_sha}"
        ));
    }
    let (path, git_ref) = uses.rsplit_once('@')?;
    let mut segments = path.splitn(3, '/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    let file = segments.next()?;
    Some(format!(
        "repos/{owner}/{repo}/contents/{file}?ref={git_ref}"
    ))
}

/// The terminal children of an expanded caller (those no sibling needs), so a
/// downstream dependent attaches to the reusable workflow's exit, not its entry.
fn reusable_terminals(caller: &str, children: &[JobSpec]) -> Vec<String> {
    let needed: HashSet<&str> = children
        .iter()
        .flat_map(|c| c.needs.iter().map(String::as_str))
        .collect();
    children
        .iter()
        .filter(|c| !needed.contains(c.id.as_str()))
        .map(|c| scope(caller, &c.id))
        .collect()
}

/// Resolve one `needs` entry to the node ids satisfying it: an expanded caller's
/// terminal children (scoped by its label, matching their node ids), or the
/// dependency unchanged.
fn resolve_dep(dep: &str, expanded: &HashMap<&str, (&str, Vec<JobSpec>)>) -> Vec<String> {
    match expanded.get(dep) {
        Some((label, children)) => reusable_terminals(label, children),
        None => vec![dep.to_owned()],
    }
}

/// The child's run-job name (resolves a `${{ }}` `name:` to its runtime value),
/// or the scoped id before the job exists.
fn child_display(scoped_id: &str, scoped_label: &str, jobs: &[RunJob]) -> String {
    if scoped_label.contains("${{") {
        return jobs
            .iter()
            .find(|j| job_matches(&j.name, scoped_id, scoped_label))
            .map_or_else(|| scoped_id.to_owned(), |j| j.name.clone());
    }
    scoped_label.to_owned()
}

/// Whether a run job belongs to a spec. Beyond an exact name/id match this
/// covers a matrix leg (`name (os)`), a reusable child (`caller / child`, with
/// further ` / ` for nested calls), and a `${{ }}` name (matched by its prefix).
fn name_matches(run_job_name: &str, candidate: &str) -> bool {
    if let Some((prefix, _)) = candidate.split_once("${{") {
        return !prefix.is_empty() && run_job_name.starts_with(prefix);
    }
    run_job_name == candidate
        || run_job_name.starts_with(&format!("{candidate} ("))
        || run_job_name.starts_with(&format!("{candidate} / "))
}

fn job_matches(run_job_name: &str, id: &str, label: &str) -> bool {
    name_matches(run_job_name, label) || name_matches(run_job_name, id)
}

/// The longest leg's time, so a matrix node reads as the wall clock its slowest
/// member spent.
fn aggregate_duration(
    id: &str,
    label: &str,
    jobs: &[RunJob],
    now: time::OffsetDateTime,
) -> Option<i64> {
    jobs.iter()
        .filter(|j| job_matches(&j.name, id, label))
        .filter_map(|j| j.duration_secs(now))
        .max()
}

fn aggregate_status(id: &str, label: &str, jobs: &[RunJob]) -> JobStatus {
    jobs.iter()
        .filter(|j| job_matches(&j.name, id, label))
        .map(|j| map_status(&j.status, j.conclusion.as_deref()))
        .reduce(JobStatus::worse)
        .unwrap_or(JobStatus::Queued)
}

fn map_status(status: &str, conclusion: Option<&str>) -> JobStatus {
    // GitHub and Forgejo Actions share the same `conclusion` vocabulary
    // (`crate::ci::map_conclusion` covers both); only the in-progress/no-conclusion
    // status strings are forge-specific
    crate::ci::map_conclusion(conclusion).unwrap_or(match status {
        "in_progress" => JobStatus::Running,
        "completed" => JobStatus::Neutral,
        _ => JobStatus::Queued,
    })
}

/// Whether a job has reached a terminal state, classified the same way as
/// every other reading of `status`/`conclusion` in this file, not a raw
/// string comparison, which would drift the moment a new terminal status or
/// conclusion is added to `map_status`.
fn job_finished(status: &str, conclusion: Option<&str>) -> bool {
    matches!(
        map_status(status, conclusion),
        JobStatus::Ok | JobStatus::Failed | JobStatus::Skipped | JobStatus::Neutral
    )
}

fn parse_created(raw: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

/// Split a `gh api -i` response into its status code, header lines and body.
fn split_response(raw: &str) -> (Option<u16>, Vec<&str>, String) {
    let mut lines = raw.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok());
    let mut headers = Vec::new();
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            break;
        }
        headers.push(line);
    }
    (status, headers, lines.collect::<Vec<_>>().join("\n"))
}

fn header(headers: &[&str], name: &str) -> Option<String> {
    headers.iter().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

/// The REST runs response. GitHub names these fields differently from
/// `gh run list --json`, and carries both a `url` and an `html_url`, so it
/// gets its own shape rather than aliases onto [`RunListItem`].
#[derive(Deserialize)]
struct RunsApi {
    workflow_runs: Vec<RunApi>,
}

#[derive(Deserialize)]
struct RunApi {
    id: u64,
    name: Option<String>,
    display_title: String,
    head_branch: String,
    head_sha: String,
    status: String,
    conclusion: Option<String>,
    created_at: String,
    html_url: String,
}

impl RunApi {
    fn into_run(self) -> CiRun {
        let workflow = self.name.unwrap_or_default();
        let name = if workflow.is_empty() {
            self.display_title.clone()
        } else {
            workflow
        };
        CiRun {
            id: RunId(self.id.to_string()),
            name,
            title: self.display_title,
            branch: self.head_branch,
            commit: self.head_sha,
            author: String::new(),
            created: parse_created(&self.created_at),
            status: map_status(&self.status, self.conclusion.as_deref()),
            url: Some(self.html_url),
            remote: None,
        }
    }
}

/// The jobs array alone (from the REST `actions/runs/{id}/jobs` response, whose
/// `total_count` is ignored): the run meta in [`RunView`] isn't needed for logs.
#[derive(Deserialize)]
struct JobList {
    jobs: Vec<RunJob>,
}

#[derive(Deserialize)]
struct RunView {
    jobs: Vec<RunJob>,
    #[serde(rename = "displayTitle")]
    display_title: String,
    #[serde(rename = "headBranch")]
    head_branch: String,
    #[serde(rename = "headSha")]
    head_sha: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "workflowName")]
    workflow_name: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    url: String,
}

impl RunView {
    fn into_run(self, id: RunId) -> CiRun {
        CiRun {
            id,
            name: self.workflow_name,
            title: self.display_title,
            branch: self.head_branch,
            commit: self.head_sha,
            author: String::new(),
            created: parse_created(&self.created_at),
            status: map_status(&self.status, self.conclusion.as_deref()),
            url: Some(self.url),
            remote: None,
        }
    }
}

// Parses both `gh run view --json jobs` (camelCase) and the REST jobs API
// (snake_case `id`/`started_at`/…), so the same shape serves the DAG and logs.
#[derive(Deserialize)]
struct RunJob {
    #[serde(rename = "databaseId", alias = "id")]
    database_id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "startedAt", alias = "started_at")]
    started_at: Option<String>,
    #[serde(rename = "completedAt", alias = "completed_at")]
    completed_at: Option<String>,
    #[serde(default)]
    steps: Vec<RunStep>,
}

impl RunJob {
    /// Time on the clock: finished jobs report their span, a running one
    /// counts from its start.
    fn duration_secs(&self, now: time::OffsetDateTime) -> Option<i64> {
        // GitHub writes an unset time as its zero value (`0001-01-01…`), which
        // parses, so both ends need the year to tell "not yet" from a real stamp
        let real = |raw: &Option<String>| {
            raw.as_deref()
                .and_then(parse_created)
                .filter(|at| at.year() >= 2000)
        };
        let started = real(&self.started_at)?;
        let end = real(&self.completed_at).unwrap_or(now);
        Some((end - started).whole_seconds().max(0))
    }
}

#[derive(Deserialize)]
struct RunStep {
    name: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "startedAt", alias = "started_at")]
    started_at: Option<String>,
    #[serde(rename = "completedAt", alias = "completed_at")]
    completed_at: Option<String>,
}

impl RunStep {
    fn to_meta(&self) -> LogStepMeta {
        let started = self.started_at.as_deref().and_then(parse_created);
        let dur = started
            .zip(self.completed_at.as_deref().and_then(parse_created))
            .map(|(start, end)| (end - start).whole_seconds());
        // a skipped/not-started step gets key 0 so it claims no log lines: GitHub
        // gives those a null or zero (`0001-…`) start that would otherwise sort
        // below real steps and, mid-list, swallow an earlier step's output
        let ran = started.is_some_and(|t| t.year() >= 2000);
        LogStepMeta {
            name: self.name.clone(),
            status: map_status(&self.status, self.conclusion.as_deref()),
            start_key: if ran {
                self.started_at.as_deref().map_or(0, ts_sort_key)
            } else {
                0
            },
            duration_secs: dur,
        }
    }
}

#[derive(Deserialize)]
struct ArtifactList {
    artifacts: Vec<ArtifactItem>,
}

#[derive(Deserialize)]
struct ArtifactItem {
    name: String,
    #[serde(rename = "size_in_bytes")]
    size_in_bytes: u64,
    expired: bool,
}

impl From<ArtifactItem> for Artifact {
    fn from(item: ArtifactItem) -> Self {
        Artifact {
            name: item.name,
            size_bytes: item.size_in_bytes,
            expired: item.expired,
        }
    }
}

/// The REST jobs response (`actions/runs/{id}/jobs`). It carries each job's
/// `check_run_url`, the handle the annotations endpoint hangs off.
/// `gh run view --json jobs` omits that field.
#[derive(Deserialize)]
struct JobsApi {
    jobs: Vec<JobApi>,
}

fn parse_posted(raw: &str) -> Result<PrComment> {
    let item: ReviewCommentApi = parse_json("pr comment", raw)?;
    Ok(item.into_comment())
}

/// One review comment from the REST API (list and post share the shape).
#[derive(Deserialize)]
struct ReviewCommentApi {
    id: u64,
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    original_line: Option<u32>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    original_start_line: Option<u32>,
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    user: UserApi,
    #[serde(default)]
    in_reply_to_id: Option<u64>,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct RepoSlug {
    owner: RepoOwner,
    name: String,
}

#[derive(Deserialize)]
struct RepoOwner {
    login: String,
}

#[derive(Deserialize)]
struct ThreadsPage {
    data: ThreadsData,
}

#[derive(Deserialize)]
struct ThreadsData {
    repository: ThreadsRepo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsRepo {
    pull_request: ThreadsPr,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsPr {
    review_threads: ThreadsConn,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsConn {
    page_info: ThreadsPageInfo,
    nodes: Vec<ThreadNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadNode {
    id: String,
    is_resolved: bool,
    comments: ThreadComments,
}

#[derive(Deserialize)]
struct ThreadComments {
    nodes: Vec<ThreadCommentNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadCommentNode {
    database_id: u64,
}

#[derive(Deserialize, Default)]
struct UserApi {
    #[serde(default)]
    login: String,
}

impl ReviewCommentApi {
    fn into_comment(self) -> PrComment {
        PrComment {
            id: self.id.to_string(),
            path: self.path,
            line: self.line.or(self.original_line),
            start_line: self.start_line.or(self.original_start_line),
            new_side: self.side.as_deref() != Some("LEFT"),
            body: self.body,
            author: self.user.login,
            reply_to: self.in_reply_to_id.map(|id| id.to_string()),
            thread_id: None,
            resolved: false,
            at: created_epoch(&self.created_at),
        }
    }
}

/// A single line (or range) comment in the GitHub review-submission wire shape.
#[derive(Serialize)]
struct ReviewCommentPayload {
    path: String,
    line: u32,
    side: &'static str,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_side: Option<&'static str>,
}

/// The REST body for POST /pulls/N/reviews: verdict as the event, the
/// optional summary, every pending comment with its side (and range when
/// multi-line).
#[derive(Serialize)]
struct ReviewPayload {
    commit_id: String,
    event: &'static str,
    comments: Vec<ReviewCommentPayload>,
    #[serde(skip_serializing_if = "String::is_empty")]
    body: String,
}

fn review_payload(review: &crate::ci::NewPrReview) -> serde_json::Value {
    let comments = review
        .comments
        .iter()
        .map(|c| {
            let side = if c.new_side { "RIGHT" } else { "LEFT" };
            ReviewCommentPayload {
                path: c.path.clone(),
                line: c.line,
                side,
                body: c.body.clone(),
                start_line: c.start_line,
                start_side: c.start_line.is_some().then_some(side),
            }
        })
        .collect();
    let event = match review.verdict {
        crate::ci::ReviewVerdict::Approve => "APPROVE",
        crate::ci::ReviewVerdict::RequestChanges => "REQUEST_CHANGES",
        crate::ci::ReviewVerdict::Comment => "COMMENT",
    };
    let payload = ReviewPayload {
        commit_id: review.head_oid.clone(),
        event,
        comments,
        body: review.body.clone(),
    };
    // every field is a plain String/number/&'static str, so serialization
    // can never fail
    #[allow(clippy::expect_used)]
    serde_json::to_value(payload).expect("wire payload of plain fields always serializes")
}

/// The number from a pull-request url anywhere in `output`, so the url the
/// create command prints can be turned back into a PR to open.
fn pr_number_from_url(output: &str) -> Option<u64> {
    output.split_whitespace().rev().find_map(|token| {
        let (_, tail) = token.rsplit_once("/pull/")?;
        tail.trim_end_matches('/').parse().ok()
    })
}

/// ISO-8601 → unix seconds; an unrecognized shape collapses to zero.
fn created_epoch(iso: &str) -> u64 {
    parse_created(iso)
        .and_then(|t| u64::try_from(t.unix_timestamp()).ok())
        .unwrap_or(0)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrListItem {
    number: u64,
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    author: AuthorApi,
}

#[derive(Deserialize, Default)]
struct AuthorApi {
    #[serde(default)]
    login: String,
}

impl PrListItem {
    fn into_pr(self) -> PullRequest {
        PullRequest {
            number: self.number,
            title: self.title,
            url: (!self.url.is_empty()).then_some(self.url),
            base_ref: self.base_ref_name,
            head_ref: self.head_ref_name,
            head_oid: self.head_ref_oid,
            author: self.author.login,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrView {
    number: u64,
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: String,
}

#[derive(Deserialize)]
struct JobApi {
    check_run_url: String,
}

#[derive(Deserialize)]
struct AnnotationItem {
    annotation_level: Option<String>,
    title: Option<String>,
    message: Option<String>,
    path: Option<String>,
    start_line: Option<u64>,
}

impl From<AnnotationItem> for Annotation {
    fn from(item: AnnotationItem) -> Self {
        let level = match item.annotation_level.as_deref() {
            Some("failure") => AnnotationLevel::Failure,
            Some("warning") => AnnotationLevel::Warning,
            _ => AnnotationLevel::Notice,
        };
        Annotation {
            level,
            title: item.title.unwrap_or_default(),
            message: item.message.unwrap_or_default(),
            path: item.path.unwrap_or_default(),
            start_line: item.start_line,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;
    use crate::ci::model::{DagSource, LogMode};

    const WORKFLOW: &str = r"
name: CI
on: push
jobs:
  lint:
    runs-on: ubuntu-latest
  test:
    needs: lint
    runs-on: ubuntu-latest
  publish:
    name: Publish
    needs: [lint, test]
    runs-on: ubuntu-latest
";

    const RELEASE_WORKFLOW: &str = r"
name: Release
on: push
jobs:
  publish:
    runs-on: ubuntu-latest
";

    // caller with a reusable `deploy` job, and the reusable workflow it fetches,
    // mirrors a real deploy pipeline (a nested `uses:` and a `${{ }}` job name)
    const DEPLOY_WORKFLOW: &str = r"
name: Auth Service Deploy
on: push
jobs:
  audit:
    name: Audit dependencies
    runs-on: ubuntu-latest
  deploy:
    name: Build and deploy
    needs: audit
    uses: syte-tech/syte-ci-tooling/.github/workflows/app-deploy.yml@main
";

    const APP_DEPLOY_WORKFLOW: &str = r"
name: Build and Deploy Application
on:
  workflow_call:
jobs:
  prepare-deployment:
    runs-on: ubuntu-latest
  build-and-push:
    needs: [prepare-deployment]
    uses: syte-tech/syte-ci-tooling/.github/workflows/docker-build-push.yml@main
  deploy:
    name: Deploy to ${{ needs.prepare-deployment.outputs.env }}
    needs: [prepare-deployment, build-and-push]
    runs-on: ubuntu-latest
";

    /// A REST runs payload with one completed run on `branch`.
    fn runs_body(branch: &str) -> String {
        format!(
            r#"{{"total_count":1,"workflow_runs":[
              {{"id":42,"name":"CI","display_title":"fix things","head_branch":"{branch}",
               "head_sha":"abc1234","status":"completed","conclusion":"success",
               "created_at":"2026-06-18T10:00:00Z","url":"https://api/runs/42",
               "html_url":"https://gh/run/42"}}]}}"#
        )
    }

    fn ok_response(body: &str, etag: &str) -> String {
        format!("HTTP/2.0 200 OK\r\nETag: {etag}\r\nContent-Type: application/json\r\n\r\n{body}")
    }

    fn provider(responses: &[(&'static str, &str)]) -> GitHubProvider {
        GitHubProvider::new(
            Box::new(RecordingRunner::new(responses)),
            vec![WORKFLOW.to_owned()],
            None,
            YamlCache::default(),
            EtagCache::default(),
            None,
        )
    }

    #[tokio::test]
    async fn list_runs_scopes_to_the_branch() {
        // the response only matches if the branch reached the query string
        let runs = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[(
                "branch=feat/x",
                &ok_response(&runs_body("feat/x"), "W/\"a\""),
            )])),
            vec![WORKFLOW.to_owned()],
            Some("feat/x".to_owned()),
            YamlCache::default(),
            EtagCache::default(),
            None,
        )
        .list_runs(10)
        .await
        .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].branch, "feat/x");
    }

    /// `gh` resolves a fork to the repo it was forked from, so a checkout with
    /// `origin` and `upstream` would report the parent's runs. Every call names
    /// the repo diffler picked instead.
    #[tokio::test]
    async fn every_call_names_the_repo_the_remote_points_at() {
        let runner = std::sync::Arc::new(RecordingRunner::new(&[
            (
                "repos/mine/widgets/actions/runs",
                &ok_response(&runs_body("main"), "W/\"a\""),
            ),
            (
                "run view",
                r#"{"displayTitle":"cut","headBranch":"main","headSha":"abc",
                   "status":"completed","conclusion":"success","workflowName":"CI",
                   "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/1","jobs":[]}"#,
            ),
            ("pr list", "[]"),
            (
                "pr view",
                r#"{"number":7,"title":"t","url":"https://gh/pr/7","baseRefName":"main",
                   "headRefName":"feat","headRefOid":"abc","author":{"login":"reviewer"}}"#,
            ),
            ("pr create", "https://gh/mine/widgets/pull/8"),
        ]));
        let provider = |branch: Option<&str>| {
            GitHubProvider::new(
                Box::new(runner.clone()),
                vec![WORKFLOW.to_owned()],
                branch.map(str::to_owned),
                YamlCache::default(),
                EtagCache::default(),
                Some("mine/widgets".to_owned()),
            )
        };
        provider(None).list_runs(10).await.expect("runs");
        provider(None)
            .run_detail(&RunId("1".to_owned()))
            .await
            .expect("detail");
        provider(None).list_prs().await.expect("prs");
        provider(None).pr(7).await.expect("pr");
        provider(Some("feat")).current_pr().await.expect("current");
        provider(None)
            .create_pr(&crate::ci::NewPullRequest {
                base: "main".to_owned(),
                head: "feat".to_owned(),
                title: "t".to_owned(),
                body: String::new(),
                draft: false,
            })
            .await
            .expect("create");

        let calls = runner.calls();
        let api = calls
            .iter()
            .find(|c| c.contains("actions/runs"))
            .expect("api");
        assert!(
            api.contains("repos/mine/widgets/") && !api.contains("{owner}"),
            "the api path names the repo: {api}"
        );
        // every subcommand, not a sample of them: an unscoped one resolves to
        // the fork's parent and answers about the wrong repository
        let subcommands: Vec<&String> = calls.iter().filter(|c| !c.starts_with("api")).collect();
        assert_eq!(subcommands.len(), 6, "{subcommands:?}");
        for call in subcommands {
            assert!(
                call.contains("-R mine/widgets"),
                "the call names the repo: {call}"
            );
        }
    }

    /// Without a parsable remote the calls go out as they always did, and `gh`
    /// resolves the repo from the checkout.
    #[tokio::test]
    async fn an_unknown_remote_leaves_the_repo_to_gh() {
        let runner = std::sync::Arc::new(RecordingRunner::new(&[(
            "repos/{owner}/{repo}/actions/runs",
            &ok_response(&runs_body("main"), "W/\"a\""),
        )]));
        GitHubProvider::new(
            Box::new(runner.clone()),
            vec![WORKFLOW.to_owned()],
            None,
            YamlCache::default(),
            EtagCache::default(),
            None,
        )
        .list_runs(10)
        .await
        .expect("runs");
        let call = runner.calls().first().cloned().unwrap_or_default();
        assert!(call.contains("{owner}"), "left for gh to expand: {call}");
        assert!(!call.contains("-R"), "{call}");
    }

    #[test]
    fn a_running_job_counts_from_its_start() {
        let now = parse_created("2026-06-20T00:05:00Z").expect("now");
        let job = |started: &str, completed: &str| RunJob {
            database_id: 1,
            name: "lint".into(),
            status: "in_progress".into(),
            conclusion: None,
            started_at: Some(started.to_owned()),
            // an unfinished job carries GitHub's zero time, not a null
            completed_at: Some(completed.to_owned()),
            steps: Vec::new(),
        };

        let running = job("2026-06-20T00:00:00Z", "0001-01-01T00:00:00Z");
        assert_eq!(
            running.duration_secs(now),
            Some(300),
            "a job still going reads as the time it has taken so far"
        );

        let done = job("2026-06-20T00:00:00Z", "2026-06-20T00:00:42Z");
        assert_eq!(done.duration_secs(now), Some(42));

        let waiting = job("0001-01-01T00:00:00Z", "0001-01-01T00:00:00Z");
        assert_eq!(
            waiting.duration_secs(now),
            None,
            "a job that has not started has no time to show"
        );
    }

    #[tokio::test]
    async fn run_detail_builds_the_dag_from_the_runs_own_workflow() {
        // the run is a `Release` run; its DAG must come from the Release YAML
        // (one `publish` job), not the CI YAML (lint/test/publish)
        let view = r#"{
          "displayTitle":"cut","headBranch":"main","headSha":"abc","status":"completed",
          "conclusion":"success","workflowName":"Release",
          "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/9",
          "jobs":[{"databaseId":1,"name":"publish","status":"completed","conclusion":"success"}]
        }"#;
        let detail = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[("run view", view)])),
            vec![WORKFLOW.to_owned(), RELEASE_WORKFLOW.to_owned()],
            None,
            YamlCache::default(),
            EtagCache::default(),
            None,
        )
        .run_detail(&RunId("9".into()))
        .await
        .expect("detail");
        let ids: Vec<&str> = detail.jobs.iter().map(|j| j.id.0.as_str()).collect();
        assert_eq!(ids, ["publish"], "matched the Release workflow, not CI");
    }

    #[tokio::test]
    async fn an_unchanged_poll_replays_the_cached_runs_and_sends_the_etag() {
        let runner = std::sync::Arc::new(RecordingRunner::new(&[
            ("If-None-Match", "HTTP/2.0 304 Not Modified\r\n\r\n"),
            (
                "actions/runs",
                &ok_response(&runs_body("main"), "W/\"tag1\""),
            ),
        ]));
        let etags = EtagCache::default();
        let build = || {
            GitHubProvider::new(
                Box::new(runner.clone()),
                vec![WORKFLOW.to_owned()],
                None,
                YamlCache::default(),
                etags.clone(),
                None,
            )
        };
        // first poll stores the etag, second is answered 304 from the cache
        let first = build().list_runs(10).await.expect("first");
        let second = build().list_runs(10).await.expect("second");
        assert_eq!(first, second, "a 304 yields the body we already had");
        assert_eq!(second.len(), 1);
        let sent = runner.calls();
        assert!(
            sent[0].contains("actions/runs") && !sent[0].contains("If-None-Match"),
            "nothing to be conditional about yet: {:?}",
            sent[0]
        );
        assert!(
            sent[1].contains("If-None-Match: W/\"tag1\""),
            "the second poll offers the etag: {:?}",
            sent[1]
        );
    }

    #[tokio::test]
    async fn a_failed_conditional_request_is_an_error_not_an_empty_list() {
        let err = provider(&[("actions/runs", "HTTP/2.0 401 Unauthorized\r\n\r\n")])
            .list_runs(10)
            .await;
        assert!(err.is_err(), "a 401 must not read as zero runs");
    }

    #[tokio::test]
    async fn list_runs_parses_the_rest_payload() {
        let runs = provider(&[("actions/runs", &ok_response(&runs_body("main"), "W/\"a\""))])
            .list_runs(10)
            .await
            .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, RunId("42".into()));
        assert_eq!(runs[0].name, "CI");
        assert_eq!(runs[0].branch, "main");
        assert_eq!(runs[0].status, JobStatus::Ok);
        assert_eq!(runs[0].url.as_deref(), Some("https://gh/run/42"));
        assert!(runs[0].created.is_some());
    }

    #[tokio::test]
    async fn run_detail_builds_dag_with_matrix_aggregation() {
        let view = r#"{
          "displayTitle":"fix things","headBranch":"main","headSha":"abc","status":"in_progress",
          "conclusion":null,"workflowName":"CI",
          "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/42",
          "jobs":[
            {"databaseId":1,"name":"lint","status":"completed","conclusion":"success"},
            {"databaseId":2,"name":"test (ubuntu-latest)","status":"completed","conclusion":"success"},
            {"databaseId":3,"name":"test (windows-latest)","status":"in_progress","conclusion":null}
          ]
        }"#;
        let detail = provider(&[("run view", view)])
            .run_detail(&RunId("42".into()))
            .await
            .expect("detail");
        let ids: Vec<&str> = detail.jobs.iter().map(|j| j.id.0.as_str()).collect();
        assert_eq!(ids, ["lint", "test", "publish"]);
        assert_eq!(detail.jobs[2].name, "Publish");
        assert_eq!(
            detail.jobs[2].needs,
            vec![JobId("lint".into()), JobId("test".into())]
        );
        assert_eq!(detail.jobs[0].status, JobStatus::Ok, "lint succeeded");
        assert_eq!(detail.jobs[1].status, JobStatus::Running, "a test leg runs");
        assert_eq!(
            detail.jobs[2].status,
            JobStatus::Queued,
            "publish not started"
        );
    }

    #[tokio::test]
    async fn run_detail_inlines_a_reusable_workflows_jobs_with_edges() {
        let view = r#"{
          "displayTitle":"deploy","headBranch":"main","headSha":"abc","status":"in_progress",
          "conclusion":null,"workflowName":"Auth Service Deploy",
          "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/9",
          "jobs":[
            {"databaseId":1,"name":"Audit dependencies","status":"completed","conclusion":"success"},
            {"databaseId":2,"name":"Build and deploy / prepare-deployment","status":"completed","conclusion":"success"},
            {"databaseId":3,"name":"Build and deploy / build-and-push / build-and-push-to-registry","status":"in_progress","conclusion":null},
            {"databaseId":4,"name":"Build and deploy / Deploy to staging","status":"queued","conclusion":null}
          ]
        }"#;
        let detail = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[
                ("run view", view),
                (
                    "contents/.github/workflows/app-deploy.yml",
                    APP_DEPLOY_WORKFLOW,
                ),
            ])),
            vec![DEPLOY_WORKFLOW.to_owned()],
            None,
            YamlCache::default(),
            EtagCache::default(),
            None,
        )
        .run_detail(&RunId("9".into()))
        .await
        .expect("detail");

        let by_id = |id: &str| detail.jobs.iter().find(|j| j.id.0 == id).cloned();
        let ids: Vec<&str> = detail.jobs.iter().map(|j| j.id.0.as_str()).collect();
        // node ids scope by the caller's label, matching GitHub's run-job names
        assert_eq!(
            ids,
            [
                "audit",
                "Build and deploy / prepare-deployment",
                "Build and deploy / build-and-push",
                "Build and deploy / deploy"
            ],
            "the reusable `deploy` job is replaced by the fetched workflow's jobs"
        );

        // the entry child inherits the caller's upstream (`audit`)
        assert_eq!(
            by_id("Build and deploy / prepare-deployment")
                .unwrap()
                .needs,
            vec![JobId("audit".into())]
        );
        // internal edges from the reusable workflow's own `needs`
        assert_eq!(
            by_id("Build and deploy / deploy").unwrap().needs,
            vec![
                JobId("Build and deploy / prepare-deployment".into()),
                JobId("Build and deploy / build-and-push".into())
            ]
        );
        // a `${{ }}` job name resolves to its run-job value
        assert_eq!(
            by_id("Build and deploy / deploy").unwrap().name,
            "Build and deploy / Deploy to staging"
        );
        // a nested reusable child takes the worst status of its run legs
        assert_eq!(
            by_id("Build and deploy / build-and-push").unwrap().status,
            JobStatus::Running
        );
        assert_eq!(
            by_id("Build and deploy / prepare-deployment")
                .unwrap()
                .status,
            JobStatus::Ok
        );
        assert_eq!(by_id("audit").unwrap().status, JobStatus::Ok);
    }

    #[tokio::test]
    async fn reusable_workflows_fetch_once_across_provider_rebuilds() {
        let view = r#"{
          "displayTitle":"deploy","headBranch":"main","headSha":"abc","status":"in_progress",
          "conclusion":null,"workflowName":"Auth Service Deploy",
          "createdAt":"2026-06-18T10:00:00Z","url":"https://gh/run/9","jobs":[]
        }"#;
        let runner = std::sync::Arc::new(RecordingRunner::new(&[
            ("run view", view),
            (
                "contents/.github/workflows/app-deploy.yml",
                APP_DEPLOY_WORKFLOW,
            ),
        ]));
        let cache = YamlCache::default();
        for _ in 0..2 {
            GitHubProvider::new(
                Box::new(runner.clone()),
                vec![DEPLOY_WORKFLOW.to_owned()],
                None,
                cache.clone(),
                EtagCache::default(),
                None,
            )
            .run_detail(&RunId("9".into()))
            .await
            .expect("detail");
        }
        let fetches = runner
            .calls()
            .iter()
            .filter(|c| c.contains("contents/"))
            .count();
        assert_eq!(fetches, 1, "second poll served from the cache");
    }

    #[tokio::test]
    async fn job_log_resolves_an_inlined_reusable_child_node() {
        // the node id is the label-scoped child; the run job is the nested leaf,
        // matched by the `caller / child / ...` prefix
        let jobs = r#"{"jobs":[{"id":7,
            "name":"Build and deploy / build-and-push / build-and-push-to-registry",
            "status":"completed","conclusion":"success","steps":[]}]}"#;
        let chunk = provider(&[("runs/9/jobs", jobs), ("/logs", "pushed\n")])
            .job_log(
                &RunId("9".into()),
                &JobId("Build and deploy / build-and-push".into()),
                0,
            )
            .await
            .expect("log resolves for an inlined child");
        assert!(chunk.text.contains("pushed"));
    }

    #[tokio::test]
    async fn job_log_fetches_a_completed_job_from_the_rest_api() {
        // the REST jobs response uses `id` (not `databaseId`): the alias covers it
        let jobs = r#"{"jobs":[{"id":7,"name":"lint","status":"completed","conclusion":"success",
            "steps":[{"name":"Run x","status":"completed","conclusion":"success",
                      "started_at":"2026-06-20T00:00:00Z","completed_at":"2026-06-20T00:00:03Z"}]}]}"#;
        let chunk = provider(&[("runs/42/jobs", jobs), ("/logs", "line one\nline two\n")])
            .job_log(&RunId("42".into()), &JobId("lint".into()), 0)
            .await
            .expect("log");
        assert!(chunk.text.contains("line one"));
        assert!(chunk.done);
        assert_eq!(chunk.steps.len(), 1);
        assert_eq!(chunk.steps[0].duration_secs, Some(3));
        assert_eq!(chunk.next_offset, chunk.text.len() as u64);
    }

    #[tokio::test]
    async fn job_log_finds_a_job_the_workflow_renamed() {
        // the node is keyed `publish`; the run job answers to `Publish`, the
        // workflow's `name:` for it
        let jobs = r#"{"jobs":[{"id":9,"name":"Publish","status":"completed","conclusion":"success",
            "steps":[]}]}"#;
        let chunk = provider(&[("runs/42/jobs", jobs), ("/logs", "shipped\n")])
            .job_log(&RunId("42".into()), &JobId("publish".into()), 0)
            .await
            .expect("log");
        assert!(chunk.text.contains("shipped"), "{chunk:?}");
    }

    #[tokio::test]
    async fn job_log_in_progress_returns_live_steps_without_text() {
        // the log archive 404s mid-run (here: no `/logs` response, so empty); the
        // job stays in_progress → live steps but no text, and polling continues
        let jobs = r#"{"jobs":[{"id":7,"name":"lint","status":"in_progress","conclusion":null,
            "steps":[{"name":"Run x","status":"in_progress","conclusion":null,
                      "started_at":"2026-06-20T00:00:00Z","completed_at":null}]}]}"#;
        let chunk = provider(&[("runs/42/jobs", jobs)])
            .job_log(&RunId("42".into()), &JobId("lint".into()), 0)
            .await
            .expect("log");
        assert!(chunk.text.is_empty(), "no log archive while running");
        assert!(!chunk.done, "keep polling until the job completes");
        assert_eq!(chunk.steps.len(), 1, "live step states are shown");
        assert_eq!(chunk.steps[0].status, JobStatus::Running);
    }

    #[tokio::test]
    async fn run_extras_collects_artifacts_and_annotations() {
        let artifacts = r#"{"artifacts":[
            {"name":"coverage","size_in_bytes":2048,"expired":false},
            {"name":"old-logs","size_in_bytes":10,"expired":true}
        ]}"#;
        let jobs = r#"{"jobs":[
            {"check_run_url":"https://api.github.com/repos/o/r/check-runs/99"}
        ]}"#;
        let annotations = r#"[
            {"annotation_level":"warning","title":"clippy","message":"unused import",
             "path":"src/lib.rs","start_line":12},
            {"annotation_level":"failure","title":"test","message":"assert failed",
             "path":"src/x.rs","start_line":null}
        ]"#;
        let extras = provider(&[
            ("artifacts", artifacts),
            ("/jobs", jobs),
            ("annotations", annotations),
        ])
        .run_extras(&RunId("42".into()))
        .await
        .expect("extras");
        assert_eq!(extras.artifacts.len(), 2);
        assert_eq!(extras.artifacts[0].name, "coverage");
        assert!(extras.artifacts[1].expired);
        assert_eq!(extras.annotations.len(), 2);
        assert_eq!(extras.annotations[0].level, AnnotationLevel::Warning);
        assert_eq!(extras.annotations[0].start_line, Some(12));
        assert_eq!(extras.annotations[1].level, AnnotationLevel::Failure);
    }

    #[tokio::test]
    async fn run_extras_degrades_to_artifacts_when_annotations_fail() {
        // the jobs list is fetchable but its one job's annotations call has no
        // recorded response (the mock errors): artifacts must survive
        let artifacts =
            r#"{"artifacts":[{"name":"coverage","size_in_bytes":2048,"expired":false}]}"#;
        let jobs =
            r#"{"jobs":[{"check_run_url":"https://api.github.com/repos/o/r/check-runs/99"}]}"#;
        let extras = provider(&[("artifacts", artifacts), ("/jobs", jobs)])
            .run_extras(&RunId("42".into()))
            .await
            .expect("extras never errors");
        assert_eq!(extras.artifacts.len(), 1);
        assert!(extras.annotations.is_empty(), "failed job is skipped");
    }

    #[tokio::test]
    async fn current_pr_parses_the_branch_pr() {
        let json = r#"{"number":28,"title":"Inline CI runs","url":"https://gh/pull/28","baseRefName":"main","headRefOid":"abc123"}"#;
        let pr = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[("pr view feat/x", json)])),
            vec![],
            Some("feat/x".to_owned()),
            YamlCache::default(),
            EtagCache::default(),
            None,
        )
        .current_pr()
        .await
        .expect("pr call");
        let pr = pr.expect("a pr");
        assert_eq!(pr.number, 28);
        assert_eq!(pr.url.as_deref(), Some("https://gh/pull/28"));
    }

    #[tokio::test]
    async fn current_pr_propagates_a_parse_failure_like_list_prs() {
        let err = GitHubProvider::new(
            Box::new(RecordingRunner::new(&[("pr view feat/x", "not json")])),
            vec![],
            Some("feat/x".to_owned()),
            YamlCache::default(),
            EtagCache::default(),
            None,
        )
        .current_pr()
        .await
        .expect_err("malformed body must not silently read as \"no PR\"");
        assert!(matches!(err, CiError::Parse { .. }));
    }

    #[tokio::test]
    async fn current_pr_is_none_without_a_branch() {
        let pr = provider(&[]).current_pr().await.expect("pr call");
        assert!(pr.is_none(), "no branch → no PR, no gh call");
    }

    #[tokio::test]
    async fn capabilities_are_config_dag_and_dump_logs() {
        let caps = provider(&[]).capabilities();
        assert_eq!(caps.dag, DagSource::ConfigFile);
        assert_eq!(caps.logs, LogMode::Dump);
    }

    #[tokio::test]
    async fn review_payload_carries_verdict_body_and_ranges() {
        let review = crate::ci::NewPrReview {
            number: 9,
            head_oid: "abc".into(),
            verdict: crate::ci::ReviewVerdict::RequestChanges,
            body: "hold on".into(),
            comments: vec![
                crate::ci::NewPrComment {
                    number: 9,
                    head_oid: "abc".into(),
                    path: "src/a.rs".into(),
                    line: 4,
                    start_line: None,
                    new_side: true,
                    counterpart: None,
                    body: "single".into(),
                },
                crate::ci::NewPrComment {
                    number: 9,
                    head_oid: "abc".into(),
                    path: "src/a.rs".into(),
                    line: 12,
                    start_line: Some(10),
                    new_side: false,
                    counterpart: None,
                    body: "range".into(),
                },
            ],
        };
        let payload = review_payload(&review);
        assert_eq!(payload["event"], "REQUEST_CHANGES");
        assert_eq!(payload["body"], "hold on");
        assert_eq!(payload["comments"][0].get("start_line"), None);
        assert_eq!(payload["comments"][1]["start_line"], 10);
        assert_eq!(payload["comments"][1]["start_side"], "LEFT");
        assert_eq!(payload["comments"][1]["line"], 12);

        // approve with nothing pending: no body key, empty comments
        let bare = crate::ci::NewPrReview {
            number: 9,
            head_oid: "abc".into(),
            verdict: crate::ci::ReviewVerdict::Approve,
            body: String::new(),
            comments: Vec::new(),
        };
        let payload = review_payload(&bare);
        assert_eq!(payload["event"], "APPROVE");
        assert_eq!(payload.get("body"), None);
    }

    #[tokio::test]
    async fn resolve_pr_thread_uses_the_matching_mutation() {
        let runner = std::sync::Arc::new(RecordingRunner::new(&[("graphql", "{}")]));
        let provider = GitHubProvider::new(
            Box::new(runner.clone()),
            vec![WORKFLOW.to_owned()],
            None,
            YamlCache::default(),
            EtagCache::default(),
            None,
        );
        provider
            .resolve_pr_thread(9, "T_1", true)
            .await
            .expect("resolve");
        provider
            .resolve_pr_thread(9, "T_1", false)
            .await
            .expect("unresolve");
        let calls = runner.calls();
        // "unresolveReviewThread" contains "resolveReviewThread": exclude it
        assert!(
            calls[0].contains("resolveReviewThread") && !calls[0].contains("unresolveReviewThread"),
            "{calls:?}"
        );
        assert!(calls[0].contains("id=T_1"), "{calls:?}");
        assert!(calls[1].contains("unresolveReviewThread"), "{calls:?}");
    }

    #[tokio::test]
    async fn pr_comments_join_graphql_thread_state() {
        let rest = r#"[{"id":100,"path":"a.rs","line":2,"side":"RIGHT",
            "body":"root","user":{"login":"alice"},"created_at":"2026-01-01T00:00:00Z"}]"#;
        let slug = r#"{"owner":{"login":"me"},"name":"repo"}"#;
        let threads = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
            "pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"T_9","isResolved":true,"comments":{"nodes":[{"databaseId":100}]}}]
        }}}}}"#;
        let provider = provider(&[
            ("pulls/9/comments", rest),
            ("repo view", slug),
            ("graphql", threads),
        ]);
        let comments = provider.pr_comments(9).await.expect("comments");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].thread_id.as_deref(), Some("T_9"));
        assert!(comments[0].resolved);
    }

    #[tokio::test]
    async fn pr_lookup_parses_the_head() {
        let json = r#"{"number":9,"title":"t","url":"u",
            "baseRefName":"main","headRefName":"feat/x",
            "headRefOid":"headsha","author":{"login":"alice"}}"#;
        let provider = provider(&[("pr view 9", json)]);
        let pr = provider.pr(9).await.expect("pr");
        assert_eq!(pr.head_oid, "headsha");
        assert_eq!(pr.base_ref, "main");
        assert_eq!(pr.head_ref, "feat/x");
    }
}

#[cfg(test)]
mod create_pr_tests {
    use super::*;
    use crate::ci::exec::test_support::RecordingRunner;
    use crate::ci::provider::NewPullRequest;
    use std::sync::Arc;

    const VIEWED: &str = r#"{"number":7,"title":"a title","url":"https://github.com/acme/widgets/pull/7",
        "baseRefName":"main","headRefName":"feat/x","headRefOid":"abc","author":{"login":"reviewer"}}"#;

    #[tokio::test]
    async fn create_passes_the_fields_and_resolves_the_new_number() {
        let runner = Arc::new(RecordingRunner::new(&[
            ("pr create", "https://github.com/acme/widgets/pull/7\n"),
            ("pr view", VIEWED),
        ]));
        let provider = GitHubProvider::new(
            Box::new(Arc::clone(&runner)),
            Vec::new(),
            Some("feat/x".to_owned()),
            YamlCache::default(),
            EtagCache::default(),
            None,
        );
        let pr = provider
            .create_pr(&NewPullRequest {
                base: "main".to_owned(),
                head: "feat/x".to_owned(),
                title: "a title".to_owned(),
                body: "a body".to_owned(),
                draft: true,
            })
            .await
            .expect("created");
        assert_eq!(pr.number, 7);
        let create = runner.calls().remove(0);
        for expected in ["--base main", "--head feat/x", "--title a title", "--draft"] {
            assert!(
                create.contains(expected),
                "{expected} missing from {create}"
            );
        }
    }

    #[test]
    fn a_number_is_read_from_the_url_the_command_prints() {
        assert_eq!(
            pr_number_from_url("https://github.com/acme/widgets/pull/12\n"),
            Some(12)
        );
        assert_eq!(pr_number_from_url("nothing useful here"), None);
    }
}
