# Hunk folding through a real PTY: `]` steps to a hunk header, `za` folds
# that hunk to its header and opens it again, and zM/zR fold and open every
# hunk of the file.

from harness import git, write


def seed_big_file(repo, changed):
    """A 30-line file with `changed` edited, far enough apart that each edit
    is its own hunk."""
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
    tui.wait_for("LINE 15")


def test_za_folds_the_hunk_bracket_stepped_to_and_opens_it_again(spawn, repo):
    seed_big_file(repo, {5, 15, 25})
    tui = spawn("--no-mcp")
    open_diff(tui)
    assert "⋯" not in tui.text(), tui.dump()

    tui.send("]")
    tui.send("za")
    tui.wait_for("⋯ 8 lines +1 -1")
    tui.wait_gone("LINE 15")
    assert "LINE 5" in tui.text() and "LINE 25" in tui.text(), tui.dump()

    tui.send("za")
    tui.wait_for("LINE 15")
    tui.wait_gone("⋯")


def test_z_m_folds_every_hunk_and_z_r_opens_them(spawn, repo):
    seed_big_file(repo, {5, 15, 25})
    tui = spawn("--no-mcp")
    open_diff(tui)

    tui.send("zM")
    tui.wait_for("folded every hunk")
    tui.wait_gone("LINE 15")
    assert tui.text().count("⋯ 8 lines") == 3, tui.dump()

    tui.send("zR")
    tui.wait_for("LINE 15")
    tui.wait_gone("⋯")
