// diffler UI demo — ratatui + crossterm, mock data only (no git/MCP/LSP).
// Two screens: Home (magit-style status) and Diff view, per demos/SPEC.md.

use std::collections::HashMap;
use std::io;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use similar::{DiffTag, TextDiff};

// ---------------------------------------------------------------- theme

const BG: Color = Color::Rgb(0x0d, 0x11, 0x17);
const PANEL: Color = Color::Rgb(0x16, 0x1b, 0x22);
const SEL: Color = Color::Rgb(0x21, 0x26, 0x2d);
const FG: Color = Color::Rgb(0xe6, 0xed, 0xf3);
const DIM: Color = Color::Rgb(0x8b, 0x94, 0x9e);
const BLUE: Color = Color::Rgb(0x58, 0xa6, 0xff);
const PURPLE: Color = Color::Rgb(0xbc, 0x8c, 0xff);
const BORDER: Color = Color::Rgb(0x30, 0x36, 0x3d);
const DEL_BG: Color = Color::Rgb(0x3c, 0x16, 0x18);
const ADD_BG: Color = Color::Rgb(0x12, 0x35, 0x2a);
const DEL_EM: Color = Color::Rgb(0x8b, 0x2c, 0x2f);
const ADD_EM: Color = Color::Rgb(0x1f, 0x6f, 0x48);
const GREEN: Color = Color::Rgb(0x3f, 0xb9, 0x50);
const RED: Color = Color::Rgb(0xf8, 0x51, 0x49);
// GitHub-dark-ish syntax palette
const SYN_KW: Color = Color::Rgb(0xff, 0x7b, 0x72);
const SYN_FN: Color = Color::Rgb(0xd2, 0xa8, 0xff);
const SYN_STR: Color = Color::Rgb(0xa5, 0xd6, 0xff);
const SYN_CONST: Color = Color::Rgb(0x79, 0xc0, 0xff);

fn s(fg: Color, bg: Color) -> Style {
    Style::new().fg(fg).bg(bg)
}

// ---------------------------------------------------------------- mock data

const FILES: [(char, &str, &str, &str); 3] = [
    ('M', "src/auth.py", "+18", "−4"),
    ('M', "src/session.py", "+6", "−1"),
    ('A', "tests/test_auth.py", "+42", "−0"),
];

const HUNK_HEADERS: [&str; 2] = [
    "@@ -10,7 +10,9 @@ def validate_token(token):",
    "@@ -31,6 +33,7 @@ def refresh_session(session_id):",
];

const MOCK_COMMENT_AT: usize = 3; // under the `<= now() - LEEWAY` line
const MOCK_COMMENT: &str =
    "why LEEWAY here? clock skew between services? add a comment or link the incident.";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ctx,
    Del,
    Add,
}

#[derive(Clone, Copy)]
struct DLine {
    kind: Kind,
    old: Option<u16>,
    new: Option<u16>,
    text: &'static str,
    pair: Option<usize>, // for Del lines: index of the matching Add line
    hunk: usize,
}

fn dl(
    kind: Kind,
    old: Option<u16>,
    new: Option<u16>,
    text: &'static str,
    pair: Option<usize>,
    hunk: usize,
) -> DLine {
    DLine { kind, old, new, text, pair, hunk }
}

