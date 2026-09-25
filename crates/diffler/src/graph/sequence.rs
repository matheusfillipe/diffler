//! Mermaid `sequenceDiagram` into a [`TextFigure`]: who calls whom, in order,
//! one lane per participant.
//!
//! Best effort by design, the way [`crate::graph::mermaid`] treats a
//! flowchart: `activate`/`deactivate`/`rect`/`box`/`create`/`destroy` are
//! simplified away and reported, never refused.

use crate::graph::model::NodeId;
use crate::graph::text_figure::{SpanKind, TextFigure, TextSpan};

/// Participants a diagram may declare. Past this a terminal card cannot lay
/// the lanes out readably anyway, and the source comes from an agent.
pub(crate) const MAX_PARTICIPANTS: usize = 12;
/// Messages, notes and frame markers combined.
pub(crate) const MAX_EVENTS: usize = 80;

const LANE_GAP: usize = 3;
const MIN_LANE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SequenceError {
    #[error("the sequence diagram declares no participants")]
    Empty,
}

#[derive(Debug, Clone)]
pub(crate) struct SequenceFigure {
    pub text: TextFigure,
    pub anchors: Vec<(NodeId, String)>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrowhead {
    Solid,
    Lost,
    Async,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Alt,
    Opt,
    Loop,
    Par,
    Critical,
    Break,
}

impl FrameKind {
    fn word(self) -> &'static str {
        match self {
            Self::Alt => "alt",
            Self::Opt => "opt",
            Self::Loop => "loop",
            Self::Par => "par",
            Self::Critical => "critical",
            Self::Break => "break",
        }
    }
}

enum Event {
    Message {
        from: usize,
        to: usize,
        label: String,
        dashed: bool,
        arrowhead: Arrowhead,
    },
    Note {
        first: usize,
        last: usize,
        label: String,
    },
    FrameStart {
        kind: FrameKind,
        label: String,
    },
    FrameDivider {
        keyword: &'static str,
        label: String,
    },
    FrameEnd,
}

struct Lane {
    id: String,
    label: String,
}

#[derive(Default)]
struct Parsed {
    lanes: Vec<Lane>,
    events: Vec<Event>,
    links: Vec<(String, String)>,
    notes: Vec<String>,
    autonumber: bool,
    frame_depth: usize,
}

impl Parsed {
    fn note_once(&mut self, note: String) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    fn lane_of(&mut self, id: &str) -> usize {
        if let Some(at) = self.lanes.iter().position(|l| l.id == id) {
            return at;
        }
        self.lanes.push(Lane {
            id: id.to_owned(),
            label: id.to_owned(),
        });
        self.lanes.len() - 1
    }

    fn declare(&mut self, rest: &str, actor: bool) {
        let _ = actor;
        let (id, alias) = rest
            .split_once(" as ")
            .map_or((rest.trim(), None), |(id, alias)| {
                (id.trim(), Some(alias.trim().to_owned()))
            });
        if id.is_empty() {
            return;
        }
        let at = self.lane_of(id);
        if let (Some(alias), Some(lane)) = (alias, self.lanes.get_mut(at)) {
            lane.label = alias;
        }
    }

    fn message(&mut self, line: &str) {
        let Some((from, arrow, rest)) = find_arrow(line) else {
            return;
        };
        let mut rest = rest;
        if rest.starts_with(['+', '-']) {
            self.note_once("activate/deactivate is ignored".to_owned());
            rest = &rest[1..];
        }
        let (to, label) = rest
            .split_once(':')
            .map_or((rest.trim(), String::new()), |(to, label)| {
                (to.trim(), label.trim().to_owned())
            });
        if to.is_empty() {
            return;
        }
        let from = self.lane_of(from.trim());
        let to = self.lane_of(to);
        let label = if self.autonumber {
            format!("{}. {label}", self.events.len() + 1)
        } else {
            label
        };
        self.events.push(Event::Message {
            from,
            to,
            label,
            dashed: arrow.starts_with("--"),
            arrowhead: arrowhead_of(arrow),
        });
    }

