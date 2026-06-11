"""diffler TUI demo (mock data) — Textual implementation of demos/SPEC.md."""

from __future__ import annotations

from dataclasses import dataclass, field
from difflib import SequenceMatcher

from rich.style import Style
from rich.syntax import Syntax
from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import Screen
from textual.widgets import Input, Static

# GitHub-dark palette (per SPEC.md)
BG = "#0d1117"
PANEL = "#161b22"
SELECT = "#21262d"
FG = "#e6edf3"
DIM = "#8b949e"
BLUE = "#58a6ff"
PURPLE = "#bc8cff"
DEL_BG = "#3c1618"
ADD_BG = "#12352a"
DEL_EM = "#8b2c2f"
ADD_EM = "#1f6f48"
BORDER = "#30363d"
GREEN = "#3fb950"
RED = "#f85149"

# ---------------------------------------------------------------- mock data

WORKSPACES = [
    ("●", "main", "~/projects/acme", "3 files changed", ""),
    (" ", "agent/fix-auth", "~/projects/acme-fix-auth", "2 files changed", "[claude: running]"),
]

CHANGES = [
    ("M", "src/auth.py", "+18 −4"),
    ("M", "src/session.py", "+6  −1"),
    ("A", "tests/test_auth.py", "+42 −0"),
]

COMMITS = [
    ("a1b2c3d", "fix: token expiry check off-by-one"),
    ("d4e5f6a", "feat: session refresh endpoint"),
]


@dataclass
class Hunk:
    header: str
    old_start: int
    new_start: int
    lines: list[tuple[str, str]]  # (kind " "/"-"/"+", code)
    verdict: str  # "accepted" | "rejected" | "pending"


HUNKS = [
    Hunk(
        header="@@ -10,7 +10,9 @@ def validate_token(token):",
        old_start=10,
        new_start=10,
        lines=[
            (" ", "def validate_token(token):"),
            (" ", "    claims = decode(token)"),
            ("-", "    if claims.expiry < now():"),
            ("+", "    if claims.expiry <= now() - LEEWAY:"),
            ("-", '        raise TokenError("expired")'),
            ("+", '        raise TokenExpiredError("expired", claims.expiry)'),
            (" ", "    return claims"),
            ("+", '    audit_log("token.validated", claims.sub)'),
            (" ", ""),
        ],
        verdict="accepted",
    ),
    Hunk(
        header="@@ -31,6 +33,7 @@ def refresh_session(session_id):",
        old_start=31,
        new_start=33,
        lines=[
            (" ", "def refresh_session(session_id):"),
            (" ", "    session = store.get(session_id)"),
            ("-", "    session.touch()"),
            ("+", "    session.touch(now())"),
            (" ", "    store.put(session)"),
            ("+", '    metrics.incr("session.refresh")'),
            (" ", "    return session"),
        ],
        verdict="pending",
    ),
]

COMMENT_ANCHOR = "    if claims.expiry <= now() - LEEWAY:"
COMMENT = (
    "mattf",
    "why LEEWAY here? clock skew between services? add a comment or link the incident.",
)

# ---------------------------------------------------------------- rendering

_SYNTAX = Syntax("", "python", theme="github-dark")


def highlight_code(code: str) -> Text:
    text = _SYNTAX.highlight(code)
    text.rstrip()
    return text


def intra_line_ranges(old: str, new: str) -> tuple[list[tuple[int, int]], list[tuple[int, int]]]:
    """Char ranges that differ between an old/new line pair."""
    old_ranges: list[tuple[int, int]] = []
    new_ranges: list[tuple[int, int]] = []
    for tag, i1, i2, j1, j2 in SequenceMatcher(None, old, new).get_opcodes():
        if tag in ("replace", "delete") and i2 > i1:
            old_ranges.append((i1, i2))
        if tag in ("replace", "insert") and j2 > j1:
            new_ranges.append((j1, j2))
    return old_ranges, new_ranges


def chip(label: str, fg: str, bg: str, bold: bool = True) -> Text:
    return Text(f" {label} ", Style(color=fg, bgcolor=bg, bold=bold))


@dataclass
class DiffLine:
    kind: str  # "ctx" | "del" | "add"
    code: str
    old_no: int | None
    new_no: int | None
    hunk_idx: int
    em_ranges: list[tuple[int, int]] = field(default_factory=list)
    has_comment: bool = False


