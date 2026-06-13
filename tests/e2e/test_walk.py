# Full review walk through a real PTY: D opens the whole working-tree diff,
# v marks the file under the cursor viewed and advances to the next unviewed
# one, the status bar tracks progress, and the marks survive the trip back
# to the status screen.


def test_review_walk_marks_all_files_and_shows_progress(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (1)")
    tui.send("D")
    tui.wait_for(" DIFF ")
    tui.wait_for("viewed 0/2 files")

    # v on the first file header: marked viewed, folded, cursor advances
    tui.send("v")
    tui.wait_for("viewed 1/2 files")
    tui.wait_for("✓ viewed")

    tui.send("v")
    tui.wait_for("viewed 2/2 files")

    tui.send("q")
    tui.wait_for(" STATUS ")
    tui.wait_for("2 files, 2 viewed")
    tui.wait_until(
        lambda text: text.count("✓") >= 2,
        "both files checked on the status screen",
    )
