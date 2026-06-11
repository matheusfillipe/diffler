// diffler UI demo — TypeScript + @opentui/core
// Mock data only; implements demos/SPEC.md (Home + Diff view, GitHub-dark).
import {
  createCliRenderer,
  BoxRenderable,
  TextRenderable,
  ScrollBoxRenderable,
  InputRenderable,
  InputRenderableEvents,
  StyledText,
  fg,
  bg,
  bold,
  italic,
  type CliRenderer,
  type KeyEvent,
  type MouseEvent,
  type TextChunk,
  type StylableInput,
} from "@opentui/core"

// ---------------------------------------------------------------- theme

const C = {
  bg: "#0d1117",
  panel: "#161b22",
  cursorLine: "#21262d",
  fg: "#e6edf3",
  dim: "#8b949e",
  blue: "#58a6ff",
  purple: "#bc8cff",
  delBg: "#3c1618",
  addBg: "#12352a",
  delEmph: "#8b2c2f",
  addEmph: "#1f6f48",
  border: "#30363d",
  green: "#3fb950",
  red: "#f85149",
  yellow: "#d29922",
} as const

// brighter row bg when the diff cursor sits on a colored line
const CURSOR_BG: Record<string, string> = {
  ctx: C.cursorLine,
  del: "#4d2023",
  add: "#1a4435",
}
const BASE_BG: Record<string, string> = { ctx: C.bg, del: C.delBg, add: C.addBg }

// ---------------------------------------------------------------- mock data (per SPEC.md)

interface MockLine {
  k: "ctx" | "del" | "add"
  text: string
  pair?: number // del/add lines sharing a pair id get intra-line char highlighting
  comment?: { author: string; text: string }
}

interface MockHunk {
  header: string
  oldStart: number
  newStart: number
  verdict: "accepted" | "pending" | "rejected"
  lines: MockLine[]
}

const MOCK_COMMENT = {
  author: "mattf",
  text: "why LEEWAY here? clock skew between services? add a comment or link the incident.",
}

const AUTH_HUNKS: MockHunk[] = [
  {
    header: "@@ -10,7 +10,9 @@ def validate_token(token):",
    oldStart: 10,
    newStart: 10,
    verdict: "accepted",
    lines: [
      { k: "ctx", text: "def validate_token(token):" },
      { k: "ctx", text: "    claims = decode(token)" },
      { k: "del", text: "    if claims.expiry < now():", pair: 1 },
      { k: "add", text: "    if claims.expiry <= now() - LEEWAY:", pair: 1, comment: MOCK_COMMENT },
      { k: "del", text: '        raise TokenError("expired")', pair: 2 },
      { k: "add", text: '        raise TokenExpiredError("expired", claims.expiry)', pair: 2 },
      { k: "ctx", text: "    return claims" },
      { k: "add", text: '    audit_log("token.validated", claims.sub)' },
      { k: "ctx", text: "" },
    ],
  },
  {
    header: "@@ -31,6 +33,7 @@ def refresh_session(session_id):",
    oldStart: 31,
    newStart: 33,
    verdict: "pending",
    lines: [
      { k: "ctx", text: "def refresh_session(session_id):" },
      { k: "ctx", text: "    session = store.get(session_id)" },
      { k: "del", text: "    session.touch()", pair: 1 },
      { k: "add", text: "    session.touch(now())", pair: 1 },
      { k: "ctx", text: "    store.put(session)" },
      { k: "add", text: '    metrics.incr("session.refresh")' },
      { k: "ctx", text: "    return session" },
    ],
  },
]

const FILES = [
  { status: "M", path: "src/auth.py", plus: "+18", minus: "−4", hunks: AUTH_HUNKS },
  { status: "M", path: "src/session.py", plus: "+6", minus: "−1", hunks: null },
  { status: "A", path: "tests/test_auth.py", plus: "+42", minus: "−0", hunks: null },
]

