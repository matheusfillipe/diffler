# The comments sidebar through a real PTY: C toggles it, lists the review's
# comments, and Enter hands focus to the pane with the comment still up.


def test_c_toggles_the_comments_sidebar(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes")
    tui.send("D")
    tui.wait_for(" DIFF ")

    tui.send("l")
    tui.send("j")
    tui.send("c")
    tui.wait_for("comment on")
    tui.send("a note that is long enough to wrap in the narrow column")
    tui.send("\r")
    tui.wait_for("a note that is long enough")

    tui.send("C")
    tui.wait_for("Comments (1)")

    tui.send("\r")
    tui.wait_until(
        lambda text: "Comments (1)" in text and "app.txt" in text,
        "enter keeps the comment's file up",
    )

    tui.send("C")
    tui.wait_gone("Comments (1)")
