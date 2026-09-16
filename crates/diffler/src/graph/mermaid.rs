//! Mermaid `flowchart` into a [`Model`], for graphs an agent writes.
//!
//! Best effort by design: agents reach for mermaid without being taught it, so
//! anything the layered engine cannot draw is simplified and reported rather
//! than refused. Only a diagram with no node-and-edge shape at all (a sequence
//! or class diagram) comes back as an error. The notes travel to the author, so
//! it learns what was dropped without the reader ever seeing a broken figure.

use crate::graph::model::{Edge, Model, Node, NodeId, NodeStatus, RankDir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
    pub model: Model,
    /// Node id paired with the anchor its `click` named, still unresolved: the
    /// text the agent wrote, not a line.
    pub anchors: Vec<(NodeId, String)>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MermaidError {
    #[error("{0} diagrams cannot be drawn; use `flowchart LR` or `flowchart TD`")]
    Unsupported(String),
    #[error("the diagram declares no nodes")]
    Empty,
}

/// Nodes one figure may hold. A graph past this is unreadable in a terminal
/// and its layout is what an agent-supplied body could otherwise spend the
/// main thread on.
pub const MAX_NODES: usize = 60;

/// Directives that only style, dropped whole.
const IGNORED: &[&str] = &["style", "classdef", "class", "linkstyle", "direction"];

pub fn parse(src: &str) -> Result<Figure, MermaidError> {
    let mut out = Parsed::default();
    let mut lines = src
        .lines()
        .map(strip_comment)
        .map(str::trim)
        .filter(|line| !line.is_empty());

    let header = lines.next().ok_or(MermaidError::Empty)?;
    out.model.rankdir = header_rankdir(header, &mut out.notes)?;

    for line in lines {
        out.statement(line);
    }
    if out.model.nodes.is_empty() {
        return Err(MermaidError::Empty);
    }
    out.settle_clicks();
    if out.model.nodes.len() > MAX_NODES {
        out.model.nodes.truncate(MAX_NODES);
        let known: Vec<NodeId> = out.model.nodes.iter().map(|n| n.id.clone()).collect();
        out.model
            .edges
            .retain(|edge| known.contains(&edge.from) && known.contains(&edge.to));
        out.anchors.retain(|(id, _)| known.contains(id));
        out.notes
            .push(format!("only the first {MAX_NODES} nodes are drawn"));
    }
    Ok(Figure {
        model: out.model,
        anchors: out.anchors,
        notes: out.notes,
    })
}

fn strip_comment(line: &str) -> &str {
    line.split_once("%%").map_or(line, |(head, _)| head)
}

fn header_rankdir(header: &str, notes: &mut Vec<String>) -> Result<RankDir, MermaidError> {
    let mut words = header.split_whitespace();
    let kind = words.next().unwrap_or_default().to_ascii_lowercase();
    // `flowchart-elk` and friends are the same language with another renderer
    let kind = kind.split('-').next().unwrap_or(&kind);
    if kind != "flowchart" && kind != "graph" {
        return Err(MermaidError::Unsupported(kind.to_owned()));
    }
    Ok(
        match words.next().unwrap_or("TD").to_ascii_uppercase().as_str() {
            "LR" => RankDir::LeftRight,
            "RL" => {
                notes.push("right-to-left is drawn left-to-right".to_owned());
                RankDir::LeftRight
            }
            "BT" => {
                notes.push("bottom-to-top is drawn top-to-bottom".to_owned());
                RankDir::TopDown
            }
            _ => RankDir::TopDown,
        },
    )
}

#[derive(Default)]
struct Parsed {
    model: Model,
    /// Collected as they are read and applied once the diagram is whole: a
    /// `click` may name a node the next line declares.
    clicks: Vec<(NodeId, String)>,
    anchors: Vec<(NodeId, String)>,
    notes: Vec<String>,
}

impl Parsed {
    fn note_once(&mut self, note: String) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    fn statement(&mut self, line: &str) {
        let head = line
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match head.as_str() {
            "end" => {}
            "subgraph" => self.note_once("subgraphs are flattened".to_owned()),
            "click" => self.click(line),
            other if IGNORED.contains(&other) => {
                self.note_once(format!("`{other}` is ignored"));
            }
            _ => self.chain(line),
        }
    }

    /// `click <id> "<target>"`, with mermaid's optional `href`/`call` keyword
    /// and trailing tooltip both skipped.
    fn click(&mut self, line: &str) {
        let mut words = line.split_whitespace().skip(1);
        let Some(id) = words.next() else { return };
        let rest = words.collect::<Vec<_>>().join(" ");
        let target = quoted(&rest).unwrap_or_else(|| {
            rest.split_whitespace()
                .find(|word| !matches!(*word, "href" | "call" | "callback"))
                .unwrap_or_default()
                .to_owned()
        });
        if target.is_empty() {
            return;
        }
        let id = NodeId::new(id);
        self.clicks.retain(|(known, _)| known != &id);
        self.clicks.push((id, target));
    }

    fn settle_clicks(&mut self) {
        for (id, target) in std::mem::take(&mut self.clicks) {
            if self.model.index_of(&id).is_none() {
                self.note_once(format!("`click {}` names no node in the diagram", id.0));
                continue;
            }
            self.anchors.push((id, target));
        }
    }

    /// One `A --> B --> C` line: alternating node specs and links, where a
    /// text run between an open link and a closed one is the edge's label.
    fn chain(&mut self, line: &str) {
        let segments = split_links(line);
        let mut pending: Option<NodeId> = None;
        let mut label: Option<String> = None;
        let mut open = false;
        for segment in segments {
            match segment {
                Segment::Link(arrow) => {
                    if !arrow.closed {
                        open = true;
                    }
                    if let Some(text) = arrow.label {
                        label = Some(text);
                    }
                    if arrow.closed {
                        open = false;
                    }
                    self.note_link(&arrow.raw);
                }
                Segment::Text(text) if open && label.is_none() => label = Some(text),
                Segment::Text(text) => {
                    let id = self.declare(&text);
                    if let (Some(from), Some(to)) = (pending.clone(), id.clone()) {
                        self.model.edges.push(Edge {
                            from,
                            to,
                            label: label.take(),
                        });
                    }
                    label = None;
                    if id.is_some() {
                        pending = id;
                    }
                }
            }
        }
    }

    fn note_link(&mut self, raw: &str) {
        if raw.contains('.') {
            self.note_once("dotted links are drawn solid".to_owned());
        } else if raw.contains('=') {
            self.note_once("thick links are drawn solid".to_owned());
        }
    }

    /// Add the node a spec names, keeping the first label seen for it. Returns
    /// `None` for a spec that holds no id at all.
    fn declare(&mut self, spec: &str) -> Option<NodeId> {
        let (id, label, shape) = node_spec(spec)?;
        if let Some(shape) = shape {
            self.note_once(format!("`{shape}` shapes are drawn as boxes"));
        }
        let id = NodeId::new(id);
        match self.model.index_of(&id) {
            Some(at) => {
                if let (Some(label), Some(node)) = (label, self.model.nodes.get_mut(at))
                    && node.label == node.id.0
                {
                    node.label = label;
                }
            }
            None => self.model.nodes.push(Node {
                label: label.unwrap_or_else(|| id.0.clone()),
                id: id.clone(),
                status: NodeStatus::Neutral,
                group: None,
                foldable: None,
            }),
        }
        Some(id)
    }
}

/// `id`, `id[label]`, `id(label)`, `id{label}`, `id((label))`, `id>label]`.
/// The third field names the shape when it is not a plain box.
fn node_spec(spec: &str) -> Option<(String, Option<String>, Option<&'static str>)> {
    let spec = spec.trim();
    // a bare id, or a spec that opens with a bracket and so names nothing
    let Some(open) = spec.find(['[', '(', '{', '>']).filter(|at| *at > 0) else {
        return (!spec.is_empty()).then(|| (spec.to_owned(), None, None));
    };
    let (id, rest) = spec.split_at(open);
    let shape = match rest.as_bytes().first() {
        Some(b'[') if rest.starts_with("[[") => Some("[[ ]]"),
        Some(b'[') if rest.starts_with("[(") => Some("[( )]"),
        Some(b'[') => None,
        Some(b'(') if rest.starts_with("((") => Some("(( ))"),
        Some(b'(') => Some("( )"),
        Some(b'{') if rest.starts_with("{{") => Some("{{ }}"),
        Some(b'{') => Some("{ }"),
        _ => Some("> ]"),
    };
    let inner = rest
        .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '{' | '}' | '>'))
        .trim();
    let label = quoted(inner).unwrap_or_else(|| inner.to_owned());
    let label = wrap_label(&break_lines(&label));
    Some((
        id.trim().to_owned(),
        (!label.is_empty()).then_some(label),
        shape,
    ))
}

