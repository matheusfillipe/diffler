# Three-dot review through a real PTY: the `d` transient diffs the branch
# against its base, committed work and uncommitted edits together.
from harness import git, write


def branch_with_committed_and_dirty_work(repo):
    """Put the fixture repo on a branch one commit ahead of main, with the
    unstaged edit to app.txt still uncommitted."""
    git(repo, "checkout", "-q", "-b", "feat/topic")
    write(repo / "feature.txt", "shipped\n")
    git(repo, "add", "feature.txt")
    git(repo, "commit", "-m", "add the feature")
    return repo


def test_d_reviews_the_branch_against_its_base(spawn, repo):
    branch_with_committed_and_dirty_work(repo)
    tui = spawn("--no-mcp")
    tui.wait_for("repo@feat/topic")
    tui.send("d")
    tui.wait_for("Diff the working tree against")
    tui.send("d")
    tui.wait_for("DIFF vs main")
    # the sidebar carries the committed file and the uncommitted edit
    tui.wait_for("feature.txt")
    tui.wait_for("app.txt")
    # the pane opens on the first file; walk to each and read its content
    tui.wait_for("beta2")  # app.txt, edited but never committed
    tui.send("\t")
    tui.wait_for("shipped")  # feature.txt, landed in the branch commit
    tui.send("q")  # the diff screen pops back to status first
    tui.wait_for(" STATUS ")
    assert tui.quit() == 0


def test_the_review_follows_edits_made_while_it_is_open(spawn, repo):
    branch_with_committed_and_dirty_work(repo)
    tui = spawn("--no-mcp")
    tui.wait_for("repo@feat/topic")
    tui.send("d")
    tui.wait_for("Diff the working tree against")
    tui.send("d")
    tui.wait_for("DIFF vs main")
    write(repo / "afterwards.txt", "written while reviewing\n")
    tui.wait_for("afterwards.txt")
    tui.send("q")
    tui.wait_for(" STATUS ")
    assert tui.quit() == 0
