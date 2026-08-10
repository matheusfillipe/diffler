# The file picker and the blame view through a real PTY, which is the only
# place the off-thread load (read + blame + highlight) actually runs.


def row_with(text, needle):
    return next((line for line in text.splitlines() if needle in line), "")


def test_picker_opens_any_tracked_file_with_its_blame(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (1)")

    tui.send_ctrl("t")
    tui.wait_for("File ·")
    # README.md is committed and untouched, so the review never lists it
    tui.send("README")
    tui.wait_for("README.md")
    # tab out of the filter so the dialog's own verbs get the key
    tui.send("\t")
    tui.send("b")

    tui.wait_for(" FILE ")
    tui.wait_for("# demo")
    tui.wait_for("initial commit")
    tui.wait_until(
        lambda text: "test" in row_with(text, "# demo"),
        "the blame column names the author beside the line it wrote",
    )

    tui.send("b")
    tui.wait_until(
        lambda text: "test" not in row_with(text, "# demo")
        and "initial commit" in text,
        "toggling the column off keeps the file and its header",
    )

    tui.send("q")
    tui.wait_for(" STATUS ")


def test_blame_from_the_status_cursor_opens_the_file_under_it(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (1)")

    tui.send_ctrl("n")
    tui.wait_until(lambda text: "app.txt" in text, "the cursor reaches a file")
    tui.send("j")
    tui.send("B")

    tui.wait_for(" FILE ")
    tui.wait_for("app.txt")
    tui.send("q")
    tui.wait_for(" STATUS ")
