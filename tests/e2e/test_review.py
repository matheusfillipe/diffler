# Review flow through a real PTY: diff view, comment modal, session
# persistence, visual-range comments, OSC52 copy, feedback send.
import base64
import json

OSC52_PREFIX = b"\x1b]52;c;"


def open_diff(tui):
    """From the status screen, put the cursor on app.txt and open the diff."""
    tui.wait_for("Unstaged changes (1)")
    tui.send("jjj")
    tui.send("\r")
    tui.wait_for(" DIFF ")


def cursor_to_added_line(tui):
    """The diff pane starts on the hunk header; reach the first added line
    (+beta2, new-side line 2): hunk, context alpha, -beta, -gamma, +beta2."""
    tui.send("jjjj")


def load_session(repo):
    return json.loads((repo / ".diffler" / "reviews" / "working.json").read_text())


def test_enter_opens_the_diff_view(spawn):
    tui = spawn("--no-mcp")
    open_diff(tui)
    tui.wait_for("beta2")  # the added line, proving the diff rendered
    tui.wait_for("c comment")  # diff hint line


def test_comment_modal_writes_comment_and_session(spawn, repo):
    tui = spawn("--no-mcp")
    open_diff(tui)
    cursor_to_added_line(tui)
    tui.send("c")
    tui.wait_for("Comment app.txt:2")
    tui.send("needs work")
    tui.send("\r")
    # the comment box renders inline: author chip, body, border
    tui.wait_for("┌─ reviewer · open")
    tui.wait_for("needs work")
    tui.wait_for("└─")

    session = load_session(repo)
    comment = session["comments"][0]
    assert comment["body"] == "needs work"
    assert comment["author"] == "reviewer"
    assert comment["anchor"]["file"] == "app.txt"
    assert comment["anchor"]["line"] == 2
    assert comment["anchor"]["line_end"] is None


def test_visual_range_comment_sets_line_end(spawn, repo):
    tui = spawn("--no-mcp")
    open_diff(tui)
    cursor_to_added_line(tui)
    tui.send("V")
    tui.send("j")
    tui.send("c")
    tui.wait_for("Comment app.txt:2-3")
    tui.send("range note")
    tui.send("\r")
    tui.wait_for("└─")

    comment = load_session(repo)["comments"][0]
    assert comment["body"] == "range note"
    assert comment["anchor"]["line"] == 2
    assert comment["anchor"]["line_end"] == 3


def test_copy_emits_osc52_and_send_bumps_feedback(spawn):
    tui = spawn("--no-mcp")
    open_diff(tui)
    cursor_to_added_line(tui)
    tui.send("c")
    tui.wait_for("Comment app.txt:2")
    tui.send("ship it")
    tui.send("\r")
    tui.wait_for("└─")

    tui.send("Y")
    tui.wait_for("copied 1 comment (all)")
    tui.wait_raw(OSC52_PREFIX)
    # the payload can straggle across PTY reads: wait for the terminator
    tui.wait_until(
        lambda _text: b"\x07" in tui.raw[tui.raw.index(OSC52_PREFIX) :],
        "OSC52 terminator in the raw stream",
    )
    start = tui.raw.index(OSC52_PREFIX) + len(OSC52_PREFIX)
    end = tui.raw.index(b"\x07", start)
    markdown = base64.b64decode(tui.raw[start:end]).decode()
    assert "## Review feedback — repo @ main (1 comment)" in markdown
    assert "### app.txt:2" in markdown
    assert "+beta2" in markdown  # fenced diff context around the anchor
    assert "> ship it" in markdown

    tui.send("Z")
    tui.wait_for("feedback sent to waiting agents")
