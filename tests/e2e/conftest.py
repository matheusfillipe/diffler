from pathlib import Path

import pytest
from harness import Tui, make_repo, tui_env

REPO_ROOT = Path(__file__).resolve().parents[2]
BIN = REPO_ROOT / "target" / "debug" / "diffler"


@pytest.fixture(scope="session", autouse=True)
def binary():
    if not BIN.exists():
        pytest.fail(f"{BIN} not found — run `cargo build -p diffler` first")
    return BIN


@pytest.fixture
def repo(tmp_path):
    return make_repo(tmp_path / "repo")


@pytest.fixture
def home(tmp_path):
    return tmp_path / "home"


@pytest.fixture
def spawn(repo, home):
    """Spawn diffler on the fixture repo with an isolated environment.
    Extra CLI args go through positionally; env vars via `env_extra`."""
    children = []

    def _spawn(*args, env_extra=None):
        env = tui_env(home, **(env_extra or {}))
        tui = Tui([str(BIN), *args, str(repo)], cwd=str(repo), env=env)
        children.append(tui)
        return tui

    yield _spawn
    for tui in children:
        tui.close()