const WORKSPACES = [
  { sel: "●", name: "main", path: "~/projects/acme", info: "3 files changed", agent: "" },
  { sel: " ", name: "agent/fix-auth", path: "~/projects/acme-fix-auth", info: "2 files changed", agent: "[claude: running]" },
]

const COMMITS = [
  { sha: "a1b2c3d", msg: "fix: token expiry check off-by-one" },
  { sha: "d4e5f6a", msg: "feat: session refresh endpoint" },
]

// ---------------------------------------------------------------- tiny python highlighter

const PY_KW = new Set(["def", "if", "elif", "else", "return", "raise", "class", "import", "from", "not", "in", "and", "or", "for", "while", "with", "as", "pass"])

interface Span {
  start: number
  end: number
  color: string
  bold?: boolean
  italic?: boolean
}

function tokenizePython(line: string): Span[] {
  const spans: Span[] = []
  const re = /(#.*$)|("[^"]*"|'[^']*')|\b(\d+(?:\.\d+)?)\b|\b([A-Za-z_][A-Za-z0-9_]*)\b/g
  let m: RegExpExecArray | null
  while ((m = re.exec(line)) !== null) {
    const start = m.index
    const end = start + m[0].length
    if (m[1]) spans.push({ start, end, color: C.dim, italic: true })
    else if (m[2]) spans.push({ start, end, color: "#a5d6ff" })
    else if (m[3]) spans.push({ start, end, color: "#79c0ff" })
    else if (m[4]) {
      const word = m[4]
      if (PY_KW.has(word)) spans.push({ start, end, color: "#ff7b72", bold: true })
      else if (word === word.toUpperCase() && word.length > 1) spans.push({ start, end, color: "#79c0ff" })
      else if (line[end] === "(") spans.push({ start, end, color: "#d2a8ff" })
      else spans.push({ start, end, color: C.fg })
    }
  }
  return spans
}

// char-level diff: common prefix/suffix → middle range differs
function charDiffRanges(a: string, b: string): { a: [number, number]; b: [number, number] } {
  let p = 0
  while (p < a.length && p < b.length && a[p] === b[p]) p++
  let s = 0
  while (s < a.length - p && s < b.length - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s++
  return { a: [p, a.length - s], b: [p, b.length - s] }
}

// styled chunks for a code line: syntax fg colors + optional emphasis bg range
function codeChunks(text: string, emph: [number, number] | null, emphBg: string): TextChunk[] {
  const spans = tokenizePython(text)
  const cuts = new Set<number>([0, text.length])
  for (const sp of spans) {
    cuts.add(sp.start)
    cuts.add(sp.end)
  }
  if (emph) {
    cuts.add(emph[0])
    cuts.add(emph[1])
  }
  const points = [...cuts].sort((x, y) => x - y)
  const chunks: TextChunk[] = []
  for (let i = 0; i < points.length - 1; i++) {
    const [s, e] = [points[i], points[i + 1]]
    const seg = text.slice(s, e)
    if (!seg) continue
    const sp = spans.find((x) => x.start <= s && x.end >= e)
    let chunk: StylableInput = seg
    chunk = fg(sp?.color ?? C.fg)(chunk)
    if (sp?.bold) chunk = bold(chunk)
    if (sp?.italic) chunk = italic(chunk)
    if (emph && s >= emph[0] && e <= emph[1]) chunk = bg(emphBg)(chunk)
    chunks.push(chunk as TextChunk)
  }
  return chunks
}

function styled(...chunks: (TextChunk | string)[]): StyledText {
  return new StyledText(chunks.map((c) => (typeof c === "string" ? fg(C.fg)(c) : c)))
}

function chip(label: string, bgColor: string, fgColor = "#0d1117"): TextChunk {
  return bold(bg(bgColor)(fg(fgColor)(` ${label} `)))
}

// ---------------------------------------------------------------- app state

type Screen = "home" | "diff"
type Panel = "files" | "diff"

interface DiffRow {
  box: BoxRenderable
  kind: "ctx" | "del" | "add"
  hunkIdx: number
}

const state = {
  screen: "home" as Screen,
  homeIdx: 0,
  filesIdx: 0,
  panel: "diff" as Panel,
  cursor: 0,
  commentOpen: false,
  selectionText: "",
}

let renderer: CliRenderer
let homeScreen: BoxRenderable
let diffScreen: BoxRenderable | null = null
let statusBar: BoxRenderable
let statusText: TextRenderable
let homeRows: { box: BoxRenderable; section: number; fileIdx?: number }[] = []
let diffRows: DiffRow[] = []
let fileRows: BoxRenderable[] = []
let scrollBox: ScrollBoxRenderable | null = null
let sidebar: BoxRenderable | null = null
let diffPanel: BoxRenderable | null = null
let hunkChips: TextRenderable[] = []
let verdicts: ("accepted" | "pending" | "rejected")[] = []
let commentWrap: BoxRenderable | null = null

// ---------------------------------------------------------------- status bar

function updateStatusBar() {
  if (!statusText) return
  const mode = state.screen === "home" ? chip("NORMAL", C.blue) : chip("REVIEW", C.purple)
  const arrow = fg(state.screen === "home" ? C.blue : C.purple)(bg(C.panel)(""))
  const hints =
    state.screen === "home"
      ? "j/k move  Tab section  Enter open  q quit"
      : "mouse: click select · wheel scroll · drag select text · y yank selection"
  const left = state.screen === "home" ? " ~/projects/acme " : ` ${FILES[state.filesIdx].path} `
  statusText.content = styled(mode, arrow, fg(C.dim)(bg(C.panel)(left)), bg(C.panel)("  "), fg(C.dim)(bg(C.panel)(hints)))
}

// ---------------------------------------------------------------- home screen

function homeRowContent(i: number): StyledText {
  const r = homeRows[i]
  const sel = i === state.homeIdx
  const accent = sel ? fg(C.blue)("▌") : " "
  const item = r as any
  if (item.kind === "ws") {
    const w = WORKSPACES[item.idx]
    const name = w.name === "main" ? bold(fg(C.fg)(w.name.padEnd(16))) : fg(C.fg)(w.name.padEnd(16))
    const dot = w.sel === "●" ? fg(C.green)("● ") : "  "
    const agent = w.agent ? fg(C.yellow)("  " + w.agent) : fg(C.dim)("")
    return styled(accent, "   ", dot, name, fg(C.dim)(w.path.padEnd(26)), fg(C.dim)(w.info), agent)
  }
  if (item.kind === "file") {
    const f = FILES[item.idx]
    const st = f.status === "A" ? fg(C.green)(f.status) : fg(C.yellow)(f.status)
    return styled(accent, "   ", st, " ", fg(sel ? C.blue : C.fg)(f.path.padEnd(20)), fg(C.green)(f.plus.padEnd(4)), fg(C.red)(f.minus))
  }
  const c = COMMITS[item.idx]
  return styled(accent, "   ", fg(C.purple)(c.sha), " ", fg(C.fg)(c.msg))
}

function refreshHome() {
  for (let i = 0; i < homeRows.length; i++) {
    homeRows[i].box.backgroundColor = i === state.homeIdx ? C.cursorLine : "transparent"
    const text = homeRows[i].box.getChildren()[0] as TextRenderable
    text.content = homeRowContent(i)
  }
  updateStatusBar()
}

function buildHome() {
  homeScreen = new BoxRenderable(renderer, {
    id: "home",
    flexGrow: 1,
    flexDirection: "column",
    backgroundColor: C.bg,
    paddingTop: 1,
  })

  const header = new BoxRenderable(renderer, {
    id: "home-header",
    height: 1,
    flexDirection: "row",
    justifyContent: "space-between",
    paddingLeft: 2,
    paddingRight: 2,
  })
  header.add(
    new TextRenderable(renderer, {
      content: styled(bold(fg(C.blue)("diffler")), fg(C.dim)(" — "), fg(C.fg)("~/projects/acme")),
      selectionBg: C.blue,
      selectionFg: C.bg,
    }),
  )
  header.add(
    new TextRenderable(renderer, {
      content: styled(bold(fg(C.purple)("main")), fg(C.yellow)(" ⇡2")),
    }),
  )
  homeScreen.add(header)
  homeScreen.add(
    new BoxRenderable(renderer, { height: 1, marginLeft: 2, marginRight: 2, borderColor: C.border, border: ["top"], borderStyle: "single", marginTop: 1 }),
  )

  const addSectionTitle = (label: string) => {
    const box = new BoxRenderable(renderer, { height: 1, marginTop: 1, paddingLeft: 2 })
    box.add(new TextRenderable(renderer, { content: styled(bold(fg(C.blue)(label))), selectionBg: C.blue, selectionFg: C.bg }))
    homeScreen.add(box)
  }

  const addRow = (meta: any) => {
    const i = homeRows.length
    const box = new BoxRenderable(renderer, {
      id: `home-row-${i}`,
      height: 1,
      width: "100%",
      onMouseDown: () => {
        state.homeIdx = i
        refreshHome()
      },
    })
    box.add(new TextRenderable(renderer, { content: styled(" "), selectionBg: C.blue, selectionFg: C.bg }))
    homeScreen.add(box)
    homeRows.push({ box, ...meta })
  }

  addSectionTitle("Workspaces (2)")
  WORKSPACES.forEach((_, idx) => addRow({ kind: "ws", idx, section: 0 }))
  addSectionTitle("Changes (3)")
  FILES.forEach((_, idx) => addRow({ kind: "file", idx, fileIdx: idx, section: 1 }))
  addSectionTitle("Recent commits")
  COMMITS.forEach((_, idx) => addRow({ kind: "commit", idx, section: 2 }))

  state.homeIdx = WORKSPACES.length // first changed file
  refreshHome()
}

// ---------------------------------------------------------------- diff screen

function verdictChunk(v: "accepted" | "pending" | "rejected"): TextChunk {
  if (v === "accepted") return chip("✓ accepted", C.addEmph, C.fg)
  if (v === "rejected") return chip("✗ rejected", C.delEmph, C.fg)
  return bg(C.cursorLine)(fg(C.dim)(" pending "))
}

function gutter(oldNo: number | null, newNo: number | null, sign: string, kind: string): TextChunk[] {
  const gbg = kind === "del" ? "#2d1214" : kind === "add" ? "#0d2a20" : C.panel
  const signColor = kind === "del" ? C.red : kind === "add" ? C.green : C.dim
  const o = (oldNo === null ? "" : String(oldNo)).padStart(4)
  const n = (newNo === null ? "" : String(newNo)).padStart(4)
  return [bg(gbg)(fg(C.dim)(`${o} ${n} `)), fg(signColor)(`${sign} `)]
}

function makeCommentBox(author: string, text: string, idSuffix: string): BoxRenderable {
  const wrap = new BoxRenderable(renderer, {
    id: `comment-${idSuffix}`,
    flexDirection: "column",
    marginLeft: 12,
    marginRight: 4,
    border: true,
    borderStyle: "rounded",
    borderColor: C.border,
    backgroundColor: C.panel,
    paddingLeft: 1,
    paddingRight: 1,
  })
  const head = new BoxRenderable(renderer, { height: 1, flexDirection: "row" })
  head.add(
    new TextRenderable(renderer, {
      content: styled(chip(author, C.purple), fg(C.dim)(bg(C.panel)("  2 hours ago"))),
      selectionBg: C.blue,
      selectionFg: C.bg,
    }),
  )
  wrap.add(head)
  wrap.add(
    new TextRenderable(renderer, {
      content: styled(fg(C.fg)(bg(C.panel)(text))),
      selectionBg: C.blue,
      selectionFg: C.bg,
    }),
  )
  return wrap
}

function refreshDiffCursor() {
  for (let i = 0; i < diffRows.length; i++) {
    const r = diffRows[i]
    const active = state.panel === "diff" && i === state.cursor
    r.box.backgroundColor = active ? CURSOR_BG[r.kind] : BASE_BG[r.kind]
  }
  if (scrollBox && diffRows[state.cursor]) {
    scrollBox.scrollChildIntoView(diffRows[state.cursor].box.id)
  }
}

function refreshSidebar() {
  for (let i = 0; i < fileRows.length; i++) {
    fileRows[i].backgroundColor = i === state.filesIdx ? C.cursorLine : "transparent"
    const text = fileRows[i].getChildren()[0] as TextRenderable
    const f = FILES[i]
    const sel = i === state.filesIdx
    const st = f.status === "A" ? fg(C.green)(f.status) : fg(C.yellow)(f.status)
    text.content = styled(sel ? fg(C.blue)("▌") : " ", st, " ", fg(sel ? C.blue : C.fg)(f.path.padEnd(19)), fg(C.dim)(`${f.plus} ${f.minus}`))
  }
  if (sidebar) sidebar.borderColor = state.panel === "files" ? C.blue : C.border
  if (diffPanel) diffPanel.borderColor = state.panel === "diff" ? C.blue : C.border
}

function refreshHunkChips() {
  AUTH_HUNKS.forEach((h, i) => {
    if (hunkChips[i]) {
      hunkChips[i].content = styled(fg(C.dim)(bg(C.panel)(` ${h.header} `)), bg(C.panel)("  "), verdictChunk(verdicts[i]))
    }
  })
}

function buildDiffContent() {
  if (!scrollBox) return
  diffRows = []
  hunkChips = []
  const file = FILES[state.filesIdx]

  // file header inside the diff pane
  const fileHeader = new BoxRenderable(renderer, { id: "diff-file-header", height: 1, width: "100%", backgroundColor: C.panel, paddingLeft: 1 })
  fileHeader.add(
    new TextRenderable(renderer, {
      content: styled(bold(fg(C.fg)(bg(C.panel)(file.path))), bg(C.panel)("  "), fg(C.green)(bg(C.panel)(file.plus)), bg(C.panel)(" "), fg(C.red)(bg(C.panel)(file.minus))),
      selectionBg: C.blue,
      selectionFg: C.bg,
    }),
  )
  scrollBox.add(fileHeader)

  if (!file.hunks) {
    const note = new BoxRenderable(renderer, { height: 3, paddingLeft: 2, paddingTop: 1 })
    note.add(
      new TextRenderable(renderer, {
        content: styled(fg(C.dim)("(no mock diff for this file — select src/auth.py)")),
        selectionBg: C.blue,
        selectionFg: C.bg,
      }),
    )
    scrollBox.add(note)
    return
  }

  file.hunks.forEach((hunk, hi) => {
    const spacer = new BoxRenderable(renderer, { height: 1 })
    scrollBox!.add(spacer)

    const hdr = new BoxRenderable(renderer, { id: `hunk-${hi}`, height: 1, width: "100%", backgroundColor: C.panel, paddingLeft: 1 })
    const hdrText = new TextRenderable(renderer, { content: styled(" "), selectionBg: C.blue, selectionFg: C.bg })
    hdr.add(hdrText)
    hunkChips[hi] = hdrText
    scrollBox!.add(hdr)

    let oldNo = hunk.oldStart
    let newNo = hunk.newStart

    // resolve intra-line emphasis ranges for del/add pairs
    const emphFor = new Map<MockLine, [number, number]>()
    for (const line of hunk.lines) {
      if (line.k === "del" && line.pair !== undefined) {
        const partner = hunk.lines.find((l) => l.k === "add" && l.pair === line.pair)
        if (partner) {
          const r = charDiffRanges(line.text, partner.text)
          emphFor.set(line, r.a)
          emphFor.set(partner, r.b)
        }
      }
    }

    for (const line of hunk.lines) {
      let o: number | null = null
      let n: number | null = null
      let sign = " "
      if (line.k === "ctx") {
        o = oldNo++
        n = newNo++
      } else if (line.k === "del") {
        o = oldNo++
        sign = "-"
      } else {
        n = newNo++
        sign = "+"
      }
      const emph = emphFor.get(line) ?? null
      const emphBg = line.k === "del" ? C.delEmph : C.addEmph
      const rowIdx = diffRows.length
      const row = new BoxRenderable(renderer, {
        id: `diff-row-${hi}-${rowIdx}`,
        height: 1,
        width: "100%",
        backgroundColor: BASE_BG[line.k],
        flexDirection: "row",
        onMouseDown: () => {
          state.panel = "diff"
          state.cursor = rowIdx
          refreshSidebar()
          refreshDiffCursor()
          updateStatusBar()
        },
      })
      row.add(
        new TextRenderable(renderer, {
          content: new StyledText([...gutter(o, n, sign, line.k), ...codeChunks(line.text, emph, emphBg)]),
          selectionBg: C.blue,
          selectionFg: C.bg,
        }),
      )
      scrollBox!.add(row)
      diffRows.push({ box: row, kind: line.k, hunkIdx: hi })

      if (line.comment) {
        scrollBox!.add(makeCommentBox(line.comment.author, line.comment.text, `${hi}-${rowIdx}`))
      }
    }
  })

  refreshHunkChips()
}

function buildDiff() {
  diffScreen = new BoxRenderable(renderer, {
    id: "diff-screen",
    flexGrow: 1,
    flexDirection: "column",
    backgroundColor: C.bg,
  })

  const main = new BoxRenderable(renderer, { id: "diff-main", flexGrow: 1, flexDirection: "row" })

  sidebar = new BoxRenderable(renderer, {
    id: "sidebar",
    width: 30,
    flexDirection: "column",
    backgroundColor: C.panel,
    border: true,
    borderStyle: "rounded",
    borderColor: C.border,
    title: "files",
    titleAlignment: "left",
  })
  fileRows = []
  FILES.forEach((_, i) => {
    const row = new BoxRenderable(renderer, {
      id: `file-row-${i}`,
      height: 1,
      width: "100%",
      onMouseDown: () => {
        state.panel = "files"
        selectFile(i)
      },
    })
    row.add(new TextRenderable(renderer, { content: styled(" "), selectionBg: C.blue, selectionFg: C.bg }))
    sidebar!.add(row)
    fileRows.push(row)
  })

  diffPanel = new BoxRenderable(renderer, {
    id: "diff-panel",
    flexGrow: 1,
    flexDirection: "column",
    border: true,
    borderStyle: "rounded",
    borderColor: C.blue,
    backgroundColor: C.bg,
  })

  scrollBox = new ScrollBoxRenderable(renderer, {
    id: "diff-scroll",
    flexGrow: 1,
    rootOptions: { backgroundColor: C.bg },
    wrapperOptions: { backgroundColor: C.bg },
    viewportOptions: { backgroundColor: C.bg },
    contentOptions: { backgroundColor: C.bg, flexDirection: "column" },
    scrollbarOptions: {
      trackOptions: { foregroundColor: C.border, backgroundColor: C.panel },
    },
  })
  diffPanel.add(scrollBox)

  const footer = new BoxRenderable(renderer, { id: "diff-footer", height: 1, backgroundColor: C.panel, paddingLeft: 1 })
  footer.add(
    new TextRenderable(renderer, {
      content: styled(
        fg(C.dim)(bg(C.panel)("j/k scroll  ")),
        fg(C.blue)(bg(C.panel)("c")),
        fg(C.dim)(bg(C.panel)(" comment  ")),
        fg(C.green)(bg(C.panel)("a")),
        fg(C.dim)(bg(C.panel)(" accept hunk  ")),
        fg(C.red)(bg(C.panel)("x")),
        fg(C.dim)(bg(C.panel)(" reject hunk  ")),
        fg(C.blue)(bg(C.panel)("Tab")),
        fg(C.dim)(bg(C.panel)(" files  ")),
        fg(C.blue)(bg(C.panel)("q")),
        fg(C.dim)(bg(C.panel)(" back")),
      ),
    }),
  )

  main.add(sidebar)
  main.add(diffPanel)
  diffScreen.add(main)
  diffScreen.add(footer)

  buildDiffContent()
  refreshSidebar()
  refreshDiffCursor()
}

function rebuildDiffContent() {
  if (!scrollBox) return
  for (const child of scrollBox.getChildren()) {
    scrollBox.remove(child.id)
    child.destroyRecursively()
  }
  state.cursor = 0
  buildDiffContent()
  refreshDiffCursor()
}

function selectFile(i: number) {
  state.filesIdx = i
  refreshSidebar()
  rebuildDiffContent()
  updateStatusBar()
}

// ---------------------------------------------------------------- inline comment input

function openCommentInput() {
  if (!scrollBox || state.commentOpen || diffRows.length === 0) return
  const anchor = diffRows[state.cursor].box
  const children = scrollBox.getChildren()
  const anchorIdx = children.findIndex((c) => c.id === anchor.id)
  if (anchorIdx < 0) return

  state.commentOpen = true
  commentWrap = new BoxRenderable(renderer, {
    id: "comment-input-wrap",
    flexDirection: "column",
    marginLeft: 12,
    marginRight: 4,
    border: true,
    borderStyle: "rounded",
    borderColor: C.blue,
    backgroundColor: C.panel,
    title: " new comment — Enter to post · Esc to cancel ",
    titleAlignment: "left",
    paddingLeft: 1,
    paddingRight: 1,
  })
  const input = new InputRenderable(renderer, {
    id: "comment-input",
    placeholder: "leave a comment…",
    backgroundColor: C.panel,
    textColor: C.fg,
    focusedBackgroundColor: C.cursorLine,
    placeholderColor: C.dim,
    cursorColor: C.blue,
  } as any)
  commentWrap.add(input)
  scrollBox.add(commentWrap, anchorIdx + 1)
  input.focus()

  const close = () => {
    if (!commentWrap || !scrollBox) return
    input.blur()
    scrollBox.remove(commentWrap.id)
    commentWrap.destroyRecursively()
    commentWrap = null
    state.commentOpen = false
  }

  input.on(InputRenderableEvents.ENTER, () => {
    const value = input.value.trim()
    if (value && scrollBox) {
      const children = scrollBox.getChildren()
      const idx = children.findIndex((c) => c.id === "comment-input-wrap")
      scrollBox.add(makeCommentBox("you", value, `new-${Date.now()}`), idx)
    }
    close()
  })
  ;(input as any)._closeCommentInput = close
}

function closeCommentInput() {
  if (!commentWrap || !scrollBox) return
  scrollBox.remove(commentWrap.id)
  commentWrap.destroyRecursively()
  commentWrap = null
  state.commentOpen = false
}

// ---------------------------------------------------------------- navigation

function showDiff(fileIdx: number) {
  state.screen = "diff"
  state.filesIdx = fileIdx
  state.panel = "diff"
  state.cursor = 0
  homeScreen.visible = false
  if (diffScreen) {
    diffScreen.visible = true
    selectFile(fileIdx)
  } else {
    buildDiff()
    appColumn.add(diffScreen!, 0)
  }
  refreshSidebar()
  refreshDiffCursor()
  updateStatusBar()
}

function showHome() {
  state.screen = "home"
  closeCommentInput()
  if (diffScreen) diffScreen.visible = false
  homeScreen.visible = true
  refreshHome()
}

function quit() {
  renderer.destroy()
  process.exit(0)
}

// ---------------------------------------------------------------- keyboard

function onKey(key: KeyEvent) {
  if (state.commentOpen) {
    if (key.name === "escape") closeCommentInput()
    return // input owns every other key while open
  }

  if (key.name === "q") {
    if (state.screen === "diff") showHome()
    else quit()
    return
  }

  if (state.screen === "home") {
    if (key.name === "j" || key.name === "down") {
      state.homeIdx = Math.min(homeRows.length - 1, state.homeIdx + 1)
      refreshHome()
    } else if (key.name === "k" || key.name === "up") {
      state.homeIdx = Math.max(0, state.homeIdx - 1)
      refreshHome()
    } else if (key.name === "tab") {
      const cur = (homeRows[state.homeIdx] as any).section
      const next = homeRows.findIndex((r: any) => r.section === (cur + 1) % 3)
      state.homeIdx = next >= 0 ? next : 0
      refreshHome()
    } else if (key.name === "return" || key.name === "enter") {
      const row = homeRows[state.homeIdx] as any
      if (row.kind === "file") showDiff(row.idx)
    }
    return
  }

  // diff screen
  if (key.name === "tab") {
    state.panel = state.panel === "files" ? "diff" : "files"
    refreshSidebar()
    refreshDiffCursor()
  } else if (key.name === "j" || key.name === "down") {
    if (state.panel === "files") {
      state.filesIdx = Math.min(FILES.length - 1, state.filesIdx + 1)
      selectFile(state.filesIdx)
    } else {
      state.cursor = Math.min(diffRows.length - 1, state.cursor + 1)
      refreshDiffCursor()
    }
  } else if (key.name === "k" || key.name === "up") {
    if (state.panel === "files") {
      state.filesIdx = Math.max(0, state.filesIdx - 1)
      selectFile(state.filesIdx)
    } else {
      state.cursor = Math.max(0, state.cursor - 1)
      refreshDiffCursor()
    }
  } else if (key.name === "return" || key.name === "enter") {
    if (state.panel === "files") {
      state.panel = "diff"
      refreshSidebar()
      refreshDiffCursor()
    }
  } else if (key.name === "c") {
    if (state.panel === "diff") openCommentInput()
  } else if (key.name === "a" || key.name === "x") {
    const row = diffRows[state.cursor]
    if (row) {
      verdicts[row.hunkIdx] = key.name === "a" ? "accepted" : "rejected"
      refreshHunkChips()
    }
  } else if (key.name === "y") {
    // copy current selection to system clipboard via OSC52
    if (state.selectionText) {
      const b64 = Buffer.from(state.selectionText).toString("base64")
      process.stdout.write(`\x1b]52;c;${b64}\x07`)
    }
  }
}

// ---------------------------------------------------------------- boot

let appColumn: BoxRenderable

async function main() {
  renderer = await createCliRenderer({
    exitOnCtrlC: true,
    targetFps: 30,
  })
  renderer.setBackgroundColor(C.bg)

  appColumn = new BoxRenderable(renderer, {
    id: "app",
    width: "100%",
    height: "100%",
    flexDirection: "column",
    backgroundColor: C.bg,
  })
  renderer.root.add(appColumn)

  buildHome()
  appColumn.add(homeScreen)

  statusBar = new BoxRenderable(renderer, { id: "status-bar", height: 1, width: "100%", backgroundColor: C.panel, flexShrink: 0 })
  statusText = new TextRenderable(renderer, { content: styled(" ") })
  statusBar.add(statusText)
  appColumn.add(statusBar)

  verdicts = AUTH_HUNKS.map((h) => h.verdict)
  updateStatusBar()

  renderer.keyInput.on("keypress", onKey)
  renderer.on("selection", (selection: any) => {
    if (selection) state.selectionText = selection.getSelectedText()
  })

  renderer.start()
}

main()
