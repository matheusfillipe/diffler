# PTY end-to-end harness: pexpect drives the diffler binary and pyte renders
# its output into an assertable screen.
import codecs
import os
import subprocess
import time
from pathlib import Path

import pexpect
import pyte
from pexpect.exceptions import EOF, TIMEOUT

DEFAULT_COLS = 120
DEFAULT_ROWS = 40
WAIT_TIMEOUT = 10.0

BASE_CONTENT = "alpha\nbeta\ngamma\n"
MODIFIED_CONTENT = "alpha\nbeta2\ngamma2\ndelta\n"


class Tui:
    """One spawned TUI process plus the virtual screen it renders into."""

    def __init__(self, cmd, cwd=None, env=None, cols=DEFAULT_COLS, rows=DEFAULT_ROWS):
        self.cols = cols
        self.rows = rows
        # we keep raw bytes because OSC sequences (OSC52 clipboard) never reach the screen grid
        self.raw = b""
        # a multibyte char can split across two reads, and decoding each chunk
        # alone would emit `�` and desync pyte's columns
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self.child = pexpect.spawn(
            cmd[0],
            cmd[1:],
            cwd=cwd,
            env=env,
            dimensions=(rows, cols),
            encoding=None,
            timeout=5,
        )
        # the app queries the cursor position when it restores the terminal after an editor
        self.screen.write_process_input = self.child.send

    def _feed(self, timeout=0.2):
        """Drain pending PTY output into the screen. Returns False when
        nothing arrived (timeout or process exit)."""
        try:
            data = self.child.read_nonblocking(size=65536, timeout=timeout)
        except TIMEOUT:
            return False
        except EOF:
            return False
        self.raw += data
        self.stream.feed(self._decoder.decode(data))
        return True

    def text(self):
        """Full screen content as one newline-joined string."""
        return "\n".join(self.screen.display)

    def dump(self):
        """Screen content framed for assertion messages."""
        bar = "-" * self.cols
        return f"{bar}\n{self.text()}\n{bar}"

    def wait_until(self, predicate, desc, timeout=WAIT_TIMEOUT):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._feed(timeout=0.2)
            if predicate(self.text()):
                return
        raise AssertionError(f"timed out waiting for {desc}; screen:\n{self.dump()}")

    def wait_for(self, substr, timeout=WAIT_TIMEOUT):
        self.wait_until(lambda text: substr in text, f"{substr!r} on screen", timeout)

    def wait_gone(self, substr, timeout=WAIT_TIMEOUT):
        self.wait_until(
            lambda text: substr not in text,
            f"{substr!r} to leave the screen",
            timeout,
        )

    def wait_raw(self, needle, timeout=WAIT_TIMEOUT):
        """Wait for a byte sequence in the raw PTY stream."""
        self.wait_until(
            lambda _text: needle in self.raw,
            f"{needle!r} in the raw stream",
            timeout,
        )

    def send(self, keys):
        self.child.send(keys)

    def send_ctrl(self, ch):
        self.child.sendcontrol(ch)

    def quit(self):
        """`q` out of the app and reap it; returns the exit status."""
        try:
            self.child.send("q")
            self.child.expect(EOF, timeout=5)
        except TIMEOUT:
            self.child.terminate(force=True)
        self.child.close()
        return self.child.exitstatus

    def close(self):
        """Best-effort teardown for fixtures: kill whatever is still alive."""
        if self.child.isalive():
            self.child.terminate(force=True)
        self.child.close()


def tui_env(home, **extra):
    """Environment with $HOME and $XDG_CONFIG_HOME in the test's tmp dir, so
    no developer config leaks in."""
    home = Path(home)
    config = home / ".config"
    config.mkdir(parents=True, exist_ok=True)
    env = {
        "TERM": "xterm-256color",
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(config),
        "USER": "reviewer",
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    }
    env.update({key: str(value) for key, value in extra.items()})
    return env


def git(repo, *args):
    """Run a git command in `repo`, isolated from the developer's config."""
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(repo),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
    }
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
        env=env,
    )


def write(path, content):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)


def make_repo(root):
    """Standard fixture repo: one commit, one unstaged modification
    (app.txt), one untracked file (notes.txt)."""
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    git(root, "init", "-b", "main", ".")
    git(root, "config", "user.name", "test")
    git(root, "config", "user.email", "test@test")
    git(root, "config", "commit.gpgsign", "false")
    write(root / "app.txt", BASE_CONTENT)
    write(root / "README.md", "# demo\n")
    git(root, "add", "-A")
    git(root, "commit", "-m", "initial commit")
    write(root / "app.txt", MODIFIED_CONTENT)
    write(root / "notes.txt", "todo\n")
    return root


def jj(repo, *args):
    """Run a jj command in `repo`, isolated from the developer's config. HOME
    sits beside the repo so jj's `.config/jj/` stays out of the reviewed tree."""
    home = Path(repo).parent / "jjhome"
    home.mkdir(exist_ok=True)
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
    }
    return subprocess.run(
        ["jj", *args],
        cwd=str(repo),
        check=True,
        capture_output=True,
        text=True,
        env=env,
    )


def make_jj_repo(root):
    """`make_repo`'s fixture, colocated with jj."""
    root = make_repo(root)
    jj(root, "git", "init", "--colocate")
    jj(root, "config", "set", "--repo", "user.name", "reviewer")
    jj(root, "config", "set", "--repo", "user.email", "reviewer@example.com")
    return root


def make_script(path, body):
    """Write an executable shell script (editor stand-ins for the tests)."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/bin/sh\n{body}\n")
    os.chmod(path, 0o755)
    return path
