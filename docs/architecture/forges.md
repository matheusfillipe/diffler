# Forges: PR review and CI

## PR review

`ReviewSource::Pr{number}` keys review state on the PR number
(survives pushes); the diff is `merge-base..head` via `Vcs::tree_diff`,
fetching the head when it isn't local, from the ref the forge serves it under
(`refs/pull/<n>/head`, or `refs/merge-requests/<n>/head` on GitLab): reviewing never
needs a checkout. The branch's PR is a status row; `b p` lists all open PRs
(Enter reviews, `b` checks out). Forge review comments sync into the session
(`remote_id` marks forge-owned rows); local comments and replies post back
through queued workers (GitHub via `gh`, GitLab via `glab api`, Forgejo over
its REST API). A Forgejo thread has no handle of its own, so it is the
comments sharing a review, a path and a signed line, rooted at the lowest id;
the forge exposes no resolution API, so `Capabilities::resolve_threads` is
false there and a resolve stays in the local session.
A comment anchored to a whole file rather than a line (`NewPrComment.line:
None`) needs `Capabilities::file_comments`: GitHub and GitLab have one, so
it rides along with a submit; Forgejo has none, so `pr_pending` holds it
back in `PrPending::file_level`, and the submit confirmation names the
forge rather than blaming reviews in general. GitHub's own review-comments
array has no slot for a line-less entry at all, so `GitHubProvider::
submit_pr_review` posts each whole-file comment through `post_pr_comment`
(`subject_type=file`, no line) once the batched review lands: one more
network call and one more forge notification per whole-file comment,
since GitHub gives no way to fold it into the single review post.

## PR page

`Screen::Pr` shows what a PR says outside its diff: state, labels, the
description, and the conversation, which is every comment on the PR as a
whole plus each review summary that says or decides something (a bare
`COMMENTED` review only wraps line comments). `ForgeProvider::pr_conversation`
reads it in one `gh pr view` on GitHub, the merge request plus its unanchored
non-system notes on GitLab, and the pull, its issue comments and its reviews
on Forgejo. The conversation stays out of the review session on purpose:
session comments are anchored to lines, and `get_comments`, feedback export,
walkthroughs and viewed marks all read them. `get_pr` is its MCP read.

The page sits between the PR list and the diff: `<cr>` in the list or on a
status PR row opens it with the cursor on the files row, so a second `<cr>`
opens the diff, and `q` from the diff comes back to it. `App::pr_pages`
holds one page per `Screen::Pr` on the stack. `c` and `r` post through
`PrPost::Note`; GitLab threads its notes, so a reply goes into the
discussion, while GitHub and Forgejo have flat PR comments, so a reply
quotes the comment it answers (`NewPrNote::quoted_body`). A post the forge
refuses comes back as the next `c`'s draft. An agent's `focus` with
`review: pr:<n>` and no file opens the page by number alone, before the PR
list knows it: the page learns the head from its first load, and its
`<cr>` waits until then.

The reviewers row lists everyone asked to review and where each stands, built
by `ci::reviewers_from` from the reviews and requests in time order: a plain
comment keeps an earlier approval, and a request after a review reopens it.
GitHub reads `reviewRequests` beside the reviews, GitLab its MR `reviewers`
endpoint with each reviewer's state, and Forgejo its review rows, where a
`REQUEST_REVIEW` row is a request. `R` (or `<cr>` on the row) lists
`ForgeProvider::reviewer_candidates`: GitHub's `suggestedReviewers` first, then
its assignable users; GitLab's project members; Forgejo's `reviewers`
endpoint. A pick posts `PrPost::RequestReviewer`. GitLab replaces the whole
reviewer list on update, so its request sends the current ids plus the new one.

## GitLab merge requests

A thread is a discussion and a comment is one of
its notes, so a reply, an edit and a delete all route through the discussion
the note belongs to, which `discussion_of` looks up. An anchored note repeats
the merge request's `diff_refs` (base, start, head) plus the line, and a
multi-line one adds a `line_range`; a whole-file comment carries the same
refs with `position[position_type]=file` and no line, GitLab's own read on
a file-level note, so it drafts and publishes through the same batch a line
comment does. Writes travel as multipart form fields:
GitLab's REST layer unflattens `position[new_line]` into nested parameters,
which a JSON body never gets. A submitted review is draft notes plus one
`bulk_publish`, so the author is notified once; the verdict maps onto
approve/unapprove, the only review state the REST API records.

## Create-pull-request form

One list of rows: base, title, body, draft, then
a `[ Create ]` and a `[ Cancel ]` button, so `j`/`k` and the pointer reach the
buttons the way they reach a field (a blank line between them would break the
row mapping `ListHits` does, hence none). Title and body both edit inline
through the input modal, which is already multiline, and its own `<c-g>`
already reaches `$EDITOR` once a field is open; the form keeps a direct `e`
as a shortcut past that step, straight from the row, through the same
scratch-file mechanism, so the base and the draft toggle still decline (not
text) while title and body skip typing `<cr>` first. A created PR is seated
into the branch band by `seat_branch_pr`,
since the band resolves its PR once per branch and would otherwise stay empty
until a checkout re-armed the poll.

## Which remote's CI

A fork has two remotes for one repo, so the order
`detect_ci_remotes` builds decides whose runs show: `ci.remote` when set,
else the remote the branch pushes to (`head.upstream`'s first segment), else
`origin`. The GitHub provider then names that repo on every call, `-R` for
the `gh` subcommands and an expanded `{owner}/{repo}` for `gh api`, because
`gh` resolves a fork to its parent when nobody tells it otherwise.

## Matrix legs on the run graph

A `CiJob` carries `legs`, populated by the
provider when a `strategy.matrix` fanned a YAML job out into several run
jobs: GitHub's `expand_jobs` still folds every matching run job into one
`CiJob` (one node per YAML job, not per leg), but now keeps each leg's own
name, status and duration instead of losing them to the aggregate. A leg is
recognized by its run job name starting with `<job name> (`, GitHub's own
matrix naming, never by parsing or counting the matrix parameters, since a
leg whose matrix value is empty drops that parameter from the name (`build
(Dockerfile.cuda, -cuda)` beside `build (Dockerfile.gpu)`). `to_model` turns
a job with legs into a foldable group: the job keeps its node id as the
group's root (`Node.foldable`), and each leg becomes a member node
(`Node.group`) with the matrix parameters alone as its label, the job's own
name already on the root. A leg's node id is the run job it ran as
(`CiJobLeg.id`, e.g. `build (3.11)`), so `<cr>` on a leg opens that leg's
own log through `job_log`'s exact-name match. A `needs` edge is always resolved at the job
level, so it lands on the root, never a leg. Forgejo's provider maps each
task straight into its own `CiJob` with no per-job grouping step at all (no
workflow YAML is parsed there), so a Forgejo matrix already shows one plain
node per leg, just not folded under a shared root; GitLab's `parallel:`
jobs have the same gap and are untouched for the same reason.