    fn note(&mut self, rest: &str) {
        let rest = rest.trim();
        let (span, label) = rest
            .split_once(':')
            .map_or((rest, String::new()), |(span, label)| {
                (span, label.trim().to_owned())
            });
        let span = span
            .trim_start_matches("over")
            .trim_start_matches("left of")
            .trim_start_matches("right of")
            .trim();
        let participants: Vec<usize> = span
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| self.lane_of(p))
            .collect();
        if participants.is_empty() {
            return;
        }
        let first = participants.iter().copied().min().unwrap_or(0);
        let last = participants.iter().copied().max().unwrap_or(0);
        self.events.push(Event::Note { first, last, label });
    }

    fn statement(&mut self, line: &str) {
        let head = line
            .split(|c: char| c.is_whitespace() || c == ':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let rest = line
            .get(head.len()..)
            .map(str::trim_start)
            .unwrap_or_default();
        match head.as_str() {
            "participant" => self.declare(rest, false),
            "actor" => self.declare(rest, true),
            "autonumber" => self.autonumber = true,
            "note" => self.note(rest),
            "link" => self.link(rest),
            "alt" => self.frame_start(FrameKind::Alt, rest),
            "opt" => self.frame_start(FrameKind::Opt, rest),
            "loop" => self.frame_start(FrameKind::Loop, rest),
            "par" => self.frame_start(FrameKind::Par, rest),
            "critical" => self.frame_start(FrameKind::Critical, rest),
            "break" => self.frame_start(FrameKind::Break, rest),
            "else" => self.frame_divider("else", rest),
            "and" => self.frame_divider("and", rest),
            "option" => self.frame_divider("option", rest),
            "end" => self.frame_end(),
            "activate" | "deactivate" | "rect" | "box" | "create" | "destroy" => {
                self.note_once(format!("`{head}` is ignored"));
            }
            _ => self.message(line),
        }
    }

    fn frame_start(&mut self, kind: FrameKind, label: &str) {
        self.frame_depth += 1;
        if self.frame_depth > 1 {
            self.note_once(
                "a nested alt/opt/loop/par/critical/break draws flat, not inset".to_owned(),
            );
        }
        self.events.push(Event::FrameStart {
            kind,
            label: label.trim().to_owned(),
        });
    }

    fn frame_divider(&mut self, keyword: &'static str, label: &str) {
        if self.frame_depth == 0 {
            return;
        }
        self.events.push(Event::FrameDivider {
            keyword,
            label: label.trim().to_owned(),
        });
    }

    fn frame_end(&mut self) {
        if self.frame_depth == 0 {
            return;
        }
        self.frame_depth -= 1;
        self.events.push(Event::FrameEnd);
    }

    /// `link <id>: <label> @ <target>`, mermaid's own `link` statement
    /// repurposed to carry the anchor a receiving message jumps to.
    fn link(&mut self, rest: &str) {
        let Some((id, rest)) = rest.split_once(':') else {
            return;
        };
        let Some((_, target)) = rest.split_once(" @ ") else {
            return;
        };
        let target = target.trim();
        if target.is_empty() {
            return;
        }
        let id = id.trim().to_owned();
        self.links.retain(|(known, _)| known != &id);
        self.links.push((id, target.to_owned()));
    }
}

fn arrowhead_of(arrow: &str) -> Arrowhead {
    if arrow.ends_with('x') {
        Arrowhead::Lost
    } else if arrow.ends_with(')') {
        Arrowhead::Async
    } else {
        Arrowhead::Solid
    }
}

/// The arrow tokens a message line names, longest first so `-->>` is not
/// mistaken for `->>` starting one character late.
const ARROWS: &[&str] = &["-->>", "--x", "--)", "->>", "-x", "-)", "-->", "->"];

/// The earliest, longest arrow in `line`, split into `(from, arrow, rest)`.
fn find_arrow(line: &str) -> Option<(&str, &str, &str)> {
    let mut best: Option<(usize, &str)> = None;
    for &arrow in ARROWS {
        if let Some(at) = line.find(arrow)
            && best.is_none_or(|(best_at, best_arrow)| {
                at < best_at || (at == best_at && arrow.len() > best_arrow.len())
            })
        {
            best = Some((at, arrow));
        }
    }
    let (at, arrow) = best?;
    Some((&line[..at], arrow, &line[at + arrow.len()..]))
}

