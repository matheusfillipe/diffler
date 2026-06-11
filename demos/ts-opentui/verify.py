#!/usr/bin/env python3
"""PTY smoke test for the diffler OpenTUI demo.

Run: uv run --with pexpect --with pyte python3 verify.py
"""
import os
import sys
import time

import pexpect
import pyte

ROWS, COLS = 40, 120
HERE = os.path.dirname(os.path.abspath(__file__))


def render(feed_bytes, screen, stream):
    stream.feed(feed_bytes.decode("utf-8", errors="replace"))
    return "\n".join(screen.display)


def drain(child, seconds=2.0):
    out = b""
    end = time.time() + seconds
    while time.time() < end:
        try:
            out += child.read_nonblocking(65536, timeout=0.2)
        except pexpect.TIMEOUT:
            pass
        except pexpect.EOF:
            break
    return out


def main():
    env = dict(os.environ, TERM="xterm-256color")
    child = pexpect.spawn(
        "bun", ["src/main.ts"], cwd=HERE, env=env,
        dimensions=(ROWS, COLS), timeout=10,
    )
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)

    failures = []

    def check(name, cond):
        print(("PASS" if cond else "FAIL"), name)
        if not cond:
            failures.append(name)

    # --- home screen ---
    time.sleep(2)
    text = render(drain(child, 2), screen, stream)
    check("home: contains 'diffler'", "diffler" in text)
    check("home: contains 'auth.py'", "auth.py" in text)
    check("home: contains 'Workspaces'", "Workspaces" in text)
    check("home: contains 'Recent commits'", "Recent commits" in text)

    # --- open diff on src/auth.py (cursor starts on it) ---
    child.send("\r")
    text = render(drain(child, 2), screen, stream)
    check("diff: hunk header '@@ -10,7'", "@@ -10,7" in text)
    check("diff: contains 'LEEWAY'", "LEEWAY" in text)
    check("diff: contains 'accepted'", "accepted" in text)
    check("diff: contains mock comment author 'mattf'", "mattf" in text)
    check("diff: second hunk '@@ -31,6'", "@@ -31,6" in text)

    # --- j/k movement and comment input ---
    child.send("jjj")
    drain(child, 0.5)
    child.send("c")
    text = render(drain(child, 1), screen, stream)
    check("comment input opens on 'c'", "new comment" in text)
    child.send("\x1b")  # escape closes input
    text = render(drain(child, 1), screen, stream)
    check("comment input closes on Esc", "new comment" not in text)

    # --- x rejects hunk under cursor ---
    child.send("x")
    text = render(drain(child, 1), screen, stream)
    check("'x' marks hunk rejected", "rejected" in text)

    # --- mouse: click a row in the files sidebar (SGR press+release) ---
    # sidebar row for tests/test_auth.py sits near the top-left of the diff screen
    child.send("\x1b[<0;5;4M\x1b[<0;5;4m")
    text = render(drain(child, 1), screen, stream)
    check("mouse click selects sidebar file", "no mock diff" in text)
    child.send("\x1b[<0;5;2M\x1b[<0;5;2m")  # click back onto src/auth.py
    text = render(drain(child, 1), screen, stream)
    check("mouse click back to auth.py", "@@ -10,7" in text)

    # --- q goes back home, q again quits cleanly ---
    child.send("q")
    text = render(drain(child, 1), screen, stream)
    check("q returns to home", "Workspaces" in text)
    child.send("q")
    child.expect(pexpect.EOF, timeout=5)
    child.close()
    check("clean exit (status 0)", child.exitstatus == 0)

    # --- wheel scroll in a short PTY where the diff overflows ---
    rows2 = 16
    child2 = pexpect.spawn(
        "bun", ["src/main.ts"], cwd=HERE, env=env,
        dimensions=(rows2, COLS), timeout=10,
    )
    screen2 = pyte.Screen(COLS, rows2)
    stream2 = pyte.Stream(screen2)
    time.sleep(2)
    drain(child2, 2)
    child2.send("\r")  # open diff
    text = render(drain(child2, 2), screen2, stream2)
    check("short pty: diff open, bottom not visible yet", "metrics.incr" not in text)
    for _ in range(25):
        child2.send("\x1b[<65;60;8M")  # SGR wheel-down inside the diff panel
        time.sleep(0.1)
    text = render(drain(child2, 2), screen2, stream2)
    check("wheel scroll reveals bottom of diff", "metrics.incr" in text)
    child2.send("qq")
    child2.expect(pexpect.EOF, timeout=5)
    child2.close()
    check("short pty: clean exit", child2.exitstatus == 0)

    if failures:
        print(f"\n{len(failures)} check(s) failed")
        sys.exit(1)
    print("\nall checks passed")


if __name__ == "__main__":
    main()
