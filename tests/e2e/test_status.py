# Status screen through a real PTY: layout, cursor movement, folding,
# stage/discard/viewed operations, clean quit.
from harness import BASE_CONTENT


def test_status_screen_renders_and_quits_clean(spawn):
    tui = spawn("--no-mcp")
    # a frame can arrive split across PTY reads: wait for each expectation
    for expected in (
        "Hint:",
        "Head:",
        "main",
        "initial commit",
        "Untracked (1)",
        "notes.txt",
        "Unstaged changes (1)",
        "app.txt",
        "Recent commits (1)",
        "repo@main",
    ):
        tui.wait_for(expected)
    assert tui.quit() == 0


def test_cursor_moves_and_tab_expands_inline_hunks(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Untracked (1)")
    # rows: Untracked header, notes.txt, Unstaged header, app.txt
    tui.send("jjj")
    tui.send("\t")
    tui.wait_for("beta2")  # the expanded inline diff shows the added line
    tui.send("\t")
    tui.wait_gone("beta2")
    # k back up to the Untracked header; folding it hides the file row
    tui.send("kkk")
    tui.send("\t")
    tui.wait_gone("notes.txt")
    tui.send("\t")
    tui.wait_for("notes.txt")


def test_stage_moves_untracked_file_to_staged(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Untracked (1)")
    tui.send("j")
    tui.send("s")
    tui.wait_for("Staged changes (1)")
    # file rows show the status glyph (A = added) and the basename
    tui.wait_for("A notes.txt")
    tui.wait_gone("Untracked (1)")


def test_discard_with_confirm_restores_the_file(spawn, repo):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (1)")
    tui.send("jjj")
    tui.send("x")
    tui.wait_for("Discard changes to app.txt?")
    tui.send("y")
    tui.wait_gone("Unstaged changes")
    assert (repo / "app.txt").read_text() == BASE_CONTENT


def test_viewed_mark_shows_a_check(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (1)")
    assert "✓" not in tui.text()
    tui.send("jjj")
    tui.send("v")
    tui.wait_for("✓")


def test_ctrl_k_palette_filters_and_runs_the_best_match(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (1)")
    tui.send("\x0b")  # ctrl+k
    tui.wait_for("Commands")
    tui.wait_for("stage everything")
    tui.send("stage ev")
    tui.send("\r")
    tui.wait_gone("Unstaged changes")
    tui.wait_for("Staged changes (2)")
