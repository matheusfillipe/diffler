// diffler UI demo — Go + Bubble Tea v2 + Lip Gloss v2.
// Mock data only (no git/MCP/LSP); implements demos/SPEC.md.
package main

import (
	"fmt"
	"image/color"
	"os"
	"strings"
	"unicode"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
)

// ---------------------------------------------------------------------------
// Theme (GitHub dark, per spec)
// ---------------------------------------------------------------------------

var (
	cBg     = lipgloss.Color("#0d1117")
	cPanel  = lipgloss.Color("#161b22")
	cSelBg  = lipgloss.Color("#21262d")
	cFg     = lipgloss.Color("#e6edf3")
	cDim    = lipgloss.Color("#8b949e")
	cBlue   = lipgloss.Color("#58a6ff")
	cPurple = lipgloss.Color("#bc8cff")
	cDelBg  = lipgloss.Color("#3c1618")
	cAddBg  = lipgloss.Color("#12352a")
	cDelEm  = lipgloss.Color("#8b2c2f")
	cAddEm  = lipgloss.Color("#1f6f48")
	cBorder = lipgloss.Color("#30363d")
	cGreen  = lipgloss.Color("#3fb950")
	cRed    = lipgloss.Color("#f85149")
	cYellow = lipgloss.Color("#d29922")
	cChipOK = lipgloss.Color("#238636")

	// Python token colors (GitHub dark scheme)
	synKw  = lipgloss.Color("#ff7b72")
	synStr = lipgloss.Color("#a5d6ff")
	synFn  = lipgloss.Color("#d2a8ff")
	synLit = lipgloss.Color("#79c0ff")
)

const sidebarW = 30

// ---------------------------------------------------------------------------
// Mock data (embedded exactly per spec)
// ---------------------------------------------------------------------------

type lineKind int

const (
	ctxLine lineKind = iota
	delLine
	addLine
)

type diffLine struct {
	kind       lineKind
	oldN, newN int // 0 means blank column
	text       string
	emS, emE   int // intra-line emphasis rune range; emS<0 → none
}

type hunkData struct {
	header  string
	verdict string // "accepted" | "pending" | "rejected"
	lines   []diffLine
}

type comment struct {
	author string
	text   string
}

type fileEntry struct {
	status string // M / A
	name   string
	plus   int
	minus  int
}

var mockFiles = []fileEntry{
	{"M", "src/auth.py", 18, 4},
	{"M", "src/session.py", 6, 1},
	{"A", "tests/test_auth.py", 42, 0},
}

func mockHunks() []hunkData {
	h1 := hunkData{
		header:  "@@ -10,7 +10,9 @@ def validate_token(token):",
		verdict: "accepted",
		lines: []diffLine{
			{ctxLine, 10, 10, "def validate_token(token):", -1, -1},
			{ctxLine, 11, 11, "    claims = decode(token)", -1, -1},
			{delLine, 12, 0, "    if claims.expiry < now():", -1, -1},
			{addLine, 0, 12, "    if claims.expiry <= now() - LEEWAY:", -1, -1},
			{delLine, 13, 0, `        raise TokenError("expired")`, -1, -1},
			{addLine, 0, 13, `        raise TokenExpiredError("expired", claims.expiry)`, -1, -1},
			{ctxLine, 14, 14, "    return claims", -1, -1},
			{addLine, 0, 15, `    audit_log("token.validated", claims.sub)`, -1, -1},
			{ctxLine, 15, 16, "", -1, -1},
		},
	}
	h2 := hunkData{
		header:  "@@ -31,6 +33,7 @@ def refresh_session(session_id):",
		verdict: "pending",
		lines: []diffLine{
			{ctxLine, 31, 33, "def refresh_session(session_id):", -1, -1},
			{ctxLine, 32, 34, "    session = store.get(session_id)", -1, -1},
			{delLine, 33, 0, "    session.touch()", -1, -1},
			{addLine, 0, 35, "    session.touch(now())", -1, -1},
			{ctxLine, 34, 36, "    store.put(session)", -1, -1},
			{addLine, 0, 37, `    metrics.incr("session.refresh")`, -1, -1},
			{ctxLine, 35, 38, "    return session", -1, -1},
		},
	}
	hs := []hunkData{h1, h2}
	// Char-level diff on each old/new pair so changed runs get emphasis bg.
	for hi := range hs {
		ls := hs[hi].lines
		for i := 0; i < len(ls)-1; i++ {
			if ls[i].kind == delLine && ls[i+1].kind == addLine {
				as, ae, bs, be := intraDiff(ls[i].text, ls[i+1].text)
				ls[i].emS, ls[i].emE = as, ae
				ls[i+1].emS, ls[i+1].emE = bs, be
			}
		}
	}
	return hs
}

