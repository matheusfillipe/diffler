# Network ops (push) through a real PTY against a local bare remote: the
# `P` push transient shells out to the user's `git`, so no credentials are
# involved and the result is deterministic.
from harness import Tui, git, make_repo, tui_env


def test_push_set_upstream_to_a_local_bare_remote(tmp_path):
    repo = make_repo(tmp_path / "repo")
    # a bare remote on disk: push reaches it over the file transport, so no
    # network or auth is needed and the outcome is deterministic
    remote = tmp_path / "remote.git"
    git(repo, "init", "--bare", str(remote))
    git(repo, "remote", "add", "origin", str(remote))

    env = tui_env(tmp_path / "home")
    tui = Tui(
        [str(_bin()), "--no-mcp", str(repo)],
        cwd=str(repo),
        env=env,
    )
    try:
        tui.wait_for("Head:")
        # P opens the push transient; u resolves the remote and asks before
        # setting upstream (the failsafe), y confirms
        tui.send("P")
        tui.wait_for("Push")
        tui.send("u")
        tui.wait_for("set it as upstream")
        tui.send("y")
        # a "running …" status shows while the subprocess runs; the success
        # toast then reads "push -u: <summary>". Waiting for that colon form
        # both proves success (errors show only the stderr line, never the
        # label) and guarantees the push finished before we inspect the remote.
        tui.wait_for("push -u:")
        out = git(repo, "ls-remote", str(remote))
        assert "refs/heads/main" in out.stdout, out.stdout
    finally:
        tui.close()


def _bin():
    from conftest import BIN

    return BIN