fn auth_lines() -> Vec<DLine> {
    use Kind::*;
    vec![
        dl(Ctx, Some(10), Some(10), "def validate_token(token):", None, 0),
        dl(Ctx, Some(11), Some(11), "    claims = decode(token)", None, 0),
        dl(Del, Some(12), None, "    if claims.expiry < now():", Some(3), 0),
        dl(Add, None, Some(12), "    if claims.expiry <= now() - LEEWAY:", None, 0),
        dl(Del, Some(13), None, "        raise TokenError(\"expired\")", Some(5), 0),
        dl(Add, None, Some(13), "        raise TokenExpiredError(\"expired\", claims.expiry)", None, 0),
        dl(Ctx, Some(14), Some(14), "    return claims", None, 0),
        dl(Add, None, Some(15), "    audit_log(\"token.validated\", claims.sub)", None, 0),
        dl(Ctx, Some(15), Some(16), "", None, 0),
        dl(Ctx, Some(31), Some(33), "def refresh_session(session_id):", None, 1),
        dl(Ctx, Some(32), Some(34), "    session = store.get(session_id)", None, 1),
        dl(Del, Some(33), None, "    session.touch()", Some(12), 1),
        dl(Add, None, Some(35), "    session.touch(now())", None, 1),
        dl(Ctx, Some(34), Some(36), "    store.put(session)", None, 1),
        dl(Add, None, Some(37), "    metrics.incr(\"session.refresh\")", None, 1),
        dl(Ctx, Some(35), Some(38), "    return session", None, 1),
    ]
}

// ---------------------------------------------------------------- intra-line diff

fn char_marks(old: &str, new: &str) -> (Vec<bool>, Vec<bool>) {
    let mut om = vec![false; old.chars().count()];
    let mut nm = vec![false; new.chars().count()];
    let diff = TextDiff::from_chars(old, new);
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {}
            DiffTag::Delete => om[op.old_range()].fill(true),
            DiffTag::Insert => nm[op.new_range()].fill(true),
            DiffTag::Replace => {
                om[op.old_range()].fill(true);
                nm[op.new_range()].fill(true);
            }
        }
    }
    (om, nm)
}

// ---------------------------------------------------------------- python token coloring

const PY_KEYWORDS: [&str; 23] = [
    "def", "if", "elif", "else", "return", "raise", "import", "from", "class", "not", "and",
    "or", "in", "is", "for", "while", "pass", "with", "as", "try", "except", "lambda", "del",
];

fn py_fg(text: &str) -> Vec<Color> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![FG; chars.len()];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += 1;
            }
            let end = (j + 1).min(chars.len());
            out[i..end].fill(SYN_STR);
            i = end;
        } else if c == '#' {
            out[i..].fill(DIM);
            break;
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let color = if PY_KEYWORDS.contains(&word.as_str())
                || word == "None"
                || word == "True"
                || word == "False"
            {
                SYN_KW
            } else if j < chars.len() && chars[j] == '(' {
                SYN_FN
            } else if word.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_') {
                SYN_CONST
            } else {
                FG
            };
            out[i..j].fill(color);
            i = j;
        } else if c.is_ascii_digit() {
            out[i] = SYN_CONST;
            i += 1;
        } else if "<>=+-*/!".contains(c) {
            out[i] = SYN_KW;
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------- small helpers

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| sp.content.chars().count()).sum()
}

fn pad_to(mut spans: Vec<Span<'static>>, width: usize, bg: Color) -> Line<'static> {
    let w = spans_width(&spans);
    if width > w {
        spans.push(Span::styled(" ".repeat(width - w), s(FG, bg)));
    }
    Line::from(spans)
}

fn pad_between(
    mut left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
    bg: Color,
) -> Line<'static> {
    let used = spans_width(&left) + spans_width(&right);
    if width > used {
        left.push(Span::styled(" ".repeat(width - used), s(FG, bg)));
    }
    left.extend(right);
    Line::from(left)
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------- app state

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Diff,
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Sidebar,
    Diff,
}

#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    Accepted,
    Pending,
    Rejected,
}

#[derive(Clone, Copy, PartialEq)]
enum Meta {
    Other,
    Hunk(usize),
    Code(usize),
}

struct App {
    screen: Screen,
    quit: bool,
    // home
    home_sel: usize,
    home_hits: Vec<(u16, usize)>, // (screen row, file idx)
    // diff
    file_sel: usize,
    focus: Focus,
    cursor: usize,
    scroll: usize,
    ensure_visible: bool,
    verdicts: [Verdict; 2],
    input: Option<String>,
    user_comments: Vec<(usize, String)>,
    lines: Vec<DLine>,
    emph: HashMap<usize, Vec<bool>>,
    // hit-testing, refreshed each draw
    side_area: Rect,
    side_hits: Vec<(u16, usize)>,
    diff_inner: Rect,
    diff_metas: Vec<Meta>,
    rows_len: usize,
}

