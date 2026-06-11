"""PTY smoke test for the diffler bubbletea demo.

Run: uv run --with pexpect --with pyte python3 verify.py
Drives the compiled binary in a pseudo-terminal and asserts both screens render.
"""
import sys
import time

import pexpect
import pyte

ROWS, COLS = 40, 120


def snapshot(child, screen, stream, settle=1.0):
    time.sleep(settle)
    while True:
        try:
            data = child.read_nonblocking(size=65536, timeout=0.3)
        except pexpect.TIMEOUT:
            break
        except pexpect.EOF:
            break
        stream.feed(data.decode("utf-8", errors="replace"))
    return "\n".join(screen.display)


def main():
    child = pexpect.spawn(
        "./diffler-demo",
        dimensions=(ROWS, COLS),
        env={"TERM": "xterm-256color", "HOME": "/tmp", "PATH": "/usr/bin:/bin"},
        cwd=".",
        timeout=5,
    )
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)

    failures = []

    def check(label, text, *needles):
        for n in needles:
            if n in text:
                print(f"PASS: {label}: found {n!r}")
            else:
                failures.append(f"{label}: missing {n!r}")
                print(f"FAIL: {label}: missing {n!r}")

    home = snapshot(child, screen, stream)
    check("home screen", home, "diffler", "auth.py", "Workspaces (2)", "Recent commits")

    # Enter on default cursor (workspace row) is a no-op; move to Changes first.
    child.send("\t")  # Tab → jump to Changes section
    child.send("\r")  # Enter → open diff for src/auth.py
    diff = snapshot(child, screen, stream)
    check("diff screen", diff, "@@ -10,7", "LEEWAY", "accepted", "pending", "mattf")

    # Comment input on `c`
    child.send("c")
    commenting = snapshot(child, screen, stream, settle=0.5)
    check("comment input", commenting, "enter save")
    child.send("\x1b")  # esc closes input

    time.sleep(0.3)
    child.send("q")  # back to home
    time.sleep(0.3)
    child.send("q")  # quit
    child.expect(pexpect.EOF, timeout=5)
    child.close()
    if child.exitstatus not in (0, None):
        failures.append(f"exit status {child.exitstatus}")
    print(f"exit status: {child.exitstatus}")

    if failures:
        print("\nFAILED:", *failures, sep="\n  ")
        sys.exit(1)
    print("\nALL CHECKS PASSED")


if __name__ == "__main__":
    main()
