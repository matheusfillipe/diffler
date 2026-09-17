# Walkthroughs through a real PTY: an agent publishes one over MCP, the human
# opens it from the status screen, walks its stops, and comments on one.
import pytest
from harness import git, write
from mcp.shared.exceptions import MCPError

from test_mcp import call_tool, free_port, mcp_url

STOPS = [
    {
        "title": "Overview",
        "body": "This change layers config sources without touching what already works.",
    },
    {
        "title": "Notes",
        "anchor": "notes.txt:1",
        "body": "The todo captures what is left before this ships.",
    },
    {
        "title": "App",
        "anchor": "app.txt:2",
        "body": "The app entrypoint picks up the new default here.",
    },
]


def many_stops(count):
    return [{"title": f"Stop {i}", "body": "why"} for i in range(count)]


def find_mcp_error(exc):
    """anyio wraps a tool's JSON-RPC error in nested `ExceptionGroup`s; dig
    through them for the `MCPError` the refusal actually raised as."""
    if isinstance(exc, MCPError):
        return exc
    if isinstance(exc, BaseExceptionGroup):
        for sub in exc.exceptions:
            found = find_mcp_error(sub)
            if found is not None:
                return found
    return None


def publish(tui, url, stops=STOPS, title="Config layering", summary=None):
    args = {"title": title, "stops": stops}
    if summary is not None:
        args["summary"] = summary
    return call_tool(tui, url, "publish_walkthrough", args)


def back_to_status(tui):
    """`q` unwinds the diff screen to status before quitting."""
    tui.send("q")
    tui.wait_for(" STATUS ")
    assert tui.quit() == 0


