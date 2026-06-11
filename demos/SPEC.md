# Diffler UI Demo Spec

Goal: mock UI demo (fake data, no git/MCP/LSP) to evaluate language/TUI-library choices.
Each demo lives in its own folder, runs standalone, implements THIS spec so comparison is fair.

## Vibe

doom-emacs / opencode / GitHub-dark. Dark full-background theming (not transparent terminal bg):

- bg: `#0d1117`, panel bg: `#161b22`, selection/cursor-line bg: `#21262d`
- fg: `#e6edf3`, dim fg: `#8b949e`
- accent: `#58a6ff` (blue), `#bc8cff` (purple)
- diff: deleted line bg `#3c1618`, added line bg `#12352a`
- intra-line emphasis (changed words/chars): deleted `#8b2c2f` bg, added `#1f6f48` bg
- status bar: powerline-ish, mode chip (e.g. ` NORMAL ` / ` REVIEW `) with colored bg
- borders: rounded where supported, subtle (`#30363d`)

## Hard requirements (all demos)

1. Alternate screen, raw mode, clean restore on exit (`q` quits).
2. Mouse support: click to select file in sidebar, wheel to scroll diff.
3. Works inside tmux.
4. Text selection: framework-native select-to-copy if available (OpenTUI/Textual);
   otherwise document the Shift+drag terminal fallback in the demo README.
5. Keyboard: `j/k` move, `Tab` cycle panel focus, `Enter` open/select, `c` comment, `q` quit.

## Screens

### 1. Home (magit-style status)

Sections (collapsible feel; static is fine):

```
  diffler — ~/projects/acme                                    main ⇡2
  ────────────────────────────────────────────────────────────
  Workspaces (2)
    ● main        ~/projects/acme              3 files changed
      agent/fix-auth  ~/projects/acme-fix-auth  2 files changed  [claude: running]

  Changes (3)
    M src/auth.py        +18 −4
    M src/session.py     +6  −1
    A tests/test_auth.py +42 −0

  Recent commits
    a1b2c3d fix: token expiry check off-by-one
    d4e5f6a feat: session refresh endpoint
```

`Enter` on a changed file → Diff view.

### 2. Diff view (the money screen)

- Unified diff, GitHub-dark styling per colors above.
- Syntax highlighting on code (any mechanism; fake/minimal token coloring acceptable if the
  framework lacks a highlighter — but say so in README).
- Intra-line word/char-level highlight on changed line pairs (use the mock data below; the
  pairs are designed to show char-level diffs).
- Line numbers (old/new columns), hunk headers (`@@ -12,7 +12,9 @@` styled dim on panel bg).
- One mock review comment rendered inline under a line: author chip `mattf`, comment box
  with rounded border, and one mock verdict chip on a hunk: `✓ accepted` (green) on hunk 1,
  `pending` (dim) on hunk 2.
- Pressing `c` on a line opens a comment input box inline (text input; doesn't need to persist).
- Footer hint bar: `j/k scroll  c comment  a accept hunk  x reject hunk  Tab files  q back`.

## Mock data (use EXACTLY this, embed as constants)

File: `src/auth.py`, 2 hunks.

Hunk 1 (`@@ -10,7 +10,9 @@ def validate_token(token):`):

```
 context:  def validate_token(token):
 context:      claims = decode(token)
-old:          if claims.expiry < now():
+new:          if claims.expiry <= now() - LEEWAY:
-old:              raise TokenError("expired")
+new:              raise TokenExpiredError("expired", claims.expiry)
 context:      return claims
+new:      audit_log("token.validated", claims.sub)
 context:
```

Hunk 2 (`@@ -31,6 +33,7 @@ def refresh_session(session_id):`):

```
 context:  def refresh_session(session_id):
 context:      session = store.get(session_id)
-old:      session.touch()
+new:      session.touch(now())
 context:      store.put(session)
+new:      metrics.incr("session.refresh")
 context:      return session
```

Mock comment under the `claims.expiry <= now() - LEEWAY` line:
> mattf: why LEEWAY here? clock skew between services? add a comment or link the incident.

## Deliverables per demo folder

- Runnable app (`README.md` with exact run command, e.g. `cargo run`, `bun start`, `go run .`,
  `uv run main.py`).
- `README.md` also: framework version, lines of code, what was easy, what was painful,
  select-to-copy story, tmux quirks found.
- Keep it ~one file of app code if possible. This is a mock, not a product. No tests needed.