def build_diff_lines() -> list[DiffLine]:
    out: list[DiffLine] = []
    for hunk_idx, hunk in enumerate(HUNKS):
        # Pair consecutive -/+ runs for intra-line emphasis.
        pairs: dict[int, list[tuple[int, int]]] = {}
        i = 0
        while i < len(hunk.lines):
            if hunk.lines[i][0] == "-":
                dels = []
                while i < len(hunk.lines) and hunk.lines[i][0] == "-":
                    dels.append(i)
                    i += 1
                adds = []
                while i < len(hunk.lines) and hunk.lines[i][0] == "+":
                    adds.append(i)
                    i += 1
                for di, ai in zip(dels, adds):
                    old_r, new_r = intra_line_ranges(hunk.lines[di][1], hunk.lines[ai][1])
                    pairs[di] = old_r
                    pairs[ai] = new_r
            else:
                i += 1
        old_no, new_no = hunk.old_start, hunk.new_start
        for idx, (marker, code) in enumerate(hunk.lines):
            if marker == "-":
                line = DiffLine("del", code, old_no, None, hunk_idx, pairs.get(idx, []))
                old_no += 1
            elif marker == "+":
                line = DiffLine("add", code, None, new_no, hunk_idx, pairs.get(idx, []))
                new_no += 1
            else:
                line = DiffLine("ctx", code, old_no, new_no, hunk_idx)
                old_no += 1
                new_no += 1
            line.has_comment = code == COMMENT_ANCHOR and marker == "+"
            out.append(line)
    return out


def render_diff_line(line: DiffLine, cursor: bool) -> Text:
    text = Text(no_wrap=True)
    text.append("▍" if cursor else " ", Style(color=BLUE))
    old = "" if line.old_no is None else str(line.old_no)
    new = "" if line.new_no is None else str(line.new_no)
    text.append(f"{old:>4} {new:>4} ", Style(color=DIM))
    marker = {"del": "-", "add": "+"}[line.kind] if line.kind != "ctx" else " "
    marker_fg = {"del": RED, "add": GREEN}.get(line.kind, DIM)
    text.append(f"{marker} ", Style(color=marker_fg))
    code = highlight_code(line.code)
    em_bg = DEL_EM if line.kind == "del" else ADD_EM
    for start, end in line.em_ranges:
        code.stylize(Style(bgcolor=em_bg), start, end)
    text.append_text(code)
    return text


def render_hunk_header(hunk: Hunk) -> Text:
    text = Text("  ")
    text.append(hunk.header, Style(color=DIM, italic=True))
    text.append("  ")
    if hunk.verdict == "accepted":
        text.append_text(chip("✓ accepted", GREEN, ADD_BG))
    elif hunk.verdict == "rejected":
        text.append_text(chip("✗ rejected", RED, DEL_BG))
    else:
        text.append_text(chip("pending", DIM, SELECT, bold=False))
    return text


def comment_text(author: str, body: str) -> Text:
    text = Text()
    text.append_text(chip(author, BG, PURPLE))
    text.append(" ")
    text.append(body, Style(color=FG))
    return text


# ---------------------------------------------------------------- widgets


class StatusBar(Static):
    """Powerline-ish top bar with a mode chip."""

    def __init__(self, mode: str, mode_bg: str, path: str, right: str) -> None:
        super().__init__()
        text = Text()
        text.append(f" {mode} ", Style(color=BG, bgcolor=mode_bg, bold=True))
        text.append("", Style(color=mode_bg, bgcolor=PANEL))
        text.append(f" {path} ", Style(color=FG, bgcolor=PANEL))
        text.append("", Style(color=PANEL))
        self._left = text
        self._right = right

    def render(self) -> Text:
        text = Text(no_wrap=True)
        text.append_text(self._left)
        right = Text(f"{self._right} ", Style(color=BLUE, bold=True))
        gap = self.size.width - text.cell_len - right.cell_len
        if gap > 0:
            text.append(" " * gap)
        text.append_text(right)
        return text