// intraDiff finds the changed middle of a pair via common prefix/suffix.
func intraDiff(a, b string) (as, ae, bs, be int) {
	ra, rb := []rune(a), []rune(b)
	p := 0
	for p < len(ra) && p < len(rb) && ra[p] == rb[p] {
		p++
	}
	s := 0
	for s < len(ra)-p && s < len(rb)-p && ra[len(ra)-1-s] == rb[len(rb)-1-s] {
		s++
	}
	return p, len(ra) - s, p, len(rb) - s
}

// ---------------------------------------------------------------------------
// Minimal hand-rolled Python token coloring
// ---------------------------------------------------------------------------

var pyKeywords = map[string]bool{
	"def": true, "if": true, "elif": true, "else": true, "return": true,
	"raise": true, "not": true, "and": true, "or": true, "in": true,
	"is": true, "for": true, "while": true, "import": true, "from": true,
	"class": true, "pass": true, "None": true, "True": true, "False": true,
}

// tokenFg returns a foreground color per rune of the line.
func tokenFg(text string) []color.Color {
	rs := []rune(text)
	fg := make([]color.Color, len(rs))
	for i := range fg {
		fg[i] = cFg
	}
	i := 0
	for i < len(rs) {
		r := rs[i]
		switch {
		case r == '"' || r == '\'':
			q := r
			j := i + 1
			for j < len(rs) && rs[j] != q {
				j++
			}
			if j < len(rs) {
				j++
			}
			for k := i; k < j; k++ {
				fg[k] = synStr
			}
			i = j
		case unicode.IsLetter(r) || r == '_':
			j := i
			for j < len(rs) && (unicode.IsLetter(rs[j]) || unicode.IsDigit(rs[j]) || rs[j] == '_') {
				j++
			}
			word := string(rs[i:j])
			var c color.Color = cFg
			switch {
			case pyKeywords[word]:
				c = synKw
			case j < len(rs) && rs[j] == '(':
				c = synFn
			case word == strings.ToUpper(word) && len(word) > 1:
				c = synLit
			}
			for k := i; k < j; k++ {
				fg[k] = c
			}
			i = j
		case unicode.IsDigit(r):
			j := i
			for j < len(rs) && (unicode.IsDigit(rs[j]) || rs[j] == '.') {
				j++
			}
			for k := i; k < j; k++ {
				fg[k] = synLit
			}
			i = j
		case strings.ContainsRune("<>=+-*/%!", r):
			fg[i] = synKw
			i++
		default:
			i++
		}
	}
	return fg
}

// renderCode emits a line with per-rune fg (syntax) and bg (diff + emphasis),
// grouping equal-style runs to keep the ANSI output small.
func renderCode(text string, base color.Color, emS, emE int, emBg color.Color) string {
	rs := []rune(text)
	if len(rs) == 0 {
		return ""
	}
	fg := tokenFg(text)
	bg := make([]color.Color, len(rs))
	for i := range bg {
		bg[i] = base
		if emS >= 0 && i >= emS && i < emE {
			bg[i] = emBg
		}
	}
	var b strings.Builder
	start := 0
	for i := 1; i <= len(rs); i++ {
		if i == len(rs) || fg[i] != fg[start] || bg[i] != bg[start] {
			b.WriteString(lipgloss.NewStyle().Foreground(fg[start]).Background(bg[start]).Render(string(rs[start:i])))
			start = i
		}
	}
	return b.String()
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

type screen int

const (
	scrHome screen = iota
	scrDiff
)

type renderedRow struct {
	str string
	sel int // selectable cursor index, -1 if not a diff line
}

type model struct {
	w, h    int
	screen  screen
	hunks   []hunkData
	files   []fileEntry
	fileSel int

	homeCursor int

	diffFocus int // 0 = files sidebar, 1 = diff panel
	cursor    int // selectable line index within diff
	offset    int

	comments map[[2]int][]comment // [hunkIdx, lineIdx] → comments

	inputActive bool
	inputText   string
	inputAt     [2]int
}

func newModel() model {
	m := model{
		hunks:    mockHunks(),
		files:    mockFiles,
		comments: map[[2]int][]comment{},
	}
	// Seeded review comment under the LEEWAY line (hunk 0, line index 3).
	m.comments[[2]int{0, 3}] = []comment{{
		author: "mattf",
		text:   "why LEEWAY here? clock skew between services? add a comment or link the incident.",
	}}
	return m
}

func (m model) Init() tea.Cmd { return nil }

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.w, m.h = msg.Width, msg.Height
		return m, nil
	case tea.KeyPressMsg:
		return m.updateKeys(msg)
	case tea.MouseClickMsg:
		return m.updateClick(msg.Mouse()), nil
	case tea.MouseWheelMsg:
		return m.updateWheel(msg.Mouse()), nil
	}
	return m, nil
}

