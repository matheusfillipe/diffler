//! The diff screen's side panes and the dividers between them, which a mouse
//! drag moves.

const MIN_SIDE_WIDTH: u16 = 16;
const MIN_DIFF_WIDTH: u16 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Divider {
    Left,
    Right,
}

/// A width the reader dragged a side pane to; `None` keeps the automatic one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PaneWidths {
    pub(crate) left: Option<u16>,
    pub(crate) right: Option<u16>,
}

impl PaneWidths {
    /// The file sidebar's width without its rail column.
    pub(crate) fn sidebar(self, total: u16, right_open: bool) -> u16 {
        self.widths(total, right_open).0
    }

    pub(crate) fn comments(self, total: u16, open: bool) -> u16 {
        self.widths(total, open).1
    }

    /// The diff pane's width between the two side panes and their gaps.
    pub(crate) fn diff(self, total: u16, right_open: bool) -> u16 {
        let (side, right) = self.widths(total, right_open);
        total.saturating_sub(side + right + gaps(right))
    }

    /// The divider a press at `col` grabs: the gap itself, or the column next
    /// to it on the diff's side, so the panes keep every column of their own.
    pub(crate) fn divider_at(self, total: u16, col: u16, right_open: bool) -> Option<Divider> {
        let (side, right) = self.widths(total, right_open);
        let left_gap = side + 1;
        let right_gap = total.saturating_sub(right + 1);
        if right > 0 && (col == right_gap || col + 1 == right_gap) {
            return Some(Divider::Right);
        }
        (col == left_gap || col == left_gap + 1).then_some(Divider::Left)
    }

    /// Puts the divider under `col`, stopping where the diff would drop below
    /// its minimum.
    pub(crate) fn drag(&mut self, divider: Divider, total: u16, col: u16, right_open: bool) {
        let (side, right) = self.widths(total, right_open);
        let room = total.saturating_sub(MIN_DIFF_WIDTH + gaps(right));
        match divider {
            Divider::Left => {
                let most = room.saturating_sub(right);
                self.left = Some(col.saturating_sub(1).min(most).max(MIN_SIDE_WIDTH));
            }
            Divider::Right => {
                let most = room.saturating_sub(side);
                let width = total.saturating_sub(col + 1);
                self.right = Some(width.min(most).max(MIN_SIDE_WIDTH));
            }
        }
    }

    pub(crate) fn reset(&mut self, divider: Divider) {
        match divider {
            Divider::Left => self.left = None,
            Divider::Right => self.right = None,
        }
    }

    /// Both side widths. A terminal that shrank since the drag gives the diff
    /// its room back from the dragged panes first, down to their minimum.
    fn widths(self, total: u16, right_open: bool) -> (u16, u16) {
        let auto = (total / 4).clamp(28, 44).min(total);
        let mut side = self.left.unwrap_or(auto);
        let mut right = match (right_open, self.right) {
            (false, _) => 0,
            (true, Some(dragged)) => dragged,
            (true, None) => auto.min(total / 3),
        };
        let mut over = (side + right + gaps(right) + MIN_DIFF_WIDTH).saturating_sub(total);
        if self.right.is_some() && right > 0 {
            let cut = over.min(right.saturating_sub(MIN_SIDE_WIDTH));
            right -= cut;
            over -= cut;
        }
        if self.left.is_some() {
            side -= over.min(side.saturating_sub(MIN_SIDE_WIDTH));
        }
        (side, right)
    }
}

/// The sidebar's rail column and its gap, plus the right pane's gap when open.
const fn gaps(right: u16) -> u16 {
    if right == 0 { 2 } else { 3 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_a_divider_moves_it_and_keeps_the_diff_readable() {
        let mut widths = PaneWidths::default();
        let total = 160;
        let auto = widths.sidebar(total, true);
        assert_eq!(
            widths.divider_at(total, auto + 1, true),
            Some(Divider::Left)
        );
        assert_eq!(
            widths.divider_at(total, auto, true),
            None,
            "the sidebar keeps its last column"
        );
        widths.drag(Divider::Left, total, 60, true);
        assert_eq!(widths.sidebar(total, true), 59);

        let right_gap = total - widths.comments(total, true) - 1;
        assert_eq!(
            widths.divider_at(total, right_gap, true),
            Some(Divider::Right)
        );
        widths.drag(Divider::Right, total, 110, true);
        assert_eq!(widths.comments(total, true), 49);
        assert!(widths.diff(total, true) >= MIN_DIFF_WIDTH);

        widths.drag(Divider::Left, total, 150, true);
        assert!(
            widths.diff(total, true) >= MIN_DIFF_WIDTH,
            "the diff keeps its minimum"
        );
        widths.reset(Divider::Left);
        assert_eq!(widths.sidebar(total, false), auto);
    }

    #[test]
    fn dragging_only_the_right_divider_stops_at_the_diff_minimum() {
        for total in [120, 160, 200] {
            let mut widths = PaneWidths::default();
            widths.drag(Divider::Right, total, 1, true);
            assert_eq!(
                widths.diff(total, true),
                MIN_DIFF_WIDTH,
                "at {total} columns"
            );
        }
    }

    #[test]
    fn a_shrunk_terminal_squeezes_dragged_panes_but_not_below_their_minimum() {
        let mut widths = PaneWidths::default();
        widths.drag(Divider::Left, 200, 100, false);
        assert_eq!(widths.sidebar(80, true), MIN_SIDE_WIDTH);
    }
}