class HomeRow(Static):
    """Navigable row on the home screen."""

    def __init__(self, index: int, build, payload=None) -> None:
        super().__init__(build(False))
        self.index = index
        self.build = build
        self.payload = payload

    def set_cursor(self, cursor: bool) -> None:
        self.set_class(cursor, "cursor")
        self.update(self.build(cursor))

    def on_click(self) -> None:
        self.screen.move_cursor_to(self.index)  # type: ignore[attr-defined]


class DiffRow(Static):
    """One diff line; navigable with the cursor."""

    def __init__(self, index: int, line: DiffLine) -> None:
        super().__init__(render_diff_line(line, False))
        self.index = index
        self.line = line
        self.add_class(f"line-{line.kind}")

    def set_cursor(self, cursor: bool) -> None:
        self.set_class(cursor, "cursor")
        self.update(render_diff_line(self.line, cursor))

    def on_click(self) -> None:
        self.screen.move_cursor_to(self.index)  # type: ignore[attr-defined]


class HunkHeader(Static):
    def __init__(self, hunk_idx: int) -> None:
        super().__init__(render_hunk_header(HUNKS[hunk_idx]))
        self.hunk_idx = hunk_idx

    def refresh_verdict(self) -> None:
        self.update(render_hunk_header(HUNKS[self.hunk_idx]))


class FileRow(Static):
    def __init__(self, index: int, status: str, path: str, stats: str) -> None:
        text = Text(" ")
        text.append(status, Style(color=GREEN if status == "A" else BLUE, bold=True))
        text.append(f" {path}", Style(color=FG))
        super().__init__(text)
        self.index = index

    def on_click(self) -> None:
        self.screen.select_file(self.index)  # type: ignore[attr-defined]


class CommentInput(Input):
    BINDINGS = [Binding("escape", "cancel", "cancel")]

    def action_cancel(self) -> None:
        self.screen.cancel_comment()  # type: ignore[attr-defined]


class Sidebar(VerticalScroll, can_focus=True):
    pass


class DiffPanel(VerticalScroll, can_focus=True):
    pass


# ---------------------------------------------------------------- screens


class HomeScreen(Screen):
    BINDINGS = [
        Binding("j,down", "cursor_down", "down", show=False),
        Binding("k,up", "cursor_up", "up", show=False),
        Binding("tab", "next_section", "section", show=False),
        Binding("enter", "open", "open", show=False),
        Binding("q", "quit", "quit", show=False),
    ]

    def __init__(self) -> None:
        super().__init__()
        self.cursor = 2  # first changed file
        self.rows: list[HomeRow] = []

    def compose(self) -> ComposeResult:
        yield StatusBar("NORMAL", BLUE, "diffler — ~/projects/acme", "main ⇡2")
        with VerticalScroll(id="home-body"):
            yield Static(Text("─" * 200, Style(color=BORDER), no_wrap=True), classes="rule")
            yield Static(self._section("Workspaces (2)"), classes="section")
            for i, ws in enumerate(WORKSPACES):
                yield self._row(i, self._workspace_builder(ws))
            yield Static(self._section("Changes (3)"), classes="section")
            for i, change in enumerate(CHANGES):
                yield self._row(2 + i, self._change_builder(change), payload=change[1])
            yield Static(self._section("Recent commits"), classes="section")
            for i, commit in enumerate(COMMITS):
                yield self._row(5 + i, self._commit_builder(commit))
        yield Static(
            Text("  j/k move  Tab section  Enter open  click select  q quit", Style(color=DIM)),
            id="footer",
        )

    def _row(self, index: int, build, payload=None) -> HomeRow:
        row = HomeRow(index, build, payload)
        self.rows.append(row)
        return row

    @staticmethod
    def _section(title: str) -> Text:
        return Text(f"  {title}", Style(color=PURPLE, bold=True))

    @staticmethod
    def _workspace_builder(ws):
        dot, name, path, stat, agent = ws

        def build(cursor: bool) -> Text:
            text = Text("    ")
            text.append(f"{dot} ", Style(color=GREEN))
            text.append(f"{name:<16}", Style(color=FG, bold=True))
            text.append(f"{path:<28}", Style(color=DIM))
            text.append(f"{stat}", Style(color=FG))
            if agent:
                text.append(f"  {agent}", Style(color=PURPLE))
            return text

        return build

    @staticmethod
    def _change_builder(change):
        status, path, stats = change

        def build(cursor: bool) -> Text:
            text = Text("    ")
            text.append(status, Style(color=GREEN if status == "A" else BLUE, bold=True))
            text.append(f" {path:<20}", Style(color=FG))
            plus, minus = stats.split(maxsplit=1)
            text.append(f"{plus} ", Style(color=GREEN))
            text.append(minus, Style(color=RED))
            return text

        return build

    @staticmethod
    def _commit_builder(commit):
        sha, msg = commit

        def build(cursor: bool) -> Text:
            text = Text("    ")
            text.append(sha, Style(color=BLUE))
            text.append(f" {msg}", Style(color=DIM))
            return text

        return build

    def on_mount(self) -> None:
        self.move_cursor_to(self.cursor)

    def move_cursor_to(self, index: int) -> None:
        self.rows[self.cursor].set_cursor(False)
        self.cursor = max(0, min(index, len(self.rows) - 1))
        row = self.rows[self.cursor]
        row.set_cursor(True)
        row.scroll_visible()

    def action_cursor_down(self) -> None:
        self.move_cursor_to(self.cursor + 1)

    def action_cursor_up(self) -> None:
        self.move_cursor_to(self.cursor - 1)

    def action_next_section(self) -> None:
        # Sections start at row indexes 0 (workspaces), 2 (changes), 5 (commits).
        starts = [0, 2, 5]
        nxt = next((s for s in starts if s > self.cursor), starts[0])
        self.move_cursor_to(nxt)

    def action_open(self) -> None:
        payload = self.rows[self.cursor].payload
        if payload is not None:
            self.app.push_screen(DiffScreen(payload))

    def action_quit(self) -> None:
        self.app.exit()