/// Columns a node label's line may run before it wraps. At an 80-column
/// terminal, the narrowest a card realistically gets, the figure's own
/// drawing area is ~46 columns (`card_budget` minus the card's frame); a
/// line past this cap would alone force a box wider than that, so it wraps
/// on a word boundary and keeps the whole figure at its own width.
const MAX_LABEL_LINE: usize = 32;

/// Wrap every line of `label` on word boundaries to [`MAX_LABEL_LINE`]
/// columns, joining wrapped lines back with `\n` alongside any the label
/// already had from [`break_lines`]. A single word longer than the cap stays
/// whole on its own line.
fn wrap_label(label: &str) -> String {
    label
        .split('\n')
        .map(|line| wrap_line(line, MAX_LABEL_LINE).join("\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_line(line: &str, cap: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > cap {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Mermaid's own line break inside a label, `<br>`/`<br/>`/`<br />`, any case
/// and any inner spacing, becomes the `\n` a node box draws as another line.
fn break_lines(label: &str) -> String {
    let chars: Vec<char> = label.chars().collect();
    let mut out = String::with_capacity(label.len());
    let mut at = 0;
    while let Some(&ch) = chars.get(at) {
        if let Some(end) = br_tag_end(&chars, at) {
            out.push('\n');
            at = end;
        } else {
            out.push(ch);
            at += 1;
        }
    }
    out
}

/// The index just past a `<br...>` tag starting at `at`, if there is one.
fn br_tag_end(chars: &[char], at: usize) -> Option<usize> {
    let is = |i: usize, c: char| chars.get(i).is_some_and(|x| x.eq_ignore_ascii_case(&c));
    if !is(at, '<') || !is(at + 1, 'b') || !is(at + 2, 'r') {
        return None;
    }
    let mut end = at + 3;
    let skip_space = |end: &mut usize| {
        while chars.get(*end).is_some_and(|c| c.is_whitespace()) {
            *end += 1;
        }
    };
    skip_space(&mut end);
    if chars.get(end) == Some(&'/') {
        end += 1;
        skip_space(&mut end);
    }
    (chars.get(end) == Some(&'>')).then_some(end + 1)
}

/// The first double-quoted run, wherever it sits: mermaid puts an optional
/// `href`/`call` keyword before a `click` target.
fn quoted(text: &str) -> Option<String> {
    let (_, rest) = text.split_once('"')?;
    let (inside, _) = rest.split_once('"')?;
    Some(inside.to_owned())
}

#[derive(Debug, PartialEq, Eq)]
enum Segment {
    Text(String),
    Link(Link),
}

#[derive(Debug, PartialEq, Eq)]
struct Link {
    raw: String,
    /// An arrowhead: `-->` connects, `--` opens a labelled link that the next
    /// link closes.
    closed: bool,
    label: Option<String>,
}

/// Split a statement into node specs and the links between them. Bracket and
/// quote depth is tracked so a hyphen inside a label is not read as a link.
fn split_links(line: &str) -> Vec<Segment> {
    let chars: Vec<char> = line.chars().collect();
    let mut segments = Vec::new();
    let mut text = String::new();
    let mut depth = 0usize;
    let mut quote = false;
    let mut at = 0usize;
    while let Some(&c) = chars.get(at) {
        if c == '"' {
            quote = !quote;
        }
        if !quote {
            match c {
                '[' | '(' | '{' => depth += 1,
                ']' | ')' | '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if !quote
            && depth == 0
            && matches!(c, '-' | '=')
            && let Some((link, next)) = link_at(&chars, at)
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                segments.push(Segment::Text(trimmed.to_owned()));
            }
            text.clear();
            segments.push(Segment::Link(link));
            at = next;
            continue;
        }
        text.push(c);
        at += 1;
    }
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        segments.push(Segment::Text(trimmed.to_owned()));
    }
    segments
}

/// The link starting at `at`, and where it ends. `--` alone is a link only
/// when a second dash follows, so a hyphenated bare id stays one token.
fn link_at(chars: &[char], at: usize) -> Option<(Link, usize)> {
    let mut end = at;
    while matches!(chars.get(end), Some('-' | '.' | '=' | '~')) {
        end += 1;
    }
    if end - at < 2 {
        return None;
    }
    let mut closed = false;
    match chars.get(end) {
        Some('>') => {
            closed = true;
            end += 1;
        }
        // `--x` and `--o` are arrowheads only when a separator follows; an
        // identifier may legitimately start with either letter
        Some(&c @ ('x' | 'o'))
            if chars
                .get(end + 1)
                .is_none_or(|next| next.is_whitespace() || *next == '|') =>
        {
            let _ = c;
            closed = true;
            end += 1;
        }
        _ => {}
    }
    let raw: String = chars.get(at..end)?.iter().collect();
    let (label, end) = inline_label(chars, end);
    Some((Link { raw, closed, label }, end))
}

/// `-->|text|`: the label mermaid puts straight after the arrow.
fn inline_label(chars: &[char], at: usize) -> (Option<String>, usize) {
    let mut cursor = at;
    while matches!(chars.get(cursor), Some(c) if c.is_whitespace()) {
        cursor += 1;
    }
    if chars.get(cursor) != Some(&'|') {
        return (None, at);
    }
    cursor += 1;
    let mut label = String::new();
    while let Some(&c) = chars.get(cursor) {
        cursor += 1;
        if c == '|' {
            let label = quoted(&label).unwrap_or_else(|| label.trim().to_owned());
            return ((!label.is_empty()).then_some(label), cursor);
        }
        label.push(c);
    }
    (None, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn figure(src: &str) -> Figure {
        parse(src).expect("parsed")
    }

    fn edges(figure: &Figure) -> Vec<(String, String, Option<String>)> {
        figure
            .model
            .edges
            .iter()
            .map(|e| (e.from.0.clone(), e.to.0.clone(), e.label.clone()))
            .collect()
    }

    fn labels(figure: &Figure) -> Vec<String> {
        figure.model.nodes.iter().map(|n| n.label.clone()).collect()
    }

    #[test]
    fn a_plain_flowchart_becomes_nodes_and_edges() {
        let figure = figure(
            "flowchart LR\n  load[load defaults] --> user[user config]\n  user --> repo[repo config]",
        );
        assert_eq!(figure.model.rankdir, RankDir::LeftRight);
        assert_eq!(
            labels(&figure),
            ["load defaults", "user config", "repo config"]
        );
        assert_eq!(
            edges(&figure),
            [
                ("load".to_owned(), "user".to_owned(), None),
                ("user".to_owned(), "repo".to_owned(), None),
            ]
        );
        assert!(figure.notes.is_empty(), "{:?}", figure.notes);
    }

    #[test]
    fn a_chain_on_one_line_links_every_pair() {
        let figure = figure("flowchart TD\n  a --> b --> c");
        assert_eq!(
            edges(&figure),
            [
                ("a".to_owned(), "b".to_owned(), None),
                ("b".to_owned(), "c".to_owned(), None),
            ]
        );
    }

    #[test]
    fn both_label_forms_land_on_the_edge() {
        let inline = figure("flowchart LR\n  a -->|hands the partial| b");
        let middle = figure("flowchart LR\n  a -- hands the partial --> b");
        for figure in [&inline, &middle] {
            assert_eq!(
                edges(figure),
                [(
                    "a".to_owned(),
                    "b".to_owned(),
                    Some("hands the partial".to_owned())
                )]
            );
            assert_eq!(labels(figure), ["a", "b"]);
        }
    }

    /// A hyphen inside a label or an id is not a link; splitting on it would
    /// invent nodes out of half a word.
    #[test]
    fn a_hyphen_inside_a_label_is_not_a_link() {
        let figure = figure("flowchart LR\n  merge-cfg[merge the well-known keys] --> out");
        assert_eq!(labels(&figure), ["merge the well-known keys", "out"]);
        assert_eq!(
            edges(&figure),
            [("merge-cfg".to_owned(), "out".to_owned(), None)]
        );
    }

    #[test]
    fn click_attaches_an_anchor_to_its_node() {
        let figure = figure(
            "flowchart LR\n  a[load] --> b[merge]\n  click a \"src/config.rs#defaults\"\n  click b href \"src/config.rs:88\"",
        );
        assert_eq!(
            figure.anchors,
            [
                (NodeId::new("a"), "src/config.rs#defaults".to_owned()),
                (NodeId::new("b"), "src/config.rs:88".to_owned()),
            ]
        );
    }

    #[test]
    fn a_click_naming_no_node_is_a_note_not_an_anchor() {
        let figure = figure("flowchart LR\n  a --> b\n  click zz \"src/lib.rs\"");
        assert!(figure.anchors.is_empty());
        assert_eq!(figure.notes, ["`click zz` names no node in the diagram"]);
    }

    #[test]
    fn shapes_we_cannot_draw_become_boxes_and_say_so() {
        let figure = figure("flowchart TD\n  a{is it set?} --> b((done))");
        assert_eq!(labels(&figure), ["is it set?", "done"]);
        assert_eq!(
            figure.notes,
            [
                "`{ }` shapes are drawn as boxes",
                "`(( ))` shapes are drawn as boxes"
            ]
        );
    }

    #[test]
    fn styling_directives_are_dropped_with_one_note_each() {
        let figure = figure(
            "flowchart LR\n  a --> b\n  style a fill:#f00\n  style b fill:#0f0\n  classDef big font-size:20px",
        );
        assert_eq!(edges(&figure).len(), 1);
        assert_eq!(
            figure.notes,
            ["`style` is ignored", "`classdef` is ignored"]
        );
    }

    #[test]
    fn a_subgraph_flattens_and_keeps_its_members() {
        let figure = figure(
            "flowchart TD\n  subgraph parse\n    a[read] --> b[lex]\n  end\n  b --> c[emit]",
        );
        assert_eq!(labels(&figure), ["read", "lex", "emit"]);
        assert_eq!(edges(&figure).len(), 2);
        assert_eq!(figure.notes, ["subgraphs are flattened"]);
    }

    #[test]
    fn dotted_and_thick_links_draw_solid() {
        let figure = figure("flowchart LR\n  a -.-> b\n  b ==> c");
        assert_eq!(edges(&figure).len(), 2);
        assert_eq!(
            figure.notes,
            [
                "dotted links are drawn solid",
                "thick links are drawn solid"
            ]
        );
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let figure = figure("flowchart LR\n%% the layering\n\n  a --> b %% trailing\n");
        assert_eq!(edges(&figure).len(), 1);
        assert_eq!(labels(&figure), ["a", "b"]);
    }

    /// Mermaid lets a node appear in an edge before it is labelled, so a later
    /// declaration fills the label in; a label already given is not replaced.
    #[test]
    fn a_later_declaration_supplies_a_missing_label() {
        let figure = figure("flowchart LR\n  a --> b[merge]\n  b --> c\n  a[load]\n  b[other]");
        assert_eq!(labels(&figure), ["load", "merge", "c"]);
    }

    /// Mermaid's own line break inside a label becomes `\n`, in any of its
    /// three spellings and any case, so the node box draws it as a second
    /// line.
    #[test]
    fn a_br_tag_becomes_a_line_break() {
        let figure = figure(
            "flowchart LR\n  a[one<br>two] --> b[three<br/>four] --> c[five<br />six] --> d[seven<BR/>eight]",
        );
        assert_eq!(
            labels(&figure),
            ["one\ntwo", "three\nfour", "five\nsix", "seven\neight"]
        );
    }

    /// A label with no `<br>` at all passes through untouched.
    #[test]
    fn a_label_without_a_break_is_unchanged() {
        let figure = figure("flowchart LR\n  a[plain label] --> b");
        assert_eq!(labels(&figure), ["plain label", "b"]);
    }

    /// A label line past the cap wraps on word boundaries into further lines,
    /// so a long sentence grows the box taller.
    #[test]
    fn a_long_label_wraps_on_word_boundaries() {
        let figure = figure(
            "flowchart LR\n  a[create, edit, and view the ai visualization perspective] --> b",
        );
        let label = &labels(&figure)[0];
        let lines: Vec<&str> = label.split('\n').collect();
        assert!(lines.len() > 1, "{label:?}");
        for line in &lines {
            assert!(line.chars().count() <= MAX_LABEL_LINE, "{line:?}");
        }
        assert_eq!(
            lines.join(" "),
            "create, edit, and view the ai visualization perspective"
        );
    }

    /// A single word longer than the cap is left whole on its own line.
    #[test]
    fn a_word_longer_than_the_cap_is_not_broken() {
        let long_word = "a".repeat(MAX_LABEL_LINE + 10);
        let figure = figure(&format!("flowchart LR\n  a[{long_word}] --> b"));
        assert_eq!(labels(&figure)[0], long_word);
    }

    #[test]
    fn a_diagram_we_cannot_draw_is_an_error_the_agent_can_act_on() {
        let err = parse("sequenceDiagram\n  alice->>bob: hi").expect_err("refused");
        assert_eq!(err, MermaidError::Unsupported("sequencediagram".to_owned()));
        assert!(err.to_string().contains("flowchart LR"), "{err}");
    }

    #[test]
    fn an_empty_diagram_is_an_error() {
        assert_eq!(
            parse("flowchart LR").expect_err("refused"),
            MermaidError::Empty
        );
        assert_eq!(parse("").expect_err("refused"), MermaidError::Empty);
    }

    #[test]
    fn graph_is_accepted_as_the_older_spelling() {
        let figure = figure("graph LR\n  a --> b");
        assert_eq!(figure.model.rankdir, RankDir::LeftRight);
        assert_eq!(edges(&figure).len(), 1);
    }

    #[test]
    fn a_direction_we_do_not_reverse_says_so() {
        let figure = figure("flowchart RL\n  a --> b");
        assert_eq!(figure.model.rankdir, RankDir::LeftRight);
        assert_eq!(figure.notes, ["right-to-left is drawn left-to-right"]);
    }
}

#[cfg(test)]
mod more_tests {
    use std::fmt::Write as _;

    use super::*;

    /// A `click` may precede the line that declares its node, the way a label
    /// may arrive after an edge already used the id.
    #[test]
    fn a_click_before_its_node_still_attaches() {
        let figure =
            parse("flowchart LR\n  click a \"src/lib.rs\"\n  a[load] --> b").expect("parsed");
        assert_eq!(
            figure.anchors,
            [(NodeId::new("a"), "src/lib.rs".to_owned())]
        );
        assert!(figure.notes.is_empty(), "{:?}", figure.notes);
    }

    #[test]
    fn a_graph_past_the_node_cap_is_truncated_and_says_so() {
        let mut src = String::from("flowchart LR\n");
        for index in 0..(MAX_NODES + 10) {
            let _ = writeln!(src, "  n{index}[node {index}]");
        }
        let _ = writeln!(src, "  n0 --> n{}", MAX_NODES + 5);
        let figure = parse(&src).expect("parsed");
        assert_eq!(figure.model.nodes.len(), MAX_NODES);
        assert!(
            figure.model.edges.is_empty(),
            "an edge into a dropped node goes with it"
        );
        assert_eq!(
            figure.notes,
            [format!("only the first {MAX_NODES} nodes are drawn")]
        );
    }
}
