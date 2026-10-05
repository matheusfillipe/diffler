# The symbol lens through a real PTY: `*` on a changed line labels each of its
# names with a digit, a digit narrows to one and opens its references beside
# the diff, `n` walks its uses, and esc takes it all away.

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


def test_star_labels_the_lines_names_and_esc_takes_the_labels_away(spawn, repo):
    seed(repo)
    tui = spawn("--no-mcp")
    tui.wait_for("Unstaged changes (2)")
    tui.send("D")
    tui.wait_for(" DIFF ")
    tui.send("l")  # the sidebar has focus after D; move into the diff pane
    tui.wait_for("discount: u32")
    tui.send("jjj")  # the hunk header, the two old lines, then the new signature

    tui.send("*")
    tui.wait_for("1pply(2rice: u32, 3ty: u32, 4iscount")

    tui.send("4")
    tui.wait_for("References · discount (2)")
    tui.send("n")
    tui.wait_for("discount")

    tui.send("\x1b")
    tui.wait_gone("1pply(2rice")

    # n left the cursor on discount's use, so `*` there names that line
    tui.send("*")
    tui.wait_for("1otal - 2iscount")