class DiffScreen(Screen):
    BINDINGS = [
        Binding("j,down", "cursor_down", "down", show=False),
        Binding("k,up", "cursor_up", "up", show=False),
        Binding("c", "comment", "comment", show=False),
        Binding("a", "verdict('accepted')", "accept", show=False),
        Binding("x", "verdict('rejected')", "reject", show=False),
        Binding("tab", "cycle_focus", "files", show=False),
        Binding("enter", "select", "select", show=False),
        Binding("q,escape", "back", "back", show=False),
    ]

    def __init__(self, path: str) -> None:
        super().__init__()
        self.path = path
        self.cursor = 0
        self.rows: list[DiffRow] = []
        self.hunk_headers: list[HunkHeader] = []
        self.file_idx = next(i for i, c in enumerate(CHANGES) if c[1] == path)
        self.comment_open = False

    def compose(self) -> ComposeResult:
        yield StatusBar("REVIEW", PURPLE, f"diffler — {self.path}", "main ⇡2")
        with Horizontal(id="diff-layout"):
            with Sidebar(id="sidebar"):
                yield Static(Text(" Changes", Style(color=PURPLE, bold=True)), classes="section")
                for i, (status, path, stats) in enumerate(CHANGES):
                    yield FileRow(i, status, path, stats)
            yield DiffPanel(id="diff-panel")
        yield Static(
            Text(
                "  j/k scroll  c comment  a accept hunk  x reject hunk  Tab files  q back",
                Style(color=DIM),
            ),
            id="footer",
        )

    def on_mount(self) -> None:
        self._load_file()
        self.query_one(DiffPanel).focus()
        self._mark_file_rows()

    def _mark_file_rows(self) -> None:
        for row in self.query(FileRow):
            row.set_class(row.index == self.file_idx, "selected")

    def _load_file(self) -> None:
        panel = self.query_one(DiffPanel)
        panel.remove_children()
        self.rows = []
        self.hunk_headers = []
        self.cursor = 0
        if self.path != "src/auth.py":
            panel.mount(
                Static(
                    Text(
                        f"\n  no mock diff for {self.path} — SPEC mock data covers src/auth.py only",
                        Style(color=DIM, italic=True),
                    )
                )
            )
            return
        lines = build_diff_lines()
        last_hunk = -1
        index = 0
        for line in lines:
            if line.hunk_idx != last_hunk:
                header = HunkHeader(line.hunk_idx)
                self.hunk_headers.append(header)
                panel.mount(header)
                last_hunk = line.hunk_idx
            row = DiffRow(index, line)
            self.rows.append(row)
            panel.mount(row)
            index += 1
            if line.has_comment:
                panel.mount(Static(comment_text(*COMMENT), classes="comment-box"))
        if self.rows:
            self.rows[0].set_cursor(True)

    def move_cursor_to(self, index: int) -> None:
        if not self.rows:
            return
        self.rows[self.cursor].set_cursor(False)
        self.cursor = max(0, min(index, len(self.rows) - 1))
        row = self.rows[self.cursor]
        row.set_cursor(True)
        row.scroll_visible()

    def select_file(self, index: int) -> None:
        self.file_idx = index
        self.path = CHANGES[index][1]
        self._mark_file_rows()
        self._load_file()

    def action_cursor_down(self) -> None:
        if isinstance(self.focused, Sidebar):
            self.select_file(min(self.file_idx + 1, len(CHANGES) - 1))
        else:
            self.move_cursor_to(self.cursor + 1)

    def action_cursor_up(self) -> None:
        if isinstance(self.focused, Sidebar):
            self.select_file(max(self.file_idx - 1, 0))
        else:
            self.move_cursor_to(self.cursor - 1)

    def action_cycle_focus(self) -> None:
        if isinstance(self.focused, Sidebar):
            self.query_one(DiffPanel).focus()
        else:
            self.query_one(Sidebar).focus()

    def action_select(self) -> None:
        if isinstance(self.focused, Sidebar):
            self.query_one(DiffPanel).focus()

    def action_verdict(self, verdict: str) -> None:
        if not self.rows:
            return
        hunk_idx = self.rows[self.cursor].line.hunk_idx
        HUNKS[hunk_idx].verdict = verdict
        self.hunk_headers[hunk_idx].refresh_verdict()

    def action_comment(self) -> None:
        if self.comment_open or not self.rows:
            return
        self.comment_open = True
        box = CommentInput(placeholder="leave a comment… (Enter submit, Esc cancel)")
        self.query_one(DiffPanel).mount(box, after=self.rows[self.cursor])
        box.focus()

    def cancel_comment(self) -> None:
        self.query_one(CommentInput).remove()
        self.comment_open = False
        self.query_one(DiffPanel).focus()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        anchor = self.rows[self.cursor]
        if event.value.strip():
            self.query_one(DiffPanel).mount(
                Static(comment_text("you", event.value.strip()), classes="comment-box"),
                after=anchor,
            )
        self.cancel_comment()

    def action_back(self) -> None:
        self.app.pop_screen()


