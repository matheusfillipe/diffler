# $EDITOR suspend/restore through a real PTY: commit abort, scripted commit,
# and line-jump argv construction.
from harness import MODIFIED_CONTENT, git, make_script


def test_cc_with_true_editor_aborts_the_commit(spawn, repo):
    # `true` exits 0 leaving the template untouched; stripping the comment
    # lines yields an empty message, which aborts
    git(repo, "add", "app.txt")
    tui = spawn("--no-mcp", env_extra={"EDITOR": "true"})
    tui.wait_for("Staged changes (1)")
    tui.send("cc")
    tui.wait_for("commit aborted")
    assert git(repo, "log", "--format=%s").stdout.splitlines() == ["initial commit"]


def test_cc_with_scripted_editor_commits(spawn, repo, tmp_path):
    git(repo, "add", "app.txt")
    editor = make_script(
        tmp_path / "bin" / "ed.sh",
        'printf "add feature x\\n" > "$1"',
    )
    tui = spawn("--no-mcp", env_extra={"EDITOR": str(editor)})
    tui.wait_for("Staged changes (1)")
    tui.send("cc")
    tui.wait_for("committed")
    tui.wait_for("add feature x")
    tui.wait_gone("Staged changes")
    assert git(repo, "log", "-1", "--format=%s").stdout.strip() == "add feature x"


def test_ce_extends_head_with_the_staged_index(spawn, repo):
    # extend (`c e`) folds the staged index into HEAD, reusing its message,
    # with no editor round trip
    git(repo, "add", "app.txt")
    tui = spawn("--no-mcp")
    tui.wait_for("Staged changes (1)")
    tui.send("ce")
    tui.wait_for("amended")
    tui.wait_gone("Staged changes")
    assert git(repo, "log", "--format=%s").stdout.splitlines() == [
        "initial commit"
    ], "extend folds into HEAD, no new commit"
    assert git(repo, "show", "HEAD:app.txt").stdout == MODIFIED_CONTENT


def test_ca_with_scripted_editor_amends_the_message_and_the_tree(spawn, repo, tmp_path):
    git(repo, "add", "app.txt")
    editor = make_script(
        tmp_path / "bin" / "ed.sh",
        'printf "reword the initial commit\\n" > "$1"',
    )
    tui = spawn("--no-mcp", env_extra={"EDITOR": str(editor)})
    tui.wait_for("Staged changes (1)")
    tui.send("ca")
    tui.wait_for("amended")
    tui.wait_for("reword the initial commit")
    tui.wait_gone("Staged changes")
    assert git(repo, "log", "--format=%s").stdout.splitlines() == [
        "reword the initial commit"
    ], "amend rewrites HEAD in place, no new commit"
    assert git(repo, "show", "HEAD:app.txt").stdout == MODIFIED_CONTENT


def test_e_passes_line_jump_argv_to_the_editor(spawn, repo, tmp_path):
    argv_file = tmp_path / "argv.txt"
    # the script is named vim so diffler uses the +line argument family
    editor = make_script(tmp_path / "bin" / "vim", f'echo "$@" > "{argv_file}"')
    tui = spawn("--no-mcp", env_extra={"EDITOR": str(editor)})
    tui.wait_for("Unstaged changes (1)")
    tui.send("jjj")
    tui.send("\r")
    tui.wait_for(" DIFF ")
    # hunk header → context alpha → -beta → -gamma → +beta2 (new line 2)
    tui.send("jjjj")
    tui.send("e")
    tui.wait_for("edited app.txt")
    argv = argv_file.read_text().split()
    assert "+2" in argv
    assert argv[-1].endswith("app.txt")