def test_a_published_walkthrough_appears_on_the_status_screen(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    published = publish(tui, url)
    assert published["stops"] == 3
    assert published["receipts"] == []

    # the header shows with no keypress: the reader sees it exists before
    # going looking for it; unfolding it shows the row and its title
    tui.wait_for("Walkthroughs")
    tui.send("\t")
    tui.wait_for("Config layering")
    assert tui.quit() == 0


def test_enter_opens_the_walkthrough_layout_and_jk_walk_the_stops(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    publish(tui, url)

    tui.wait_for("Walkthroughs")
    tui.send("\t")  # unfold the header
    tui.send("j")  # onto the walkthrough row
    tui.send("\r")  # open it, on the first stop: no summary was published
    tui.wait_for(" DIFF ")
    # the sidebar heading carries the walkthrough's name instead of "Files"
    tui.wait_for("Config layering")
    tui.wait_for("This change layers config sources")
    tui.wait_gone("The todo captures what is left")

    tui.send("j")  # onto the second stop
    tui.wait_for("The todo captures what is left")
    tui.wait_gone("This change layers config sources")

    tui.send("t")
    tui.wait_for("Files")
    back_to_status(tui)


def test_a_summary_is_the_leading_row_and_cr_opens_on_it(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    publish(tui, url, summary="Three files pick up the new default, in order.")

    tui.wait_for("Walkthroughs")
    tui.send("\t")
    tui.send("j")
    tui.send("\r")  # open it, on the summary: this publish gave it one
    tui.wait_for(" DIFF ")
    tui.wait_for("Summary")
    tui.wait_for("Three files pick up the new default, in order.")

    tui.send("j")  # onto the first stop
    tui.wait_for("This change layers config sources")
    back_to_status(tui)


def test_a_reply_on_a_stop_reaches_the_agent_on_that_stops_comment(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    publish(tui, url)
    epoch = call_tool(tui, url, "review_status")["feedback_epoch"]
    stops = call_tool(tui, url, "get_walkthrough")["walkthrough"]["stops"]

    tui.wait_for("Walkthroughs")
    tui.send("\t")
    tui.send("j")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    tui.wait_for("This change layers config sources")

    tui.send("j")  # onto the second stop: no summary was published
    tui.wait_for("The todo captures what is left")  # notes.txt, seated on line 1

    tui.send("l")  # focus the diff pane, seated on the anchored line
    tui.send("j")  # onto the stop's own card
    tui.send("r")
    tui.wait_for("reply")
    tui.send("looks right to me")
    tui.send("\r")
    tui.wait_for("reviewer: looks right to me")

    feedback = call_tool(
        tui, url, "wait_for_feedback", {"since_epoch": epoch, "timeout_seconds": 5}
    )
    answered = [c for c in feedback["comments"] if c["id"] == stops[1]["id"]]
    assert len(answered) == 1, feedback["comments"]
    assert [r["body"] for r in answered[0]["replies"]] == ["looks right to me"]
    back_to_status(tui)


def test_one_stop_over_the_rail_is_refused_with_a_receipt(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)

    # one past MAX_STOPS in diffler-core, the rail against dumping the diff
    with pytest.raises(BaseExceptionGroup) as excinfo:
        call_tool(
            tui, url, "publish_walkthrough", {"title": "Too many", "stops": many_stops(21)}
        )
    error = find_mcp_error(excinfo.value)
    assert error is not None, f"expected an MCPError: {excinfo.value!r}"
    assert "too_many_stops" in str(error)

    status = call_tool(tui, url, "review_status")
    assert status["walkthroughs"] == []
    assert tui.quit() == 0


def test_selecting_a_comment_in_another_slide_switches_the_window(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    stops = [
        {
            "title": "App",
            "anchor": "app.txt:2",
            "body": "Stop one names app.txt line two.",
        },
        {
            "title": "Notes",
            "anchor": "notes.txt:1",
            "body": "Stop two names notes.txt line one.",
        },
    ]
    publish(tui, url, stops=stops, title="Two slides")

    tui.wait_for("Walkthroughs")
    tui.send("\t")
    tui.send("j")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    tui.wait_for("Stop one names app.txt line two.")

    tui.send("C")
    tui.wait_for("Comments (2)")
    tui.send("j")  # onto the second stop's own comment

    tui.wait_until(
        lambda text: "Stop two names notes.txt line one." in text
        and "Stop one names app.txt line two." not in text,
        "the pane windows to the second stop's slide, not the first",
    )
    back_to_status(tui)


def test_publishing_twice_without_an_id_lists_two_walkthroughs(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    first = publish(tui, url, stops=[STOPS[0]], title="First tour")
    second = publish(tui, url, stops=[STOPS[0]], title="Second tour")
    assert first["id"] != second["id"]

    status = call_tool(tui, url, "review_status")
    titles = [w["title"] for w in status["walkthroughs"]]
    assert titles == ["Second tour", "First tour"], "newest first"

    tui.wait_for("Walkthroughs (2)")
    tui.send("\t")
    tui.wait_for("Second tour")
    tui.wait_for("First tour")
    assert tui.quit() == 0


def test_the_walkthrough_survives_a_restart(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    publish(tui, url)
    tui.wait_for("Walkthrough")
    assert tui.quit() == 0

    restarted = spawn("--port", str(free_port()))
    restarted.wait_for("Walkthrough")
    assert restarted.quit() == 0


def test_publishing_keeps_stops_out_of_the_working_trees_comments(spawn):
    # a walkthrough is its own review source: get_comments tags every stop
    # with its own source, never "working", so the working-tree review never
    # sees them
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    publish(tui, url)

    comments = call_tool(tui, url, "get_comments")["comments"]
    assert not any(c["source"] == "working" for c in comments), comments
    walkthrough_sources = {c["source"] for c in comments if c["source"].startswith("walkthrough-")}
    assert len(walkthrough_sources) == 1, comments
    assert len(comments) == 3
    assert tui.quit() == 0


def test_d_on_the_working_tree_leaves_the_walkthrough_intact(spawn):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    publish(tui, url)

    tui.send("D")  # status screen: opens the working-tree diff
    tui.wait_for(" DIFF ")
    tui.send("jjjj")
    tui.send("c")
    tui.wait_for("comment on")
    tui.send("a working-tree note")
    tui.send("\r")
    tui.wait_for("▌ reviewer")

    tui.send("D")  # diff screen: clears the working tree's own comments
    tui.wait_for("Delete all")
    tui.send("y")
    tui.wait_gone("a working-tree note")

    comments = call_tool(tui, url, "get_comments")["comments"]
    assert [c["source"] for c in comments] != [], "the walkthrough's stops survive"
    assert all(c["source"] != "working" for c in comments), comments
    assert len(comments) == 3
    back_to_status(tui)


def test_a_stop_in_a_committed_file_opens_over_a_clean_tree(spawn, repo):
    # app.txt back to its committed content, notes.txt gone: nothing left
    # uncommitted, so the working-tree diff itself carries zero files
    git(repo, "checkout", "--", "app.txt")
    (repo / "notes.txt").unlink()

    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    call_tool(
        tui,
        url,
        "publish_walkthrough",
        {
            "title": "Committed file tour",
            "stops": [
                {
                    "title": "Base app",
                    "anchor": "app.txt:1",
                    "body": "why line one",
                }
            ],
        },
    )

    tui.wait_for("Walkthroughs")
    tui.send("\t")
    tui.send("j")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    tui.wait_for("Base app")
    tui.wait_for("alpha")
    back_to_status(tui)


def test_c_on_a_slide_line_opens_a_composer_anchored_to_the_code_under_it(spawn, repo):
    # a clean tree: app.txt:1 is a stop pointing at a file the working-tree
    # diff itself does not carry, exactly where a reader answering the stop
    # about its code needs `c` to work
    git(repo, "checkout", "--", "app.txt")
    (repo / "notes.txt").unlink()

    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    call_tool(
        tui,
        url,
        "publish_walkthrough",
        {
            "title": "Committed file tour",
            "stops": [
                {
                    "title": "Base app",
                    "anchor": "app.txt:1",
                    "body": "why line one",
                }
            ],
        },
    )

    tui.wait_for("Walkthroughs")
    tui.send("\t")
    tui.send("j")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    tui.wait_for("alpha")

    tui.send("l")  # focus the diff pane, seated on the anchored line
    tui.send("c")
    tui.wait_for("comment on app.txt:1")
    tui.send("why alpha matters")
    tui.send("\r")
    tui.wait_for("reviewer · open")
    tui.wait_for("why alpha matters")

    comments = call_tool(tui, url, "get_comments")["comments"]
    mine = [c for c in comments if c["body"] == "why alpha matters"]
    assert len(mine) == 1, comments
    assert mine[0]["file"] == "app.txt"
    assert mine[0]["line"] == 1
    back_to_status(tui)


def test_a_stop_pinned_before_a_removal_still_shows_its_code(spawn, repo):
    # committed, then dropped by a later commit: a walkthrough published
    # against the earlier one still has to show the code it pointed at
    write(repo / "gone.txt", "kept-line-one\nkept-line-two\nkept-line-three\n")
    git(repo, "add", "gone.txt")
    git(repo, "commit", "-m", "add gone.txt")

    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    call_tool(
        tui,
        url,
        "publish_walkthrough",
        {
            "title": "Before it left",
            "stops": [
                {
                    "title": "The middle line",
                    "anchor": "gone.txt:2",
                    "body": "why line two",
                }
            ],
        },
    )

    git(repo, "rm", "gone.txt")
    git(repo, "commit", "-m", "drop gone.txt")

    tui.wait_for("Walkthroughs")
    tui.send("\t")
    tui.send("j")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    tui.wait_for("The middle line")
    tui.wait_for("kept-line-two")
    back_to_status(tui)
