# The kinds sidebar through a real PTY: t reaches it, the diff groups by what
# each file is, and the bucket nobody reviews by hand opens folded.

from harness import git, write


def test_t_cycles_into_the_kinds_layout(spawn, repo):
    write(repo / "src" / "lib.rs", "fn main() {}\n")
    write(repo / "tests" / "cases.rs", "fn case() {}\n")
    write(repo / "Cargo.lock", "# lock\n")

    tui = spawn("--no-mcp")
    tui.wait_for("Untracked (4)")
    tui.send("D")
    tui.wait_for(" DIFF ")

    tui.send("t")
    tui.wait_for("To review")
    tui.send("t")

    tui.wait_until(
        lambda text: "Source (1)" in text and "Tests (1)" in text,
        "the kinds layout groups the diff",
    )
    # the arrow carries the fold state; the pane may well be showing the file
    tui.wait_until(
        lambda text: "▸ Generated (1)" in text,
        "the generated bucket is present and folded",
    )


def test_a_file_the_repo_declares_generated_regroups_after_a_refresh(spawn, repo):
    write(repo / ".gitattributes", "gen/** linguist-generated=true\n")
    git(repo, "add", ".gitattributes")
    git(repo, "commit", "-m", "declare the generated tree")

    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes")
    tui.send("D")
    tui.wait_for(" DIFF ")
    tui.send("tt")
    tui.wait_for("Docs")

    # the watcher picks this up while the kinds layout is already open
    write(repo / "gen" / "thing.rs", "fn generated() {}\n")

    tui.wait_until(
        lambda text: "Generated (1)" in text,
        "the attributes reach the sidebar without a layout cycle",
    )