pub(crate) fn parse(src: &str) -> Result<SequenceFigure, SequenceError> {
    let mut lines = src
        .lines()
        .map(|line| line.split_once("%%").map_or(line, |(head, _)| head))
        .map(str::trim)
        .filter(|line| !line.is_empty());
    // the header (`sequenceDiagram`) is already how the caller chose this
    // parser; skip it here too so it is never read as a bare message
    let _ = lines.next();

    let mut parsed = Parsed::default();
    for line in lines {
        parsed.statement(line);
    }
    if parsed.lanes.is_empty() {
        return Err(SequenceError::Empty);
    }
    if parsed.lanes.len() > MAX_PARTICIPANTS {
        parsed.lanes.truncate(MAX_PARTICIPANTS);
        parsed.events.retain(|event| match event {
            Event::Message { from, to, .. } => *from < MAX_PARTICIPANTS && *to < MAX_PARTICIPANTS,
            Event::Note { first, last, .. } => {
                *first < MAX_PARTICIPANTS && *last < MAX_PARTICIPANTS
            }
            Event::FrameStart { .. } | Event::FrameDivider { .. } | Event::FrameEnd => true,
        });
        parsed.notes.push(format!(
            "only the first {MAX_PARTICIPANTS} participants are drawn"
        ));
    }
    if parsed.events.len() > MAX_EVENTS {
        parsed.events.truncate(MAX_EVENTS);
        parsed
            .notes
            .push(format!("only the first {MAX_EVENTS} events are drawn"));
    }

    let (anchors, links_by_lane) = resolve_links(&parsed.lanes, &parsed.links);
    let text = render(&parsed.lanes, &parsed.events, &links_by_lane);
    Ok(SequenceFigure {
        text,
        anchors,
        notes: parsed.notes,
    })
}

/// One [`NodeId`] per linked participant, and which lane index it belongs to,
/// for [`render`] to attach it to that participant's own message rows.
fn resolve_links(
    lanes: &[Lane],
    links: &[(String, String)],
) -> (Vec<(NodeId, String)>, Vec<Option<NodeId>>) {
    let mut anchors = Vec::new();
    let mut by_lane = vec![None; lanes.len()];
    for (id, target) in links {
        let Some(at) = lanes.iter().position(|l| &l.id == id) else {
            continue;
        };
        let node = NodeId::new(format!("participant:{id}"));
        anchors.push((node.clone(), target.clone()));
        if let Some(slot) = by_lane.get_mut(at) {
            *slot = Some(node);
        }
    }
    (anchors, by_lane)
}

struct LaneGeometry {
    x: usize,
    w: usize,
}

fn layout_lanes(lanes: &[Lane]) -> Vec<LaneGeometry> {
    let mut x = 0usize;
    lanes
        .iter()
        .map(|lane| {
            let w = lane.label.chars().count().max(MIN_LANE) + 2;
            let geometry = LaneGeometry { x, w };
            x += w + LANE_GAP;
            geometry
        })
        .collect()
}

fn center(geometry: &LaneGeometry) -> usize {
    geometry.x + geometry.w / 2
}

/// A blank canvas as `width` columns of `' '` for every row, the base every
/// event's own row overlays: a fresh lifeline is redrawn on top per row.
struct Canvas {
    rows: Vec<Vec<char>>,
    width: usize,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            rows: vec![vec![' '; width]; height],
            width,
        }
    }

    fn put(&mut self, x: usize, y: usize, ch: char) {
        if let Some(cell) = self.rows.get_mut(y).and_then(|row| row.get_mut(x)) {
            *cell = ch;
        }
    }

    fn write(&mut self, x: usize, y: usize, text: &str) -> usize {
        let mut at = x;
        for ch in text.chars() {
            self.put(at, y, ch);
            at += 1;
        }
        at - x
    }

    fn write_centered(&mut self, from: usize, to: usize, y: usize, text: &str) {
        let span = to.saturating_sub(from).max(text.chars().count());
        let elided = elide(text, span);
        let pad = span.saturating_sub(elided.chars().count()) / 2;
        self.write(from + pad, y, &elided);
    }

    fn lifelines(&mut self, y: usize, lanes: &[LaneGeometry]) {
        for lane in lanes {
            self.put(center(lane), y, '│');
        }
    }

    fn into_lines(self) -> Vec<String> {
        self.rows
            .into_iter()
            .map(|row| row.into_iter().collect::<String>().trim_end().to_owned())
            .collect()
    }
}

