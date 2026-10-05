# The symbol lens through a real PTY: `*` on a changed line names its symbols
# on the strip that replaces the hint line, a digit narrows to one, `n` walks
# its uses, and esc brings the hint line back.

from harness import git, write

LIB_BEFORE = """pub fn apply(price: u32, qty: u32) -> u32 {
    price * qty
}

pub fn other(price: u32) -> u32 {
    price + 1
}
"""

LIB_AFTER = """pub fn apply(price: u32, qty: u32, discount: u32) -> u32 {
    let total = price * qty;
    total - discount.min(total)
}

pub fn other(price: u32) -> u32 {
    price + 2
}
"""


def seed(repo):
    git(repo, "checkout", "--", "app.txt")
    (repo / "notes.txt").unlink()
    write(repo / "src" / "lib.rs", LIB_BEFORE)
    write(repo / "src" / "main.rs", "use crate::apply;\n\npub fn run() -> u32 {\n    apply(2, 3)\n}\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-m", "seed the billing code")
    write(repo / "src" / "lib.rs", LIB_AFTER)
    write(repo / "src" / "main.rs", "use crate::apply;\n\npub fn run() -> u32 {\n    apply(2, 3, 1)\n}\n")


def test_star_names_the_lines_symbols_and_esc_closes_the_lens(spawn, repo):
    seed(repo)
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (2)")
    tui.send("D")
    tui.wait_for(" DIFF ")
    tui.send("l")  # the sidebar has focus after D; move into the diff pane
    tui.wait_for("discount: u32")
    tui.send("jjj")  # the hunk header, the two old lines, then the new signature

    tui.send("*")
    tui.wait_for("1 apply 5 uses in 2 files")
    assert "2 price 4 uses in apply" in tui.text(), tui.dump()

    tui.send("4")
    tui.send("n")
    tui.wait_for("discount")

    tui.send("\x1b")
    tui.wait_gone("1 apply 5 uses")
    tui.wait_for("add comment")

    # n left the cursor on discount's use, so `*` there names that line
    tui.send("*")
    tui.wait_for("1 total 3 uses in apply")