impl App {
    fn new() -> Self {
        let lines = auth_lines();
        let mut emph: HashMap<usize, Vec<bool>> = HashMap::new();
        for (i, l) in lines.iter().enumerate() {
            if l.kind == Kind::Del {
                if let Some(j) = l.pair {
                    let (om, nm) = char_marks(l.text, lines[j].text);
                    emph.insert(i, om);
                    emph.insert(j, nm);
                }
            }
        }
        Self {
            screen: Screen::Home,
            quit: false,
            home_sel: 0,
            home_hits: Vec::new(),
            file_sel: 0,
            focus: Focus::Diff,
            cursor: 0,
            scroll: 0,
            ensure_visible: false,
            verdicts: [Verdict::Accepted, Verdict::Pending],
            input: None,
            user_comments: Vec::new(),
            lines,
            emph,
            side_area: Rect::default(),
            side_hits: Vec::new(),
            diff_inner: Rect::default(),
            diff_metas: Vec::new(),
            rows_len: 0,
        }
    }

    fn run(&mut self, term: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            term.draw(|f| self.draw(f))?;
            if self.quit {
                return Ok(());
            }
            match event::read()? {
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    self.on_key(k)
                }
                Event::Mouse(m) => self.on_mouse(m),
                _ => {}
            }
        }
    }

    fn code_count(&self) -> usize {
        if self.file_sel == 0 {
            self.lines.len()
        } else {
            0
        }
    }

    fn open_diff(&mut self, idx: usize) {
        self.file_sel = idx;
        self.screen = Screen::Diff;
        self.focus = Focus::Diff;
        self.cursor = 0;
        self.scroll = 0;
        self.input = None;
    }

    fn select_file(&mut self, idx: usize) {
        if self.file_sel != idx {
            self.file_sel = idx;
            self.cursor = 0;
            self.scroll = 0;
            self.input = None;
        }
    }

    // ------------------------------------------------------------ input

    fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(buf) = self.input.as_mut() {
            match k.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => {
                    let text = self.input.take().unwrap();
                    if !text.trim().is_empty() {
                        self.user_comments.push((self.cursor, text));
                        self.ensure_visible = true;
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
        match self.screen {
            Screen::Home => match k.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('j') | KeyCode::Down => {
                    self.home_sel = (self.home_sel + 1).min(FILES.len() - 1)
                }
                KeyCode::Char('k') | KeyCode::Up => self.home_sel = self.home_sel.saturating_sub(1),
                KeyCode::Enter => self.open_diff(self.home_sel),
                _ => {}
            },
            Screen::Diff => match k.code {
                KeyCode::Char('q') => self.screen = Screen::Home,
                KeyCode::Tab => {
                    self.focus = if self.focus == Focus::Diff {
                        Focus::Sidebar
                    } else {
                        Focus::Diff
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => match self.focus {
                    Focus::Sidebar => {
                        let i = (self.file_sel + 1).min(FILES.len() - 1);
                        self.select_file(i);
                    }
                    Focus::Diff => {
                        if self.code_count() > 0 {
                            self.cursor = (self.cursor + 1).min(self.code_count() - 1);
                            self.ensure_visible = true;
                        }
                    }
                },
                KeyCode::Char('k') | KeyCode::Up => match self.focus {
                    Focus::Sidebar => {
                        let i = self.file_sel.saturating_sub(1);
                        self.select_file(i);
                    }
                    Focus::Diff => {
                        self.cursor = self.cursor.saturating_sub(1);
                        self.ensure_visible = true;
                    }
                },
                KeyCode::Enter => {
                    if self.focus == Focus::Sidebar {
                        self.focus = Focus::Diff;
                    }
                }
                KeyCode::Char('c') => {
                    if self.focus == Focus::Diff && self.code_count() > 0 {
                        self.input = Some(String::new());
                        self.ensure_visible = true;
                    }
                }
                KeyCode::Char('a') => self.set_verdict(Verdict::Accepted),
                KeyCode::Char('x') => self.set_verdict(Verdict::Rejected),
                _ => {}
            },
        }
    }

    fn set_verdict(&mut self, v: Verdict) {
        if self.focus == Focus::Diff && self.code_count() > 0 {
            let h = self.lines[self.cursor].hunk;
            self.verdicts[h] = v;
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position { x: m.column, y: m.row };
        match m.kind {
            MouseEventKind::ScrollDown => match self.screen {
                Screen::Home => self.home_sel = (self.home_sel + 1).min(FILES.len() - 1),
                Screen::Diff => {
                    let h = self.diff_inner.height as usize;
                    let max = self.rows_len.saturating_sub(h);
                    self.scroll = (self.scroll + 3).min(max);
                }
            },
            MouseEventKind::ScrollUp => match self.screen {
                Screen::Home => self.home_sel = self.home_sel.saturating_sub(1),
                Screen::Diff => self.scroll = self.scroll.saturating_sub(3),
            },
            MouseEventKind::Down(MouseButton::Left) => match self.screen {
                Screen::Home => {
                    if let Some(&(_, i)) =
                        self.home_hits.iter().find(|&&(y, _)| y == m.row)
                    {
                        if self.home_sel == i {
                            self.open_diff(i);
                        } else {
                            self.home_sel = i;
                        }
                    }
                }
                Screen::Diff => {
                    if self.side_area.contains(pos) {
                        if let Some(&(_, i)) =
                            self.side_hits.iter().find(|&&(y, _)| y == m.row)
                        {
                            self.focus = Focus::Sidebar;
                            self.select_file(i);
                        }
                    } else if self.diff_inner.contains(pos) {
                        let idx = self.scroll + (m.row - self.diff_inner.y) as usize;
                        if let Some(Meta::Code(i)) = self.diff_metas.get(idx) {
                            self.focus = Focus::Diff;
                            self.cursor = *i;
                        }
                    }
                }
            },
            _ => {}
        }
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        match self.screen {
            Screen::Home => self.draw_home(f),
            Screen::Diff => self.draw_diff(f),
        }
    }

    fn status_bar(&self, mode: &str, mode_bg: Color, hints: &str, width: usize) -> Line<'static> {
        let left = vec![
            Span::styled(
                format!(" {mode} "),
                s(BG, mode_bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled("\u{e0b0}", s(mode_bg, SEL)),
            Span::styled(" ~/projects/acme ", s(FG, SEL)),
            Span::styled("\u{e0b0}", s(SEL, PANEL)),
        ];
        let right = vec![Span::styled(format!(" {hints} "), s(DIM, PANEL))];
        pad_between(left, right, width, PANEL)
    }

    fn draw_home(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Block::new().style(s(FG, BG)), area);
        if area.height < 3 || area.width < 10 {
            return;
        }
        let body = Rect { height: area.height - 1, ..area };
        let bar = Rect { y: area.y + area.height - 1, height: 1, ..area };
        let w = body.width as usize;
        self.home_hits.clear();

        let mut lines: Vec<Line> = Vec::new();
        lines.push(pad_between(
            vec![
                Span::styled("  diffler", s(BLUE, BG).add_modifier(Modifier::BOLD)),
                Span::styled(" — ~/projects/acme", s(DIM, BG)),
            ],
            vec![
                Span::styled("main ", s(BLUE, BG)),
                Span::styled("⇡2  ", s(PURPLE, BG)),
            ],
            w,
            BG,
        ));
        lines.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(w.saturating_sub(4))),
            s(BORDER, BG),
        )));
        lines.push(Line::default());

        // Workspaces
        lines.push(Line::from(Span::styled(
            "  Workspaces (2)",
            s(BLUE, BG).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            Span::styled("    ● ", s(GREEN, BG)),
            Span::styled(format!("{:<15}", "main"), s(FG, BG).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<27}", "~/projects/acme"), s(DIM, BG)),
            Span::styled("3 files changed", s(DIM, BG)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("      ", s(FG, BG)),
            Span::styled(format!("{:<15}", "agent/fix-auth"), s(PURPLE, BG)),
            Span::styled(format!("{:<27}", "~/projects/acme-fix-auth"), s(DIM, BG)),
            Span::styled("2 files changed  ", s(DIM, BG)),
            Span::styled("[claude: running]", s(PURPLE, BG)),
        ]));
        lines.push(Line::default());

        // Changes (selectable)
        lines.push(Line::from(Span::styled(
            "  Changes (3)",
            s(BLUE, BG).add_modifier(Modifier::BOLD),
        )));
        for (i, (st, name, add, del)) in FILES.iter().enumerate() {
            let bg = if i == self.home_sel { SEL } else { BG };
            let st_fg = if *st == 'A' { GREEN } else { BLUE };
            let mut name_style = s(FG, bg);
            if i == self.home_sel {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            let row = pad_to(
                vec![
                    Span::styled("    ", s(FG, bg)),
                    Span::styled(format!("{st} "), s(st_fg, bg)),
                    Span::styled(format!("{name:<21}"), name_style),
                    Span::styled(format!("{add:<4}"), s(GREEN, bg)),
                    Span::styled((*del).to_string(), s(RED, bg)),
                ],
                w,
                bg,
            );
            self.home_hits.push((body.y + lines.len() as u16, i));
            lines.push(row);
        }
        lines.push(Line::default());

        // Recent commits
        lines.push(Line::from(Span::styled(
            "  Recent commits",
            s(BLUE, BG).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            Span::styled("    a1b2c3d ", s(BLUE, BG)),
            Span::styled("fix: token expiry check off-by-one", s(FG, BG)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    d4e5f6a ", s(BLUE, BG)),
            Span::styled("feat: session refresh endpoint", s(FG, BG)),
        ]));

        f.render_widget(Paragraph::new(lines).style(s(FG, BG)), body);
        f.render_widget(
            Paragraph::new(self.status_bar(
                "NORMAL",
                BLUE,
                "j/k move  Enter open  q quit",
                area.width as usize,
            )),
            bar,
        );
    }

    fn draw_diff(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Block::new().style(s(FG, BG)), area);
        if area.height < 5 || area.width < 40 {
            return;
        }
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        let cols =
            Layout::horizontal([Constraint::Length(30), Constraint::Min(10)]).split(rows[0]);

        // ---- sidebar
        let sb_focus = self.focus == Focus::Sidebar;
        let sb_border = if sb_focus { BLUE } else { BORDER };
        let sb_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s(sb_border, PANEL))
            .style(s(FG, PANEL))
            .title(Line::from(Span::styled(
                " Files ",
                s(if sb_focus { BLUE } else { DIM }, PANEL).add_modifier(Modifier::BOLD),
            )));
        let sb_inner = sb_block.inner(cols[0]);
        f.render_widget(sb_block, cols[0]);
        self.side_area = sb_inner;
        self.side_hits.clear();
        let mut sb_lines: Vec<Line> = Vec::new();
        for (i, (st, name, add, del)) in FILES.iter().enumerate() {
            let bg = if i == self.file_sel { SEL } else { PANEL };
            let st_fg = if *st == 'A' { GREEN } else { BLUE };
            let mut name_style = s(FG, bg);
            if i == self.file_sel {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            let row = pad_between(
                vec![
                    Span::styled(format!(" {st} "), s(st_fg, bg)),
                    Span::styled((*name).to_string(), name_style),
                ],
                vec![
                    Span::styled(format!(" {add} "), s(GREEN, bg)),
                    Span::styled(format!("{del} "), s(RED, bg)),
                ],
                sb_inner.width as usize,
                bg,
            );
            self.side_hits.push((sb_inner.y + i as u16, i));
            sb_lines.push(row);
        }
        f.render_widget(Paragraph::new(sb_lines), sb_inner);

        // ---- diff panel
        let df_focus = self.focus == Focus::Diff;
        let df_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s(if df_focus { BLUE } else { BORDER }, BG))
            .style(s(FG, BG))
            .title(Line::from(Span::styled(
                format!(" {} ", FILES[self.file_sel].1),
                s(if df_focus { BLUE } else { DIM }, BG).add_modifier(Modifier::BOLD),
            )));
        let inner = df_block.inner(cols[1]);
        f.render_widget(df_block, cols[1]);

        let (built, metas) = self.build_diff_rows(inner.width as usize);
        let total = built.len();
        let h = inner.height as usize;
        let max_scroll = total.saturating_sub(h);
        if self.ensure_visible {
            if let Some(cv) = metas.iter().position(|m| *m == Meta::Code(self.cursor)) {
                let extra = if self.input.is_some() { 3 } else { 0 };
                if cv < self.scroll {
                    self.scroll = cv;
                }
                let bottom = (cv + extra).min(total.saturating_sub(1));
                if h > 0 && bottom >= self.scroll + h {
                    self.scroll = bottom + 1 - h;
                }
            }
            self.ensure_visible = false;
        }
        self.scroll = self.scroll.min(max_scroll);
        let visible: Vec<Line> = built.into_iter().skip(self.scroll).take(h).collect();
        f.render_widget(Paragraph::new(visible), inner);
        self.diff_inner = inner;
        self.diff_metas = metas;
        self.rows_len = total;

        // ---- footer
        f.render_widget(
            Paragraph::new(self.status_bar(
                "REVIEW",
                PURPLE,
                "j/k scroll  c comment  a accept hunk  x reject hunk  Tab files  q back",
                area.width as usize,
            )),
            rows[1],
        );
    }

    // ------------------------------------------------------------ diff rows

    fn build_diff_rows(&self, width: usize) -> (Vec<Line<'static>>, Vec<Meta>) {
        let mut out: Vec<Line> = Vec::new();
        let mut metas: Vec<Meta> = Vec::new();
        if self.file_sel != 0 {
            out.push(Line::default());
            metas.push(Meta::Other);
            out.push(Line::from(Span::styled(
                "  mock diff data only exists for src/auth.py",
                s(DIM, BG),
            )));
            metas.push(Meta::Other);
            out.push(Line::from(Span::styled(
                "  select it in the Files panel (Tab, then j/k + Enter)",
                s(DIM, BG),
            )));
            metas.push(Meta::Other);
            return (out, metas);
        }
        for (i, l) in self.lines.iter().enumerate() {
            if i == 0 || self.lines[i - 1].hunk != l.hunk {
                out.push(self.hunk_header_row(l.hunk, width));
                metas.push(Meta::Hunk(l.hunk));
            }
            out.push(self.code_row(l, i, width));
            metas.push(Meta::Code(i));
            if i == MOCK_COMMENT_AT {
                for row in comment_box("mattf", PURPLE, MOCK_COMMENT, BORDER, width, false) {
                    out.push(row);
                    metas.push(Meta::Other);
                }
            }
            for (at, text) in &self.user_comments {
                if *at == i {
                    for row in comment_box("you", BLUE, text, BORDER, width, false) {
                        out.push(row);
                        metas.push(Meta::Other);
                    }
                }
            }
            if let Some(buf) = &self.input {
                if self.cursor == i {
                    for row in comment_box("comment", BLUE, buf, BLUE, width, true) {
                        out.push(row);
                        metas.push(Meta::Other);
                    }
                }
            }
        }
        (out, metas)
    }

    fn hunk_header_row(&self, hunk: usize, width: usize) -> Line<'static> {
        let text = HUNK_HEADERS[hunk];
        let chip = match self.verdicts[hunk] {
            Verdict::Accepted => Span::styled(" ✓ accepted ", s(BG, GREEN).add_modifier(Modifier::BOLD)),
            Verdict::Pending => Span::styled(" pending ", s(DIM, SEL)),
            Verdict::Rejected => Span::styled(" ✗ rejected ", s(BG, RED).add_modifier(Modifier::BOLD)),
        };
        pad_between(
            vec![Span::styled(format!(" {text}"), s(DIM, PANEL))],
            vec![chip, Span::styled(" ", s(DIM, PANEL))],
            width,
            PANEL,
        )
    }

    fn code_row(&self, l: &DLine, idx: usize, width: usize) -> Line<'static> {
        let (line_bg, em_bg, sign, sign_fg) = match l.kind {
            Kind::Ctx => (BG, BG, ' ', DIM),
            Kind::Del => (DEL_BG, DEL_EM, '-', RED),
            Kind::Add => (ADD_BG, ADD_EM, '+', GREEN),
        };
        let cursor_here = idx == self.cursor && self.focus == Focus::Diff;
        let line_bg = if cursor_here && l.kind == Kind::Ctx { SEL } else { line_bg };
        let gut_bg = if cursor_here { SEL } else { PANEL };
        let num = |n: Option<u16>| n.map_or("    ".to_string(), |v| format!("{v:>4}"));

        let mut spans: Vec<Span> = vec![
            Span::styled(format!("{} {}", num(l.old), num(l.new)), s(DIM, gut_bg)),
            if cursor_here {
                Span::styled("▎", s(BLUE, gut_bg))
            } else {
                Span::styled(" ", s(DIM, gut_bg))
            },
            Span::styled(format!("{sign} "), s(sign_fg, line_bg)),
        ];

        let fgs = py_fg(l.text);
        let marks = self.emph.get(&idx);
        let chars: Vec<char> = l.text.chars().collect();
        let mut k = 0;
        while k < chars.len() {
            let fg = fgs[k];
            let bg = if marks.is_some_and(|m| m[k]) { em_bg } else { line_bg };
            let start = k;
            while k < chars.len()
                && fgs[k] == fg
                && (if marks.is_some_and(|m| m[k]) { em_bg } else { line_bg }) == bg
            {
                k += 1;
            }
            let chunk: String = chars[start..k].iter().collect();
            spans.push(Span::styled(chunk, s(fg, bg)));
        }
        pad_to(spans, width, line_bg)
    }
}