fn elide(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_owned();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

/// Rows an event occupies: a cross-lane message is a label then an arrow, a
/// self message, note, or frame marker takes just one.
fn event_rows(event: &Event) -> usize {
    match event {
        Event::Message { from, to, .. } if from == to => 1,
        Event::Message { .. } => 2,
        Event::Note { .. }
        | Event::FrameStart { .. }
        | Event::FrameDivider { .. }
        | Event::FrameEnd => 1,
    }
}

fn render(lanes: &[Lane], events: &[Event], links_by_lane: &[Option<NodeId>]) -> TextFigure {
    let geometry = layout_lanes(lanes);
    let width = geometry.last().map_or(0, |last| last.x + last.w).max(1);
    let body_rows: usize = events.iter().map(event_rows).sum();
    let mut canvas = Canvas::new(width, 1 + body_rows);
    let mut spans = Vec::new();
    let mut row_nodes: Vec<Option<NodeId>> = vec![None; 1 + body_rows];

    for (lane, geo) in lanes.iter().zip(&geometry) {
        let label = format!("[ {} ]", lane.label);
        let from = centered_start(geo, label.chars().count());
        let len = canvas.write(from, 0, &label);
        spans.push(TextSpan {
            x: u16::try_from(from).unwrap_or(0),
            y: 0,
            len: u16::try_from(len).unwrap_or(0),
            kind: SpanKind::Fg,
        });
    }

    let mut row = 1usize;
    for event in events {
        row = draw_event(
            &mut canvas,
            &geometry,
            links_by_lane,
            &mut row_nodes,
            &mut spans,
            event,
            row,
        );
    }

    TextFigure {
        width: u16::try_from(canvas.width).unwrap_or(u16::MAX),
        height: u16::try_from(canvas.rows.len()).unwrap_or(u16::MAX),
        lines: canvas.into_lines(),
        spans,
        row_nodes,
    }
}

/// The left column that centers `len` characters within `lane`.
fn centered_start(lane: &LaneGeometry, len: usize) -> usize {
    lane.x + lane.w.saturating_sub(len) / 2
}

// draws one event's row(s) onto `canvas`, returning the row index the next
// event starts at; kept as one function since every branch shares the same
// lane geometry and row bookkeeping, and splitting it would only pass that
// context back and forth
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn draw_event(
    canvas: &mut Canvas,
    geometry: &[LaneGeometry],
    links_by_lane: &[Option<NodeId>],
    row_nodes: &mut [Option<NodeId>],
    spans: &mut Vec<TextSpan>,
    event: &Event,
    row: usize,
) -> usize {
    match event {
        Event::Message {
            from, to, label, ..
        } if from == to => {
            canvas.lifelines(row, geometry);
            if let Some(geo) = geometry.get(*from) {
                let text = format!("↺ {label}");
                let start = geo.x;
                let len = canvas.write(
                    start,
                    row,
                    &elide(&text, canvas.width.saturating_sub(start)),
                );
                spans.push(TextSpan {
                    x: u16::try_from(start).unwrap_or(0),
                    y: u16::try_from(row).unwrap_or(0),
                    len: u16::try_from(len).unwrap_or(0),
                    kind: SpanKind::Fg,
                });
            }
            if let Some(Some(node)) = links_by_lane.get(*to)
                && let Some(slot) = row_nodes.get_mut(row)
            {
                *slot = Some(node.clone());
            }
            row + 1
        }
        Event::Message {
            from,
            to,
            label,
            dashed,
            arrowhead,
        } => {
            let (Some(a), Some(b)) = (geometry.get(*from), geometry.get(*to)) else {
                return row;
            };
            let (left, right, forward) = if center(a) <= center(b) {
                (a, b, true)
            } else {
                (b, a, false)
            };
            let label_row = row;
            let arrow_row = row + 1;
            canvas.lifelines(label_row, geometry);
            canvas.write_centered(center(left), center(right), label_row, label);
            spans.push(TextSpan {
                x: u16::try_from(center(left)).unwrap_or(0),
                y: u16::try_from(label_row).unwrap_or(0),
                len: u16::try_from(center(right).saturating_sub(center(left))).unwrap_or(0),
                kind: SpanKind::Fg,
            });
            canvas.lifelines(arrow_row, geometry);
            let body = if *dashed { '╌' } else { '─' };
            for x in center(left) + 1..center(right) {
                canvas.put(x, arrow_row, body);
            }
            let head = match arrowhead {
                Arrowhead::Solid => {
                    if forward {
                        '▸'
                    } else {
                        '◂'
                    }
                }
                Arrowhead::Lost => 'x',
                Arrowhead::Async => ')',
            };
            if forward {
                canvas.put(center(right), arrow_row, head);
            } else {
                canvas.put(center(left), arrow_row, head);
            }
            if let Some(Some(node)) = links_by_lane.get(*to)
                && let Some(slot) = row_nodes.get_mut(arrow_row)
            {
                *slot = Some(node.clone());
            }
            row + 2
        }
        Event::Note { first, last, label } => {
            canvas.lifelines(row, geometry);
            let (Some(a), Some(b)) = (geometry.get(*first), geometry.get(*last)) else {
                return row + 1;
            };
            let text = format!("┤ {label} ├");
            canvas.write_centered(a.x, b.x + b.w, row, &text);
            spans.push(TextSpan {
                x: u16::try_from(a.x).unwrap_or(0),
                y: u16::try_from(row).unwrap_or(0),
                len: u16::try_from(b.x + b.w - a.x).unwrap_or(0),
                kind: SpanKind::Fg,
            });
            row + 1
        }
        Event::FrameStart { kind, label } => {
            draw_frame_rule(canvas, spans, row, '┌', '┐', kind.word(), label);
            row + 1
        }
        Event::FrameDivider { keyword, label } => {
            draw_frame_rule(canvas, spans, row, '├', '┤', keyword, label);
            row + 1
        }
        Event::FrameEnd => {
            draw_frame_rule(canvas, spans, row, '└', '┘', "", "");
            row + 1
        }
    }
}

// a frame rule's own drawing: caps, the fill between, and its centered
// label, which is enough distinct behavior to earn the extra parameter over
// folding it into `draw_event` itself
#[allow(clippy::too_many_arguments)]
fn draw_frame_rule(
    canvas: &mut Canvas,
    spans: &mut Vec<TextSpan>,
    row: usize,
    left_cap: char,
    right_cap: char,
    kind: &str,
    label: &str,
) {
    let width = canvas.width;
    canvas.put(0, row, left_cap);
    for x in 1..width.saturating_sub(1) {
        canvas.put(x, row, '─');
    }
    if width > 1 {
        canvas.put(width - 1, row, right_cap);
    }
    let text = if kind.is_empty() && label.is_empty() {
        String::new()
    } else if kind.is_empty() {
        format!(" {label} ")
    } else if label.is_empty() {
        format!(" {kind} ")
    } else {
        format!(" {kind}: {label} ")
    };
    if !text.is_empty() {
        let len = canvas.write(2, row, &elide(&text, width.saturating_sub(4)));
        spans.push(TextSpan {
            x: 2,
            y: u16::try_from(row).unwrap_or(0),
            len: u16::try_from(len).unwrap_or(0),
            kind: SpanKind::Fg,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn figure(src: &str) -> SequenceFigure {
        parse(src).expect("parsed")
    }

    #[test]
    fn participants_and_a_message_draw() {
        let figure = figure("sequenceDiagram\n  participant A\n  participant B\n  A->>B: hello");
        assert!(figure.text.lines[0].contains('A'));
        assert!(figure.text.lines[0].contains('B'));
        let art = figure.text.lines.join("\n");
        assert!(art.contains("hello"));
        assert!(art.contains('▸'), "{art}");
    }

    #[test]
    fn participants_are_declared_implicitly_by_first_use() {
        let figure = figure("sequenceDiagram\n  A->>B: hi");
        assert_eq!(figure.text.lines[0].matches('[').count(), 2);
    }

    #[test]
    fn an_alias_becomes_the_lane_label() {
        let figure = figure("sequenceDiagram\n  participant A as Alice\n  A->>A: think");
        assert!(figure.text.lines[0].contains("Alice"));
    }

    #[test]
    fn a_dashed_reply_draws_dashed() {
        let figure = figure("sequenceDiagram\n  A->>B: ask\n  B-->>A: answer");
        let art = figure.text.lines.join("\n");
        assert!(art.contains('╌'), "{art}");
    }

    #[test]
    fn a_lost_message_draws_an_x() {
        let figure = figure("sequenceDiagram\n  A-x B: gone");
        assert!(figure.text.lines.join("\n").contains('x'));
    }

    #[test]
    fn a_self_message_draws_a_loop_glyph() {
        let figure = figure("sequenceDiagram\n  A->>A: think");
        assert!(figure.text.lines.join("\n").contains('↺'));
    }

    #[test]
    fn a_note_over_two_participants_spans_them() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  Note over A,B: greeting");
        assert!(figure.text.lines.join("\n").contains("greeting"));
    }

    #[test]
    fn autonumber_prefixes_every_message() {
        let figure = figure("sequenceDiagram\n  autonumber\n  A->>B: hi\n  B-->>A: hey");
        let art = figure.text.lines.join("\n");
        assert!(art.contains("1. hi"), "{art}");
        assert!(art.contains("2. hey"), "{art}");
    }

    #[test]
    fn alt_else_end_draws_frame_rules() {
        let figure = figure(
            "sequenceDiagram\n  alt success\n    A->>B: go\n  else failure\n    A->>B: stop\n  end",
        );
        let art = figure.text.lines.join("\n");
        assert!(art.contains("alt: success"), "{art}");
        assert!(art.contains("else: failure"), "{art}");
        assert!(art.contains('└'), "{art}");
    }

    #[test]
    fn ignored_directives_are_noted_not_drawn() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  activate B\n  deactivate B");
        assert!(figure.notes.iter().any(|n| n.contains("activate")));
    }

    #[test]
    fn a_link_attaches_to_the_receivers_message_row() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  link B: profile @ src/b.rs#B");
        assert_eq!(
            figure.anchors,
            [(NodeId::new("participant:B"), "src/b.rs#B".to_owned())]
        );
        let jump_row = (1..figure.text.height).find(|&row| figure.text.node_at_row(row).is_some());
        assert_eq!(jump_row, Some(2), "the arrow row, not the label row");
    }

    #[test]
    fn a_diagram_with_no_participants_is_an_error() {
        assert_eq!(parse("sequenceDiagram").unwrap_err(), SequenceError::Empty);
    }

    #[test]
    fn a_diagram_past_the_participant_cap_is_truncated_and_says_so() {
        use std::fmt::Write as _;
        let mut src = String::from("sequenceDiagram\n");
        for index in 0..(MAX_PARTICIPANTS + 3) {
            let _ = writeln!(src, "  participant p{index}");
        }
        let figure = figure(&src);
        assert_eq!(figure.text.lines[0].matches('[').count(), MAX_PARTICIPANTS);
        assert!(figure.notes.iter().any(|n| n.contains("participants")));
    }

    #[test]
    fn an_unknown_message_to_participant_still_draws() {
        let figure = figure("sequenceDiagram\n  Alice->>Unknown: hi");
        assert!(figure.text.lines[0].contains("Unknown"));
    }

    /// The canvas is sized from the participants' own lane widths, not from
    /// message content, so a label far longer than the card just runs off
    /// its edge rather than growing it; this only has to not panic.
    #[test]
    fn a_very_long_message_label_does_not_panic() {
        let long = "x".repeat(500);
        let src = format!("sequenceDiagram\n  a->>b: {long}");
        let figure = figure(&src);
        assert!(figure.text.lines.join("\n").contains('x'));
    }

    #[test]
    fn a_single_participant_with_no_message_still_draws() {
        let figure = figure("sequenceDiagram\n  participant Solo");
        assert!(figure.text.lines[0].contains("Solo"));
        assert_eq!(figure.text.lines.len(), 1, "just the header row");
    }
}