func (m model) updateKeys(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	if msg.String() == "ctrl+c" {
		return m, tea.Quit
	}
	if m.inputActive {
		switch msg.String() {
		case "esc":
			m.inputActive = false
			m.inputText = ""
		case "enter":
			if strings.TrimSpace(m.inputText) != "" {
				m.comments[m.inputAt] = append(m.comments[m.inputAt], comment{author: "you", text: m.inputText})
			}
			m.inputActive = false
			m.inputText = ""
		case "backspace":
			if rs := []rune(m.inputText); len(rs) > 0 {
				m.inputText = string(rs[:len(rs)-1])
			}
		default:
			if msg.Text != "" {
				m.inputText += msg.Text
			}
		}
		return m, nil
	}

	if m.screen == scrHome {
		items := homeSelectableCount()
		switch msg.String() {
		case "q":
			return m, tea.Quit
		case "j", "down":
			if m.homeCursor < items-1 {
				m.homeCursor++
			}
		case "k", "up":
			if m.homeCursor > 0 {
				m.homeCursor--
			}
		case "tab":
			m.homeCursor = nextSectionStart(m.homeCursor)
		case "enter":
			if f, ok := homeChangeIndex(m.homeCursor); ok {
				m.openDiff(f)
				return m, nil
			}
		}
		return m, nil
	}

	// Diff screen
	switch msg.String() {
	case "q", "esc":
		m.screen = scrHome
		return m, nil
	case "tab":
		m.diffFocus = 1 - m.diffFocus
		return m, nil
	}
	if m.diffFocus == 0 { // sidebar
		switch msg.String() {
		case "j", "down":
			if m.fileSel < len(m.files)-1 {
				m.fileSel++
				m.cursor, m.offset = 0, 0
			}
		case "k", "up":
			if m.fileSel > 0 {
				m.fileSel--
				m.cursor, m.offset = 0, 0
			}
		case "enter":
			m.diffFocus = 1
		}
		return m, nil
	}
	rows, nSel := m.diffRows(false)
	switch msg.String() {
	case "j", "down":
		if m.cursor < nSel-1 {
			m.cursor++
		}
	case "k", "up":
		if m.cursor > 0 {
			m.cursor--
		}
	case "c":
		if nSel > 0 {
			if hk, ln, ok := m.cursorTarget(); ok {
				m.inputActive = true
				m.inputText = ""
				m.inputAt = [2]int{hk, ln}
			}
		}
	case "a":
		if hk, _, ok := m.cursorTarget(); ok {
			m.hunks[hk].verdict = "accepted"
		}
	case "x":
		if hk, _, ok := m.cursorTarget(); ok {
			m.hunks[hk].verdict = "rejected"
		}
	}
	m = m.ensureCursorVisible(rows)
	return m, nil
}

func (m *model) openDiff(fileIdx int) {
	m.screen = scrDiff
	m.fileSel = fileIdx
	m.diffFocus = 1
	m.cursor, m.offset = 0, 0
}

// cursorTarget maps the diff cursor to (hunkIdx, lineIdx).
func (m model) cursorTarget() (int, int, bool) {
	if m.fileSel != 0 {
		return 0, 0, false
	}
	sel := 0
	for hi, h := range m.hunks {
		for li := range h.lines {
			if sel == m.cursor {
				return hi, li, true
			}
			sel++
		}
	}
	return 0, 0, false
}

