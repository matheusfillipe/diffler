# Switching the diff algorithm live through a real PTY: the picker opens
# from the diff screen, applying a pick re-diffs the open view and names the
# algorithm in the pane heading.


def open_diff(tui):
    tui.wait_for("Unstaged changes (1)")
    tui.send("jjj")
    tui.send("\r")
    tui.wait_for(" DIFF ")


def test_switching_to_histogram_renames_the_pane_and_keeps_the_diff(spawn):
    tui = spawn("--no-mcp")
    open_diff(tui)
    tui.wait_for("beta2")  # the added line, proving the diff rendered

    tui.send_ctrl("a")
    tui.wait_for("Diff algorithm")
    tui.wait_for("histogram")

    # myers, minimal, patience, histogram: three steps down, then apply
    tui.send("jjj")
    tui.send("\r")

    tui.wait_for("Diff · histogram")  # non-default algorithm names itself
    tui.wait_for("beta2")  # re-diffed under the new algorithm, still correct
