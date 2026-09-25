# Diff-pane folding through a real PTY: a default fold hides the unchanged
# lines between two nearby edits, za reopens it, and zR/zM open and restore
# every fold.

from harness import git, write


def seed_big_file(repo, changed):
    """A 30-line file with `changed` edited: edits six lines apart share a
    hunk, and the unchanged lines between them clear the context fold's
    5-line floor."""
    git(repo, "checkout", "--", "app.txt")
    (repo / "notes.txt").unlink()
    lines = [f"line {i}" for i in range(1, 31)]
    write(repo / "big.txt", "\n".join(lines) + "\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-m", "seed big.txt")
    edited = [f"LINE {i}" if i in changed else f"line {i}" for i in range(1, 31)]
    write(repo / "big.txt", "\n".join(edited) + "\n")


def open_diff(tui):
    tui.wait_for("Unstaged changes (1)")
    tui.send("D")
    tui.wait_for(" DIFF ")
    tui.send("l")  # sidebar has focus after D; move into the diff pane
    tui.wait_for("⋯")


def test_default_folding_hides_the_run_between_two_edits_and_za_reopens_it(spawn, repo):
    seed_big_file(repo, {5, 12})
    tui = spawn("--no-mcp")
    open_diff(tui)

    tui.wait_for("⋯ 6 lines")
    assert "line 8" not in tui.text(), tui.dump()

    # hunk header, three context lines, the edit's two rows, then the fold
    tui.send("jjjjjj")
    tui.send("za")
    tui.wait_for("line 8")
    tui.wait_gone("⋯")


def test_z_r_opens_every_fold_and_z_m_restores_them(spawn, repo):
    seed_big_file(repo, {5, 11, 17})
    tui = spawn("--no-mcp")
    open_diff(tui)
    tui.wait_for("⋯ 5 lines")
    assert "line 13" not in tui.text(), tui.dump()

    tui.send("zR")
    tui.wait_for("line 13")
    tui.wait_gone("⋯")

    tui.send("zM")
    tui.wait_for("⋯ 5 lines")
    tui.wait_gone("line 13")
