# The x (caller graph) key through a real PTY. The fixture repo is .txt, so
# the deterministic path everywhere is the unsupported-language message; the
# LSP-backed happy path is covered by the rust-analyzer integration test.


def test_x_on_an_unsupported_language_says_so(spawn):
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (1)")
    tui.send("jjj")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    tui.send("x")
    tui.wait_for("no language server known for .txt files")
