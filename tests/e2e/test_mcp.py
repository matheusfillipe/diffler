# MCP server through a real PTY plus a streamable-HTTP client (official
# `mcp` python SDK): tool calls round-trip through the running TUI.
import asyncio
import json
import re
import socket

from mcp import ClientSession

try:
    from mcp.client.streamable_http import streamable_http_client
except ImportError:
    from mcp.client.streamable_http import (
        streamablehttp_client as streamable_http_client,
    )


def free_port():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def _attr(obj, *names):
    """First attribute that exists: the SDK renamed its camelCase fields."""
    for name in names:
        value = getattr(obj, name, None)
        if value is not None:
            return value
    return None


async def _call_tool(url, name, arguments):
    # the SDK yields (read, write) or (read, write, get_session_id) by version
    async with streamable_http_client(url) as streams:
        read, write = streams[0], streams[1]
        async with ClientSession(read, write) as session:
            await session.initialize()
            result = await session.call_tool(name, arguments)
            errored = _attr(result, "is_error", "isError")
            assert not errored, f"tool {name} errored: {result.content}"
            structured = _attr(result, "structured_content", "structuredContent")
            if structured is not None:
                return structured
            return json.loads(result.content[0].text)


def call_tool(url, name, arguments=None):
    return asyncio.run(_call_tool(url, name, arguments or {}))


def mcp_url(tui):
    """Parse the bound MCP port from the status bar. The requested port can
    be lost to a race (the server falls back to an ephemeral one), so the
    screen is the source of truth."""
    tui.wait_for("mcp :")
    match = re.search(r"mcp :(\d+)", tui.text())
    assert match, f"no mcp port on screen:\n{tui.dump()}"
    return f"http://127.0.0.1:{match.group(1)}/mcp"


def test_review_status_and_get_comments_round_trip(spawn, repo):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)

    status = call_tool(url, "review_status")
    assert status["repo"] == repo.name
    assert status["branch"] == "main"
    paths = {entry["path"] for entry in status["files_changed"]}
    assert "app.txt" in paths
    assert "notes.txt" in paths

    # comment through the TUI, read it back over MCP
    tui.send("jjj")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    # the diff pane opens focused on the hunk header; reach +beta2 (line 2)
    tui.send("jjjj")
    tui.send("c")
    tui.wait_for("Comment app.txt:2")
    tui.send("ship it")
    tui.send("\r")
    tui.wait_for("▌ reviewer")

    comments = call_tool(url, "get_comments")["comments"]
    assert [c["body"] for c in comments] == ["ship it"]
    assert comments[0]["file"] == "app.txt"
    assert comments[0]["line"] == 2
    assert comments[0]["status"] == "open"


def test_endpoint_file_publishes_the_bound_port(spawn, repo):
    tui = spawn("--port", str(free_port()))
    url = mcp_url(tui)
    port = int(re.search(r":(\d+)/mcp", url).group(1))
    # the stdio proxy discovers the live port from this file
    endpoint = json.loads((repo / ".diffler" / "mcp.json").read_text())
    assert endpoint["port"] == port
    assert endpoint["url"] == url
