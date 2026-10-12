//! The lanes `git log --graph` draws beside history, one row per commit so a
//! row stays one entry: a merge or a fork draws its connector on the commit's
//! own row.

use std::collections::{HashMap, HashSet};

use crate::vcs::LogEntry;

/// A lane's identity, which keeps one colour from where it starts to where it
/// ends. `head` marks the lane walking HEAD's first-parent line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lane {
    pub id: usize,
    pub head: bool,
}

/// One lane's two columns on a row: its glyph, then the character joining it
/// to the next lane, each in its own lane's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub glyph: char,
    pub glyph_lane: Option<Lane>,
    pub fill: char,
    pub fill_lane: Option<Lane>,
}

#[derive(Clone)]
struct Slot {
    awaits: String,
    id: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Join {
    /// A lane ending here because it was waiting for this same commit.
    Ends,
    /// A merge's later parent that opens a new lane below.
    Opens,
    /// A merge's later parent already waited for in a lane of its own.
    Meets,
}

/// The graph cells for each entry, in `entries`' order, which must list a
/// commit before its parents. `head` is HEAD's full oid.
pub fn layout(entries: &[LogEntry], head: Option<&str>) -> Vec<Vec<Cell>> {
    let chain = first_parent_chain(entries, head);
    // Several lanes can wait for one commit on HEAD's line; we accent only the
    // lane HEAD's line itself walks down.
    let mut head_lane = None;
    let mut slots: Vec<Option<Slot>> = Vec::new();
    let mut next_id = 0;
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let col = slots
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|s| s.awaits == entry.oid))
            .or_else(|| slots.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                slots.push(None);
                slots.len() - 1
            });
        let id = slots.get(col).and_then(Option::as_ref).map_or_else(
            || {
                next_id += 1;
                next_id - 1
            },
            |slot| slot.id,
        );
        let own = Lane {
            id,
            head: chain.contains(entry.oid.as_str()),
        };
        let walked = head_lane;
        if own.head {
            head_lane = Some(id);
        }
        let lane = |slot: &Slot| Lane {
            id: slot.id,
            head: [walked, head_lane].contains(&Some(slot.id))
                && chain.contains(slot.awaits.as_str()),
        };
        let before = slots.clone();
        let mut joins: Vec<(usize, Join, Lane)> = Vec::new();
        for (index, slot) in slots.iter_mut().enumerate() {
            if index != col
                && let Some(waiting) = slot.take_if(|s| s.awaits == entry.oid)
            {
                joins.push((index, Join::Ends, lane(&waiting)));
            }
        }
        if let Some(own_slot) = slots.get_mut(col) {
            *own_slot = entry.parents.first().map(|parent| Slot {
                awaits: parent.clone(),
                id,
            });
        }
        for parent in entry.parents.iter().skip(1) {
            if let Some(index) = slots
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|s| &s.awaits == parent))
            {
                let met = slots.get(index).and_then(Option::as_ref).map(lane);
                joins.extend(met.map(|met| (index, Join::Meets, met)));
                continue;
            }
            // We skip a column whose lane ends on this row, so the row still draws that end.
            let index = (0..slots.len())
                .find(|&index| {
                    slots.get(index).is_some_and(Option::is_none)
                        && !joins.iter().any(|(joined, ..)| *joined == index)
                })
                .unwrap_or_else(|| {
                    slots.push(None);
                    slots.len() - 1
                });
            let slot = Slot {
                awaits: parent.clone(),
                id: next_id,
            };
            next_id += 1;
            joins.push((index, Join::Opens, lane(&slot)));
            if let Some(free) = slots.get_mut(index) {
                *free = Some(slot);
            }
        }
        rows.push(draw_row(col, own, &before, &joins, &lane));
        while slots.last().is_some_and(Option::is_none) {
            slots.pop();
        }
    }
    rows
}

fn draw_row(
    col: usize,
    own: Lane,
    before: &[Option<Slot>],
    joins: &[(usize, Join, Lane)],
    lane: &impl Fn(&Slot) -> Lane,
) -> Vec<Cell> {
    let width = joins
        .iter()
        .map(|(index, ..)| index + 1)
        .chain([before.len(), col + 1])
        .max()
        .unwrap_or(0);
    let run_crossing_gap_after = |index: usize| {
        joins.iter().find(|(target, ..)| {
            let (lo, hi) = (col.min(*target), col.max(*target));
            index >= lo && index < hi
        })
    };
    let passing = |index: usize| before.get(index).and_then(Option::as_ref).map(lane);
    (0..width)
        .map(|index| {
            let crossing = index
                .checked_sub(1)
                .and_then(run_crossing_gap_after)
                .filter(|_| run_crossing_gap_after(index).is_some());
            let (glyph, glyph_lane) = if index == col {
                ('●', Some(own))
            } else if let Some((_, join, joined)) = joins.iter().find(|(t, ..)| *t == index) {
                let right = index > col;
                // We draw a tee when the run continues to a farther join.
                let beyond = if right {
                    run_crossing_gap_after(index).is_some()
                } else {
                    index
                        .checked_sub(1)
                        .and_then(run_crossing_gap_after)
                        .is_some()
                };
                let glyph = match (join, right, beyond) {
                    (Join::Ends, _, true) => '┴',
                    (Join::Opens, _, true) => '┬',
                    (Join::Meets, _, true) => '┼',
                    (Join::Ends, true, false) => '╯',
                    (Join::Ends, false, false) => '╰',
                    (Join::Opens, true, false) => '╮',
                    (Join::Opens, false, false) => '╭',
                    (Join::Meets, true, false) => '┤',
                    (Join::Meets, false, false) => '├',
                };
                (glyph, Some(*joined))
            } else if let Some(through) = passing(index) {
                (if crossing.is_some() { '┼' } else { '│' }, Some(through))
            } else if let Some((.., run)) = crossing {
                ('─', Some(*run))
            } else {
                (' ', None)
            };
            let (fill, fill_lane) =
                run_crossing_gap_after(index).map_or((' ', None), |(.., run)| ('─', Some(*run)));
            Cell {
                glyph,
                glyph_lane,
                fill,
                fill_lane,
            }
        })
        .collect()
}