// ---------------------------------------------------------------- comment boxes

fn comment_box(
    author: &str,
    author_bg: Color,
    text: &str,
    border: Color,
    width: usize,
    show_cursor: bool,
) -> Vec<Line<'static>> {
    let indent = 12usize.min(width / 4);
    let bw = width.saturating_sub(indent + 2).clamp(24, 66);
    let iw = bw - 4;
    let chip_len = author.chars().count() + 2;
    let wrap_w = if show_cursor { iw.saturating_sub(1) } else { iw };
    let body = wrap(text, wrap_w);
    let pad = |n: usize| Span::styled(" ".repeat(n), s(FG, BG));
    let mut out: Vec<Line> = Vec::new();

    out.push(pad_to(
        vec![
            pad(indent),
            Span::styled("╭─", s(border, BG)),
            Span::styled(
                format!(" {author} "),
                s(BG, author_bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}╮", "─".repeat(bw.saturating_sub(3 + chip_len))),
                s(border, BG),
            ),
        ],
        width,
        BG,
    ));
    let last = body.len() - 1;
    for (i, row) in body.iter().enumerate() {
        let mut spans = vec![
            pad(indent),
            Span::styled("│ ", s(border, BG)),
            Span::styled(row.clone(), s(FG, PANEL)),
        ];
        let mut used = row.chars().count();
        if show_cursor && i == last {
            spans.push(Span::styled("█", s(FG, PANEL)));
            used += 1;
        }
        spans.push(Span::styled(" ".repeat(iw.saturating_sub(used)), s(FG, PANEL)));
        spans.push(Span::styled(" │", s(border, BG)));
        out.push(pad_to(spans, width, BG));
    }
    out.push(pad_to(
        vec![
            pad(indent),
            Span::styled(format!("╰{}╯", "─".repeat(bw - 2)), s(border, BG)),
        ],
        width,
        BG,
    ));
    out
}

// ---------------------------------------------------------------- main

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = App::new().run(&mut terminal);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