func (m model) updateClick(mo tea.Mouse) model {
	if mo.Button != tea.MouseLeft {
		return m
	}
	if m.screen == scrHome {
		if idx, ok := homeRowAt(mo.Y); ok {
			if idx == m.homeCursor {
				if f, fok := homeChangeIndex(idx); fok {
					m.openDiff(f)
					return m
				}
			}
			m.homeCursor = idx
		}
		return m
	}
	// Diff screen: header is row 0, content starts at row 1.
	contentY := mo.Y - 1
	if contentY < 0 || contentY >= m.contentH() {
		return m
	}
	if mo.X < sidebarW { // sidebar: click selects file
		fi := contentY - 2 // title + blank line above the file list
		if fi >= 0 && fi < len(m.files) {
			m.fileSel = fi
			m.cursor, m.offset = 0, 0
			m.diffFocus = 0
		}
		return m
	}
	m.diffFocus = 1
	rows, _ := m.diffRows(false)
	ri := m.offset + contentY
	if ri >= 0 && ri < len(rows) && rows[ri].sel >= 0 {
		m.cursor = rows[ri].sel
	}
	return m
}

func (m model) updateWheel(mo tea.Mouse) model {
	delta := 3
	if mo.Button == tea.MouseWheelUp {
		delta = -3
	} else if mo.Button != tea.MouseWheelDown {
		return m
	}
	if m.screen == scrHome {
		m.homeCursor = clamp(m.homeCursor+delta/3, 0, homeSelectableCount()-1)
		return m
	}
	rows, _ := m.diffRows(false)
	maxOff := max(0, len(rows)-m.contentH())
	m.offset = clamp(m.offset+delta, 0, maxOff)
	return m
}

func (m model) ensureCursorVisible(rows []renderedRow) model {
	cursorRow := -1
	for i, r := range rows {
		if r.sel == m.cursor {
			cursorRow = i
			break
		}
	}
	if cursorRow < 0 {
		return m
	}
	h := m.contentH()
	if cursorRow < m.offset {
		m.offset = cursorRow
	}
	if cursorRow >= m.offset+h {
		m.offset = cursorRow - h + 1
	}
	return m
}

func (m model) contentH() int { return max(1, m.h-3) } // header + footer hint + status bar

// ---------------------------------------------------------------------------
// Shared render helpers
// ---------------------------------------------------------------------------

func st(fg, bg color.Color) lipgloss.Style {
	return lipgloss.NewStyle().Foreground(fg).Background(bg)
}

// fill pads a styled string with bg-colored spaces to exactly w cells.
func fill(s string, w int, bg color.Color) string {
	gap := w - lipgloss.Width(s)
	if gap > 0 {
		s += st(cFg, bg).Render(strings.Repeat(" ", gap))
	}
	return s
}

func chip(label string, bg color.Color, fg color.Color) string {
	return lipgloss.NewStyle().Foreground(fg).Background(bg).Bold(true).Render(" " + label + " ")
}