# ---------------------------------------------------------------- app


class DifflerApp(App):
    CSS = f"""
    Screen {{
        background: {BG};
        color: {FG};
    }}
    StatusBar {{
        dock: top;
        height: 1;
        background: {BG};
    }}
    #footer {{
        dock: bottom;
        height: 1;
        background: {PANEL};
    }}
    #home-body {{
        scrollbar-size: 1 1;
    }}
    .section {{
        margin-top: 1;
    }}
    .rule {{
        color: {BORDER};
    }}
    HomeRow.cursor {{
        background: {SELECT};
    }}
    #diff-layout {{
        height: 1fr;
    }}
    #sidebar {{
        width: 26;
        background: {PANEL};
        border: round {BORDER};
        scrollbar-size: 1 1;
    }}
    #sidebar:focus {{
        border: round {BLUE};
    }}
    FileRow {{
        padding: 0 1;
    }}
    FileRow.selected {{
        background: {SELECT};
        color: {BLUE};
    }}
    #diff-panel {{
        background: {BG};
        border: round {BORDER};
        scrollbar-size: 1 1;
    }}
    #diff-panel:focus {{
        border: round {PURPLE};
    }}
    DiffRow.line-del {{
        background: {DEL_BG};
    }}
    DiffRow.line-add {{
        background: {ADD_BG};
    }}
    DiffRow.line-ctx.cursor {{
        background: {SELECT};
    }}
    HunkHeader {{
        background: {PANEL};
        margin: 1 0 0 0;
    }}
    .comment-box {{
        border: round {BORDER};
        background: {PANEL};
        margin: 0 4 0 12;
        padding: 0 1;
        max-width: 80;
    }}
    CommentInput {{
        border: round {BLUE};
        background: {PANEL};
        margin: 0 4 0 12;
        max-width: 80;
    }}
    """

    def on_mount(self) -> None:
        self.push_screen(HomeScreen())


if __name__ == "__main__":
    DifflerApp().run()
