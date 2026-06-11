# PTY smoke test: drives the compiled demo binary through a virtual terminal
# and asserts both screens render. Run with:
#   uv run --with pexpect --with pyte python3 verify.py
import os
import sys
import time

import pexpect
import pyte

ROWS, COLS = 40, 120
HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, "target", "debug", "diffler-demo")


def screen_text(screen):
    return "\n".join(screen.display)


def feed_all(child, stream, timeout=1.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            data = child.read_nonblocking(size=65536, timeout=0.2)
            stream.feed(data.decode("utf-8", "replace"))
        except pexpect.TIMEOUT:
            pass
        except pexpect.EOF:
            break


def main():
    child = pexpect.spawn(
        BIN,
        dimensions=(ROWS, COLS),
        env={**os.environ, "TERM": "xterm-256color"},
        encoding=None,
    )
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)

    time.sleep(1)
    feed_all(child, stream)
    home = screen_text(screen)
    assert "diffler" in home, f"home screen missing 'diffler':\n{home}"
    assert "auth.py" in home, f"home screen missing 'auth.py':\n{home}"
    assert "Workspaces (2)" in home, "home screen missing Workspaces section"
    assert "Recent commits" in home, "home screen missing Recent commits"
    print("home screen OK")

    child.send("\r")  # Enter -> diff view
    time.sleep(0.5)
    feed_all(child, stream)
    diff = screen_text(screen)
    assert "@@ -10,7" in diff, f"diff view missing hunk header:\n{diff}"
    assert "LEEWAY" in diff, f"diff view missing LEEWAY line:\n{diff}"
    assert "mattf" in diff, "diff view missing mock comment author chip"
    assert "accepted" in diff, "diff view missing verdict chip"
    print("diff view OK")

    child.send("q")  # back to home
    child.send("q")  # quit
    child.expect(pexpect.EOF, timeout=5)
    child.close()
    assert child.exitstatus == 0, f"non-zero exit: {child.exitstatus}"
    print("clean exit OK")
    print("ALL CHECKS PASSED")


if __name__ == "__main__":
    sys.exit(main())
