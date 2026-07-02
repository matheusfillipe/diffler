# Latency ceilings through a real PTY: first frame and diff file-switching
# must stay interactive even with large files. Ceilings are generous for CI
# noise; the point is catching order-of-magnitude regressions (e.g. heavy
# work creeping back onto the render path).
import subprocess
import time

from conftest import BIN
from harness import Tui, tui_env

FIRST_FRAME_CEILING = 3.0
SWITCH_CEILING = 1.0


def big_repo(tmp_path):
    root = tmp_path / "bigrepo"
    root.mkdir()
    run = lambda *a: subprocess.run(a, cwd=root, check=True, capture_output=True)
    run("git", "init", "-b", "main")
    run("git", "config", "user.name", "t")
    run("git", "config", "user.email", "t@t")
    for n in range(6):
        body = "\n".join(
            f"fn item_{n}_{i}(x: u32) -> u32 {{ x + {i} }}" for i in range(4000)
        )
        (root / f"file_{n}.rs").write_text(body + "\n")
    run("git", "add", "-A")
    run("git", "commit", "-m", "initial")
    for n in range(6):
        path = root / f"file_{n}.rs"
        path.write_text(path.read_text().replace("x + 7", "x * 7") + f"// edit {n}\n")
    return root


def test_first_frame_and_file_switch_stay_interactive(tmp_path, home):
    root = big_repo(tmp_path)
    start = time.monotonic()
    tui = Tui(
        [str(BIN), "--no-mcp", str(root)],
        cwd=str(root),
        env=tui_env(home),
    )
    try:
        tui.wait_for("Unstaged changes (6)")
        first_frame = time.monotonic() - start
        assert first_frame < FIRST_FRAME_CEILING, f"first frame took {first_frame:.2f}s"

        tui.send("j")
        tui.send("\r")  # open the diff screen on the first file
        tui.wait_for("item_0_9(")  # pane content, not the sidebar name
        for step in range(1, 6):
            began = time.monotonic()
            tui.send("\x0e")  # ctrl-n: next file
            tui.wait_for(f"item_{step}_9(")
            took = time.monotonic() - began
            assert took < SWITCH_CEILING, f"switch to file_{step} took {took:.2f}s"
    finally:
        tui.close()
