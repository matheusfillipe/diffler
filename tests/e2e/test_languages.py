# The language breakdown through a real PTY: the review's mix on the status
# screen, and the repo table `L` opens.


def test_status_names_the_languages_of_the_review(spawn, repo):
    # the fixture's own changes are both Text, and one language is no mix
    (repo / "README.md").write_text("# demo\n\nand prose\n")
    tui = spawn("--no-mcp")
    tui.wait_for("Changes")
    tui.wait_for("Languages")
    tui.wait_for("Markdown")
    tui.wait_for("Text")
    assert tui.quit() == 0


def test_a_single_language_review_has_no_mix_line(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Changes")
    tui.wait_for("Recent commits")
    assert "Languages" not in tui.text(), tui.dump()
    assert tui.quit() == 0


def test_l_opens_the_breakdown_and_q_returns(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Head:")
    tui.send("L")
    tui.wait_for(" STATS ")
    # the scan runs off the main task, so the table replaces "counting…"
    tui.wait_for("Language")
    tui.wait_for("Markdown")
    tui.wait_for("total")
    tui.send("q")
    tui.wait_for(" STATUS ")
    assert tui.quit() == 0


def test_sorting_the_breakdown_reports_the_column(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Head:")
    tui.send("L")
    tui.wait_for("Language")
    tui.send("s")
    tui.wait_for("sorted by files")
    tui.send("s")
    tui.wait_for("sorted by lines")
    # `q` leaves the breakdown; quit() sends the one that ends the app
    tui.send("q")
    tui.wait_for(" STATUS ")
    assert tui.quit() == 0