func clamp(v, lo, hi int) int {
	if hi < lo {
		return lo
	}
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

// ---------------------------------------------------------------------------
// Home screen (magit-style status)
// ---------------------------------------------------------------------------

// Selectable home rows: 2 workspaces, then 3 changes, then 2 commits.
func homeSelectableCount() int { return 7 }

func nextSectionStart(cur int) int {
	switch {
	case cur < 2:
		return 2
	case cur < 5:
		return 5
	default:
		return 0
	}
}

func homeChangeIndex(cur int) (int, bool) {
	if cur >= 2 && cur < 5 {
		return cur - 2, true
	}
	return 0, false
}

// Home layout rows (y → selectable index). Layout is static:
// 0 header, 1 separator, 2 blank, 3 "Workspaces", 4-5 workspaces,
// 6 blank, 7 "Changes", 8-10 changes, 11 blank, 12 "Recent commits", 13-14 commits.
var homeRowY = map[int]int{4: 0, 5: 1, 8: 2, 9: 3, 10: 4, 13: 5, 14: 6}

func homeRowAt(y int) (int, bool) {
	idx, ok := homeRowY[y]
	return idx, ok
}

func (m model) viewHome() string {
	w := m.w
	var rows []string
	add := func(s string) { rows = append(rows, fill(s, w, cBg)) }

	title := st(cBlue, cBg).Bold(true).Render("  diffler") +
		st(cDim, cBg).Render(" — ~/projects/acme")
	branch := st(cPurple, cBg).Bold(true).Render("main ") + st(cBlue, cBg).Render("⇡2") + st(cDim, cBg).Render("  ")
	gap := w - lipgloss.Width(title) - lipgloss.Width(branch)
	add(title + st(cFg, cBg).Render(strings.Repeat(" ", max(0, gap))) + branch)
	add(st(cBorder, cBg).Render("  " + strings.Repeat("─", max(0, w-4))))
	add("")

	section := func(s string) { add(st(cBlue, cBg).Bold(true).Render("  " + s)) }
	selRow := func(idx int, content string) {
		bg := cBg
		prefix := "    "
		if idx == m.homeCursor {
			bg = cSelBg
			prefix = "  ▌ "
		}
		line := st(cPurple, bg).Render(prefix) + reBg(content, bg)
		rows = append(rows, fill(line, w, bg))
	}

	section("Workspaces (2)")
	selRow(0, st(cGreen, cBg).Render("● ")+st(cFg, cBg).Bold(true).Render("main")+
		st(cDim, cBg).Render("            ~/projects/acme              ")+st(cFg, cBg).Render("3 files changed"))
	selRow(1, st(cDim, cBg).Render("  agent/fix-auth")+
		st(cDim, cBg).Render("  ~/projects/acme-fix-auth  ")+st(cFg, cBg).Render("2 files changed")+
		st(cFg, cBg).Render("  ")+chip("claude: running", cSelBg, cPurple))
	add("")

	section("Changes (3)")
	for i, f := range m.files {
		statusColor := cYellow
		if f.status == "A" {
			statusColor = cGreen
		}
		name := f.name + strings.Repeat(" ", max(0, 20-len(f.name)))
		selRow(2+i,
			st(statusColor, cBg).Bold(true).Render(f.status+" ")+
				st(cFg, cBg).Render(name)+
				st(cGreen, cBg).Render(fmt.Sprintf("+%-3d", f.plus))+
				st(cRed, cBg).Render(fmt.Sprintf("−%d", f.minus)))
	}
	add("")

	section("Recent commits")
	selRow(5, st(cBlue, cBg).Render("a1b2c3d ")+st(cFg, cBg).Render("fix: token expiry check off-by-one"))
	selRow(6, st(cBlue, cBg).Render("d4e5f6a ")+st(cFg, cBg).Render("feat: session refresh endpoint"))

	for len(rows) < m.h-2 {
		add("")
	}
	rows = rows[:max(0, m.h-2)]
	rows = append(rows, m.hintBar([][2]string{{"j/k", "move"}, {"Tab", "section"}, {"Enter", "open"}, {"q", "quit"}}))
	rows = append(rows, m.statusBar("NORMAL", cBlue, "~/projects/acme", "main ⇡2"))
	return strings.Join(rows, "\n")
}

// reBg rewrites background codes so selected-row content sits on the row bg.
// Cheap trick for the home rows: content was rendered on cBg; on the cursor
// row we re-render by swapping the bg SGR sequence.
func reBg(s string, bg color.Color) string {
	if bg == cBg {
		return s
	}
	from := bgSeq(cBg)
	to := bgSeq(bg)
	return strings.ReplaceAll(s, from, to)
}

func bgSeq(c color.Color) string {
	r, g, b, _ := c.RGBA()
	return fmt.Sprintf("48;2;%d;%d;%d", r>>8, g>>8, b>>8)
}

// ---------------------------------------------------------------------------
// Diff screen
// ---------------------------------------------------------------------------

func (m model) diffPanelW() int { return max(20, m.w-sidebarW-1) }

// diffRows flattens the selected file's diff into display rows.
// Returns rows plus the number of selectable (cursor-addressable) lines.
func (m model) diffRows(styled bool) ([]renderedRow, int) {
	w := m.diffPanelW()
	var rows []renderedRow
	if m.fileSel != 0 {
		for _, s := range []string{
			"",
			"  (mock demo)",
			"",
			"  Only src/auth.py carries diff data in this demo.",
			"  Press Tab and pick src/auth.py to see the money screen.",
		} {
			rows = append(rows, renderedRow{str: fill(st(cDim, cBg).Render(s), w, cBg), sel: -1})
		}
		return rows, 0
	}
	sel := 0
	for hi, h := range m.hunks {
		if styled {
			rows = append(rows, renderedRow{str: m.renderHunkHeader(h, w), sel: -1})
		} else {
			rows = append(rows, renderedRow{sel: -1})
		}
		for li, ln := range h.lines {
			cursorHere := styled && m.diffFocus == 1 && sel == m.cursor
			if styled {
				rows = append(rows, renderedRow{str: m.renderDiffLine(ln, w, cursorHere), sel: sel})
			} else {
				rows = append(rows, renderedRow{sel: sel})
			}
			for _, cm := range m.comments[[2]int{hi, li}] {
				rows = appendBlock(rows, m.renderCommentBox(cm, w), styled)
			}
			if m.inputActive && m.inputAt == [2]int{hi, li} {
				rows = appendBlock(rows, m.renderInputBox(w), styled)
			}
			sel++
		}
		if styled {
			rows = append(rows, renderedRow{str: fill("", w, cBg), sel: -1})
		} else {
			rows = append(rows, renderedRow{sel: -1})
		}
	}
	return rows, sel
}

func appendBlock(rows []renderedRow, block []string, styled bool) []renderedRow {
	for _, l := range block {
		r := renderedRow{sel: -1}
		if styled {
			r.str = l
		}
		rows = append(rows, r)
	}
	return rows
}

func (m model) renderHunkHeader(h hunkData, w int) string {
	left := st(cDim, cPanel).Render(" " + h.header)
	var vchip string
	switch h.verdict {
	case "accepted":
		vchip = chip("✓ accepted", cChipOK, lipgloss.Color("#ffffff"))
	case "rejected":
		vchip = chip("✗ rejected", cDelEm, lipgloss.Color("#ffffff"))
	default:
		vchip = chip("pending", cSelBg, cDim)
	}
	gap := w - lipgloss.Width(left) - lipgloss.Width(vchip) - 1
	return left + st(cDim, cPanel).Render(strings.Repeat(" ", max(0, gap))) + vchip + st(cDim, cPanel).Render(" ")
}

func (m model) renderDiffLine(ln diffLine, w int, cursor bool) string {
	base := cBg
	var marker string
	var markerFg color.Color = cDim
	var emBg color.Color = cBg
	switch ln.kind {
	case delLine:
		base, emBg, marker, markerFg = cDelBg, cDelEm, "-", cRed
	case addLine:
		base, emBg, marker, markerFg = cAddBg, cAddEm, "+", cGreen
	default:
		marker = " "
		if cursor {
			base = cSelBg
		}
	}
	old, new := "    ", "    "
	if ln.oldN > 0 {
		old = fmt.Sprintf("%4d", ln.oldN)
	}
	if ln.newN > 0 {
		new = fmt.Sprintf("%4d", ln.newN)
	}
	cursorMark := " "
	if cursor {
		cursorMark = "▌"
	}
	gutter := st(cPurple, base).Render(cursorMark) +
		st(cDim, base).Render(old+" "+new+" ") +
		st(markerFg, base).Bold(ln.kind != ctxLine).Render(marker+" ")
	code := renderCode(ln.text, base, ln.emS, ln.emE, emBg)
	return fill(gutter+code, w, base)
}

func (m model) renderCommentBox(cm comment, w int) []string {
	boxW := min(w-14, 72)
	author := chip(cm.author, cPurple, cBg)
	body := lipgloss.NewStyle().Foreground(cFg).Background(cPanel).Width(boxW - 4).Render(cm.text)
	inner := author + "\n" + body
	box := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(cBorder).
		BorderBackground(cBg).
		Background(cPanel).
		Padding(0, 1).
		Render(inner)
	var out []string
	for _, l := range strings.Split(box, "\n") {
		out = append(out, fill(st(cFg, cBg).Render("           ")+l, w, cBg))
	}
	return out
}

func (m model) renderInputBox(w int) []string {
	boxW := min(w-14, 72)
	prompt := st(cFg, cPanel).Render(m.inputText) + st(cBg, cBlue).Render("█")
	hint := st(cDim, cPanel).Render("enter save · esc cancel")
	inner := lipgloss.NewStyle().Background(cPanel).Width(boxW-4).Render(prompt) + "\n" + hint
	box := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(cBlue).
		BorderBackground(cBg).
		Background(cPanel).
		Padding(0, 1).
		Render(inner)
	var out []string
	for _, l := range strings.Split(box, "\n") {
		out = append(out, fill(st(cFg, cBg).Render("           ")+l, w, cBg))
	}
	return out
}

func (m model) renderSidebar(h int) []string {
	w := sidebarW
	rows := make([]string, 0, h)
	titleFg := cDim
	if m.diffFocus == 0 {
		titleFg = cBlue
	}
	rows = append(rows, fill(st(titleFg, cPanel).Bold(true).Render("  Changes (3)"), w, cPanel))
	rows = append(rows, fill("", w, cPanel))
	for i, f := range m.files {
		bg := cPanel
		prefix := "  "
		if i == m.fileSel {
			bg = cSelBg
			prefix = "▌ "
		}
		statusColor := cYellow
		if f.status == "A" {
			statusColor = cGreen
		}
		row := st(cPurple, bg).Render(prefix) +
			st(statusColor, bg).Bold(true).Render(f.status+" ") +
			st(cFg, bg).Render(f.name)
		rows = append(rows, fill(row, w, bg))
	}
	rows = append(rows, fill("", w, cPanel))
	pm := st(cGreen, cPanel).Render(fmt.Sprintf("  +%d ", m.files[m.fileSel].plus)) +
		st(cRed, cPanel).Render(fmt.Sprintf("−%d", m.files[m.fileSel].minus))
	rows = append(rows, fill(pm, w, cPanel))
	for len(rows) < h {
		rows = append(rows, fill("", w, cPanel))
	}
	return rows[:h]
}

func (m model) viewDiff() string {
	w, h := m.w, m.contentH()
	var out []string

	// Header
	f := m.files[m.fileSel]
	head := st(cPurple, cBg).Bold(true).Render("  "+f.name) +
		st(cGreen, cBg).Render(fmt.Sprintf("  +%d ", f.plus)) +
		st(cRed, cBg).Render(fmt.Sprintf("−%d", f.minus))
	out = append(out, fill(head, w, cBg))

	side := m.renderSidebar(h)
	rows, _ := m.diffRows(true)
	maxOff := max(0, len(rows)-h)
	off := clamp(m.offset, 0, maxOff)
	border := st(cBorder, cBg).Render("│")
	for y := 0; y < h; y++ {
		var diffPart string
		if off+y < len(rows) {
			diffPart = rows[off+y].str
		} else {
			diffPart = fill("", m.diffPanelW(), cBg)
		}
		out = append(out, side[y]+border+diffPart)
	}

	out = append(out, m.hintBar([][2]string{
		{"j/k", "scroll"}, {"c", "comment"}, {"a", "accept hunk"},
		{"x", "reject hunk"}, {"Tab", "files"}, {"q", "back"},
	}))
	mode, modeColor := "REVIEW", cPurple
	if m.inputActive {
		mode, modeColor = "COMMENT", cGreen
	}
	pos := fmt.Sprintf("%d%%", int(100*float64(min(off+h, len(rows)))/float64(max(1, len(rows)))))
	out = append(out, m.statusBar(mode, modeColor, f.name, "main ⇡2  "+pos))
	return strings.Join(out, "\n")
}

// ---------------------------------------------------------------------------
// Bars
// ---------------------------------------------------------------------------

func (m model) hintBar(hints [][2]string) string {
	var b strings.Builder
	b.WriteString(st(cDim, cPanel).Render(" "))
	for i, h := range hints {
		if i > 0 {
			b.WriteString(st(cBorder, cPanel).Render("  ·  "))
		}
		b.WriteString(st(cBlue, cPanel).Bold(true).Render(h[0]))
		b.WriteString(st(cDim, cPanel).Render(" " + h[1]))
	}
	return fill(b.String(), m.w, cPanel)
}

func (m model) statusBar(mode string, modeColor color.Color, path, right string) string {
	left := chip(mode, modeColor, cBg) +
		st(modeColor, cPanel).Render("") +
		st(cFg, cPanel).Render(" "+path)
	r := st(cDim, cPanel).Render(right + " ")
	gap := m.w - lipgloss.Width(left) - lipgloss.Width(r)
	return left + st(cFg, cPanel).Render(strings.Repeat(" ", max(0, gap))) + r
}

// ---------------------------------------------------------------------------
// View root
// ---------------------------------------------------------------------------

func (m model) View() tea.View {
	var content string
	if m.w == 0 || m.h == 0 {
		content = "loading…"
	} else if m.screen == scrHome {
		content = m.viewHome()
	} else {
		content = m.viewDiff()
	}
	v := tea.NewView(content)
	v.AltScreen = true
	v.MouseMode = tea.MouseModeCellMotion
	v.BackgroundColor = cBg
	v.WindowTitle = "diffler"
	return v
}

func main() {
	if _, err := tea.NewProgram(newModel()).Run(); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}
