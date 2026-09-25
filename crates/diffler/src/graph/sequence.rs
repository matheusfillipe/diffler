//! Mermaid `sequenceDiagram` into a [`TextFigure`]: who calls whom, in order,
//! one lane per participant.
//!
//! Best effort by design, the way [`crate::graph::mermaid`] treats a
//! flowchart: `activate`/`deactivate`/`rect`/`box`/`create`/`destroy` are
//! simplified away and reported, never refused.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::graph::model::NodeId;
use crate::graph::text_figure::{SpanKind, TextFigure, TextSpan, elide};

/// Participants a diagram may declare. Past this a terminal card cannot lay
/// the lanes out readably anyway, and the source comes from an agent.
pub(crate) const MAX_PARTICIPANTS: usize = 12;
/// Messages, notes and frame markers combined.
pub(crate) const MAX_EVENTS: usize = 80;

const LANE_GAP: usize = 3;

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
    messages: usize,
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

    fn declare(&mut self, rest: &str) {
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
        self.messages += 1;
        let label = if self.autonumber {
            format!("{}. {label}", self.messages)
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
            "participant" | "actor" => self.declare(rest),
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
        let Some((_, target)) = rest.rsplit_once(" @ ") else {
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

/// The arrow tokens a message line names. Order here does not matter:
/// [`find_arrow`] breaks a tie by length, so `-->>` is never mistaken for
/// `->>` starting one character late.
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

/// Parse and lay out a `sequenceDiagram`, widening the lanes so message
/// labels fit, as far as `max_width` columns allow; a label that still does
/// not fit its lane is elided.
pub(crate) fn parse(src: &str, max_width: usize) -> Result<SequenceFigure, SequenceError> {
    let mut lines = crate::graph::mermaid::statements(src);
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
    let text = render(&parsed.lanes, &parsed.events, &links_by_lane, max_width);
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

fn box_label(lane: &Lane) -> String {
    format!("[ {} ]", lane.label)
}

/// Columns a message label needs between its two lifelines: one blank on
/// each side, plus the arrowhead's own cell.
fn message_need(label: &str) -> usize {
    label.width() + 3
}

/// Columns a self message's `↺ label` needs from its own lifeline to the next.
fn self_need(label: &str) -> usize {
    label.width() + 5
}

/// Each lane's lifeline column, and the canvas width. Lanes start packed as
/// tight as their boxes allow; then, left to right, the gap before a
/// message's right end grows until its label fits, while `max_width` lasts.
fn lane_centers(lanes: &[Lane], events: &[Event], max_width: usize) -> (Vec<usize>, usize) {
    let boxes: Vec<usize> = lanes.iter().map(|l| box_label(l).width()).collect();
    // `gaps[i]` is lane i's lifeline minus lane i-1's (minus the canvas edge
    // for lane 0); the extra last entry is the canvas past the last lifeline
    let mut gaps: Vec<usize> = boxes
        .iter()
        .enumerate()
        .map(
            |(i, &w)| match i.checked_sub(1).and_then(|p| boxes.get(p)) {
                Some(&prev) => prev - prev / 2 + LANE_GAP + w / 2,
                None => w / 2,
            },
        )
        .collect();
    gaps.push(boxes.last().map_or(1, |&w| w - w / 2));
    let mut budget = max_width.saturating_sub(gaps.iter().sum());
    let mut needs: Vec<(usize, usize, usize)> = events
        .iter()
        .filter_map(|event| match event {
            Event::Message {
                from, to, label, ..
            } if from == to => Some((*from, from + 1, self_need(label))),
            Event::Message {
                from, to, label, ..
            } => Some(((*from).min(*to), (*from).max(*to), message_need(label))),
            _ => None,
        })
        .collect();
    needs.sort_by_key(|&(_, right, _)| right);
    for (left, right, need) in needs {
        let have: usize = gaps.get(left + 1..=right).map_or(0, |g| g.iter().sum());
        let grow = need.saturating_sub(have).min(budget);
        if let Some(gap) = gaps.get_mut(right) {
            *gap += grow;
            budget -= grow;
        }
    }
    // a note or a frame rule spans the whole canvas, so it can only widen the
    // right margin
    let banner = events
        .iter()
        .filter_map(|event| match event {
            Event::Note { label, .. }
            | Event::FrameStart { label, .. }
            | Event::FrameDivider { label, .. } => Some(label.width() + 12),
            Event::Message { .. } | Event::FrameEnd => None,
        })
        .max()
        .unwrap_or(0);
    let grow = banner.saturating_sub(gaps.iter().sum()).min(budget);
    if let Some(tail) = gaps.last_mut() {
        *tail += grow;
    }
    let centers = gaps
        .iter()
        .take(lanes.len())
        .scan(0, |at, gap| {
            *at += gap;
            Some(*at)
        })
        .collect();
    (centers, gaps.iter().sum())
}

/// A blank canvas as `width` columns of `' '` for every row, the base every
/// event's own row overlays: a fresh lifeline is redrawn on top per row.
struct Canvas {
    rows: Vec<Vec<char>>,
    width: usize,
}

impl Canvas {
    /// Marks the trailing column of a two-cell-wide glyph, so [`Self::into_lines`]
    /// can drop it: emitting a real cell there would shift everything after
    /// it one column to the right.
    const WIDE_CONT: char = '\u{e000}';

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

    /// Write `text` starting at `x`, advancing by each glyph's terminal
    /// width, and return the display columns it took: a wide glyph occupies
    /// two columns and marks the second so it is never overwritten.
    fn write(&mut self, x: usize, y: usize, text: &str) -> usize {
        let mut at = x;
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0);
            self.put(at, y, ch);
            if w == 2 {
                self.put(at + 1, y, Self::WIDE_CONT);
            }
            at += w.max(1);
        }
        at - x
    }

    fn lifelines(&mut self, y: usize, centers: &[usize]) {
        for &x in centers {
            self.put(x, y, '│');
        }
    }

    fn into_lines(self) -> Vec<String> {
        self.rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .filter(|&c| c != Self::WIDE_CONT)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }
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

struct Draw<'a> {
    canvas: Canvas,
    centers: &'a [usize],
    links_by_lane: &'a [Option<NodeId>],
    row_nodes: Vec<Option<NodeId>>,
    spans: Vec<TextSpan>,
}

fn render(
    lanes: &[Lane],
    events: &[Event],
    links_by_lane: &[Option<NodeId>],
    max_width: usize,
) -> TextFigure {
    let (centers, width) = lane_centers(lanes, events, max_width);
    let rows = 1 + events.iter().map(event_rows).sum::<usize>();
    let mut draw = Draw {
        canvas: Canvas::new(width, rows),
        centers: &centers,
        links_by_lane,
        row_nodes: vec![None; rows],
        spans: Vec::new(),
    };
    for (lane, &center) in lanes.iter().zip(&centers) {
        let label = box_label(lane);
        let start = center.saturating_sub(label.width() / 2);
        draw.text(start, 0, &label);
    }
    let mut row = 1;
    for event in events {
        draw.event(event, row);
        row += event_rows(event);
    }
    TextFigure {
        width: u16::try_from(draw.canvas.width).unwrap_or(u16::MAX),
        height: u16::try_from(rows).unwrap_or(u16::MAX),
        lines: draw.canvas.into_lines(),
        spans: draw.spans,
        row_nodes: draw.row_nodes,
    }
}

impl Draw<'_> {
    fn center(&self, lane: usize) -> usize {
        self.centers.get(lane).copied().unwrap_or(0)
    }

    /// Write `text` in the foreground colour, the one thing on its row the
    /// reader should read.
    fn text(&mut self, x: usize, y: usize, text: &str) {
        let len = self.canvas.write(x, y, text);
        self.spans.push(TextSpan {
            x: u16::try_from(x).unwrap_or(u16::MAX),
            y: u16::try_from(y).unwrap_or(u16::MAX),
            len: u16::try_from(len).unwrap_or(0),
            kind: SpanKind::Fg,
        });
    }

    fn jump_to(&mut self, lane: usize, rows: std::ops::Range<usize>) {
        let Some(Some(node)) = self.links_by_lane.get(lane) else {
            return;
        };
        for row in rows {
            if let Some(slot) = self.row_nodes.get_mut(row) {
                *slot = Some(node.clone());
            }
        }
    }

    fn event(&mut self, event: &Event, row: usize) {
        match event {
            Event::Message {
                from, to, label, ..
            } if from == to => {
                self.canvas.lifelines(row, self.centers);
                let x = self.center(*from) + 2;
                let room = self
                    .centers
                    .get(from + 1)
                    .map_or(self.canvas.width, |next| next.saturating_sub(1))
                    .saturating_sub(x);
                self.text(x, row, &elide(&format!("↺ {label}"), room));
                self.jump_to(*to, row..row + 1);
            }
            Event::Message {
                from,
                to,
                label,
                dashed,
                arrowhead,
            } => {
                let forward = self.center(*from) <= self.center(*to);
                let (left, right) = if forward {
                    (self.center(*from), self.center(*to))
                } else {
                    (self.center(*to), self.center(*from))
                };
                self.canvas.lifelines(row, self.centers);
                self.canvas.lifelines(row + 1, self.centers);
                let room = right.saturating_sub(left + 3);
                let label = elide(label, room);
                let pad = room.saturating_sub(label.width()) / 2;
                self.text(left + 2 + pad, row, &label);
                let body = if *dashed { '╌' } else { '─' };
                for x in left + 1..right {
                    self.canvas.put(x, row + 1, body);
                }
                let head = match (arrowhead, forward) {
                    (Arrowhead::Solid, true) => '▸',
                    (Arrowhead::Solid, false) => '◂',
                    (Arrowhead::Async, true) => '▹',
                    (Arrowhead::Async, false) => '◃',
                    (Arrowhead::Lost, _) => '×',
                };
                let head_at = if forward { right - 1 } else { left + 1 };
                self.canvas.put(head_at, row + 1, head);
                self.jump_to(*to, row..row + 2);
            }
            Event::Note { first, last, label } => {
                self.canvas.lifelines(row, self.centers);
                let text = elide(&format!("┤ {label} ├"), self.canvas.width);
                let len = text.width();
                let middle = usize::midpoint(self.center(*first), self.center(*last));
                let start = middle
                    .saturating_sub(len / 2)
                    .min(self.canvas.width.saturating_sub(len));
                self.text(start, row, &text);
            }
            Event::FrameStart { kind, label } => {
                self.frame_rule(row, ('┌', '┐'), kind.word(), label);
            }
            Event::FrameDivider { keyword, label } => {
                self.frame_rule(row, ('├', '┤'), keyword, label);
            }
            Event::FrameEnd => self.frame_rule(row, ('└', '┘'), "", ""),
        }
    }

    fn frame_rule(&mut self, row: usize, (left, right): (char, char), kind: &str, label: &str) {
        let width = self.canvas.width;
        self.canvas.put(0, row, left);
        for x in 1..width.saturating_sub(1) {
            self.canvas.put(x, row, '─');
        }
        if width > 1 {
            self.canvas.put(width - 1, row, right);
        }
        let text = match (kind.is_empty(), label.is_empty()) {
            (true, true) => return,
            (true, false) => format!(" {label} "),
            (false, true) => format!(" {kind} "),
            (false, false) => format!(" {kind}: {label} "),
        };
        self.text(2, row, &elide(&text, width.saturating_sub(4)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn figure(src: &str) -> SequenceFigure {
        parse(src, usize::MAX).expect("parsed")
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
    fn a_lost_message_draws_a_cross() {
        let figure = figure("sequenceDiagram\n  A-x B: gone");
        assert!(figure.text.lines.join("\n").contains('×'));
    }

    #[test]
    fn a_self_message_draws_a_loop_glyph_beside_its_lifeline() {
        let figure = figure("sequenceDiagram\n  participant A\n  A->>A: think");
        let row = &figure.text.lines[1];
        assert!(row.contains("│ ↺ think"), "{row}");
    }

    #[test]
    fn a_note_over_two_participants_spans_them() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  Note over A,B: greeting");
        assert!(figure.text.lines.join("\n").contains("greeting"));
    }

    /// Only messages take a number: a note or a frame between two messages
    /// never makes the count skip.
    #[test]
    fn autonumber_counts_messages_only() {
        let figure = figure(
            "sequenceDiagram\n  autonumber\n  A->>B: hi\n  Note over A: aside\n  \
             alt ok\n    B-->>A: hey\n  end",
        );
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
    fn end_without_a_frame_is_ignored() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  end\n  else nope");
        assert_eq!(figure.text.lines.len(), 3, "{:?}", figure.text.lines);
    }

    #[test]
    fn ignored_directives_are_noted_not_drawn() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  activate B\n  deactivate B");
        assert!(figure.notes.iter().any(|n| n.contains("activate")));
    }

    #[test]
    fn a_link_attaches_to_both_rows_of_the_receivers_message() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  link B: profile @ src/b.rs#B");
        assert_eq!(
            figure.anchors,
            [(NodeId::new("participant:B"), "src/b.rs#B".to_owned())]
        );
        let rows: Vec<u16> = (0..figure.text.height)
            .filter(|&row| figure.text.node_at_row(row).is_some())
            .collect();
        assert_eq!(rows, [1, 2], "the label row and the arrow row");
    }

    #[test]
    fn a_link_label_may_hold_an_at_sign() {
        let figure = figure("sequenceDiagram\n  A->>B: hi\n  link B: mail @ me @ src/b.rs#B");
        assert_eq!(figure.anchors[0].1, "src/b.rs#B");
    }

    #[test]
    fn a_diagram_with_no_participants_is_an_error() {
        assert_eq!(
            parse("sequenceDiagram", usize::MAX).unwrap_err(),
            SequenceError::Empty
        );
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

    /// Lanes widen to fit a label while the card has room, so nothing is
    /// elided that did not have to be.
    #[test]
    fn lanes_widen_to_fit_a_label_the_card_has_room_for() {
        let label = "POST /login {user, pass}";
        let src = format!("sequenceDiagram\n  A->>B: {label}");
        let wide = parse(&src, 80).expect("parsed");
        assert!(wide.text.lines[1].contains(label), "{:?}", wide.text.lines);
        assert!(wide.text.width <= 80);
    }

    /// A label wider than the card allows is elided to its lane, never
    /// written across the next lifeline or past the canvas edge.
    #[test]
    fn a_label_too_long_for_the_card_is_elided_to_its_lane() {
        let long = "x".repeat(500);
        let src = format!("sequenceDiagram\n  participant A\n  participant B\n  A->>B: {long}");
        let figure = parse(&src, 40).expect("parsed");
        assert!(figure.text.width <= 40, "{}", figure.text.width);
        let label_row = &figure.text.lines[1];
        assert!(label_row.contains('…'), "{label_row}");
        assert_eq!(label_row.matches('│').count(), 2, "{label_row}");
    }

    #[test]
    fn a_single_participant_with_no_message_still_draws() {
        let figure = figure("sequenceDiagram\n  participant Solo");
        assert!(figure.text.lines[0].contains("Solo"));
        assert_eq!(figure.text.lines.len(), 1, "just the header row");
    }

    #[test]
    fn crlf_and_comment_lines_parse_like_plain_ones() {
        let figure = figure("sequenceDiagram\r\n  %% a comment\r\n  A->>B: hi  \r\n");
        assert_eq!(figure.text.lines.len(), 3, "{:?}", figure.text.lines);
        assert!(figure.text.lines[1].contains("hi"));
    }

    /// CJK participant names and message labels are twice as wide on screen
    /// as their character count, so lane placement and the figure's own
    /// `width` have to be sized in cells or a row overflows its own canvas
    /// and the next figure's lifelines misalign under it.
    #[test]
    fn cjk_participants_and_labels_stay_within_the_figures_own_width() {
        let figure = figure(
            "sequenceDiagram\n  participant 客户端\n  participant 服务器\n  客户端->>服务器: 请求登录",
        );
        for line in &figure.text.lines {
            assert!(
                line.width() <= usize::from(figure.text.width),
                "row {line:?} is {} cells wide, over the figure's own {} column budget",
                line.width(),
                figure.text.width
            );
        }
    }
}
