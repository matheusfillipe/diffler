import pytest

# Status screen against a colocated jj repo through a real PTY: no staging
# area, so the working copy reads as one section and the stage hint is gone.
@pytest.mark.parametrize("spawn", ["jj_repo"], indirect=True)
def test_status_screen_folds_into_one_working_copy_section(spawn):
    tui = spawn("--no-mcp")
    for expected in (
        "c commit",
        "b branch",
        "x discard",
        "Head:",
        "main",
        "Working copy (@) (2)",
        "notes.txt",
        "app.txt",
    ):
        tui.wait_for(expected)
    assert "s stage" not in tui.text()
    assert tui.quit() == 0