/// HEAD and its first parents, the line a reader means by "this branch".
fn first_parent_chain<'a>(entries: &'a [LogEntry], head: Option<&'a str>) -> HashSet<&'a str> {
    let parents: HashMap<&str, Option<&str>> = entries
        .iter()
        .map(|entry| {
            (
                entry.oid.as_str(),
                entry.parents.first().map(String::as_str),
            )
        })
        .collect();
    let mut chain = HashSet::new();
    let mut at = head;
    while let Some(oid) = at
        && chain.insert(oid)
    {
        at = parents.get(oid).copied().flatten();
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(oid: &str, parents: &[&str]) -> LogEntry {
        LogEntry {
            oid: oid.to_owned(),
            oid7: oid.to_owned(),
            refs: Vec::new(),
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            subject: String::new(),
            author: String::new(),
            time_unix: 0,
        }
    }

    fn text(rows: &[Vec<Cell>]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.iter()
                    .flat_map(|cell| [cell.glyph, cell.fill])
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn a_straight_history_is_one_lane() {
        let rows = layout(
            &[entry("c", &["b"]), entry("b", &["a"]), entry("a", &[])],
            Some("c"),
        );
        assert_eq!(text(&rows), ["●", "●", "●"]);
        assert!(
            rows.iter()
                .all(|row| row[0].glyph_lane.is_some_and(|l| l.head))
        );
    }

    #[test]
    fn a_merge_opens_a_lane_that_ends_at_the_fork() {
        // m merges f into main; f and b both come from a
        let rows = layout(
            &[
                entry("m", &["b", "f"]),
                entry("f", &["a"]),
                entry("b", &["a"]),
                entry("a", &[]),
            ],
            Some("m"),
        );
        assert_eq!(text(&rows), ["●─╮", "│ ●", "● │", "●─╯"]);
        let side = rows[1][1].glyph_lane.expect("the merged branch has a lane");
        assert!(!side.head, "only the first-parent line is HEAD's");
        assert!(rows[2][0].glyph_lane.is_some_and(|l| l.head));
    }

    #[test]
    fn two_branch_tips_share_their_fork() {
        let rows = layout(
            &[entry("x", &["a"]), entry("y", &["a"]), entry("a", &[])],
            Some("y"),
        );
        assert_eq!(text(&rows), ["●", "│ ●", "●─╯"]);
        assert_eq!(rows[0][0].glyph_lane.map(|l| l.head), Some(false));
        assert_eq!(
            rows[1][0].glyph_lane.map(|l| l.head),
            Some(false),
            "x's lane stays its own colour down to the fork"
        );
        assert_eq!(rows[2][1].glyph_lane.map(|l| l.head), Some(true));
    }

    #[test]
    fn a_branch_forked_from_a_merge_meets_it_while_the_merge_opens_a_lane() {
        let rows = layout(
            &[
                entry("x", &["a"]),
                entry("y", &["a"]),
                entry("a", &["b", "c"]),
                entry("c", &["b"]),
                entry("b", &[]),
            ],
            Some("y"),
        );
        assert_eq!(text(&rows), ["●", "│ ●", "●─┴─╮", "│   ●", "●───╯"]);
    }

    #[test]
    fn lanes_meeting_one_commit_close_into_it_in_one_run() {
        let rows = layout(
            &[
                entry("x", &["a"]),
                entry("y", &["a"]),
                entry("z", &["a"]),
                entry("a", &[]),
            ],
            None,
        );
        assert_eq!(text(&rows).last().map(String::as_str), Some("●─┴─╯"));
    }

    #[test]
    fn a_run_crossing_a_lane_draws_a_crossing() {
        // a closes lane 2 while lane 1 still waits for r
        let rows = layout(
            &[
                entry("x", &["a"]),
                entry("y", &["p"]),
                entry("z", &["a"]),
                entry("a", &[]),
                entry("p", &["r"]),
                entry("r", &[]),
            ],
            None,
        );
        assert_eq!(text(&rows), ["●", "│ ●", "│ │ ●", "●─┼─╯", "  ●", "  ●"]);
    }
}
