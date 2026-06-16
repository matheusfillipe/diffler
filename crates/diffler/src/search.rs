//! Vim-style `/` search shared by every pane. A pane exposes its rows through
//! [`Searchable`]; [`Search`] holds the query, the matches, and the active one,
//! and drives incremental highlight + `n`/`N` navigation. Matching is plain
//! substring with smartcase (case-insensitive unless the query has an
//! uppercase letter).

use std::ops::Range;

/// One match: a byte range within the text of row `row`. The row index is the
/// pane's own row index — whatever the pane uses to move its cursor and to key
/// its rendered rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub range: Range<usize>,
}

pub struct Search {
    query: String,
    /// Char index of the prompt's edit cursor.
    query_cursor: usize,
    /// The prompt is capturing input (vs committed, highlights persisting).
    pub open: bool,
    matches: Vec<Match>,
    /// Index into `matches` of the active match.
    current: usize,
    /// Cursor row when the search started, so the first match is picked
    /// relative to it and the cursor can be restored on cancel.
    origin_row: usize,
}

impl Search {
    pub fn start(origin_row: usize) -> Self {
        Self {
            query: String::new(),
            query_cursor: 0,
            open: true,
            matches: Vec::new(),
            current: 0,
            origin_row,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn query_cursor(&self) -> usize {
        self.query_cursor
    }

    pub fn origin_row(&self) -> usize {
        self.origin_row
    }

    /// `(1-based active, total)`, or `(0, 0)` when there are no matches.
    pub fn count(&self) -> (usize, usize) {
        if self.matches.is_empty() {
            (0, 0)
        } else {
            (self.current + 1, self.matches.len())
        }
    }

    pub fn insert(&mut self, c: char) {
        let byte = self
            .query
            .char_indices()
            .nth(self.query_cursor)
            .map_or(self.query.len(), |(i, _)| i);
        self.query.insert(byte, c);
        self.query_cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.query_cursor == 0 {
            return;
        }
        let prev = self.query_cursor - 1;
        if let Some((byte, c)) = self.query.char_indices().nth(prev) {
            self.query.drain(byte..byte + c.len_utf8());
            self.query_cursor = prev;
        }
    }

    pub fn move_left(&mut self) {
        self.query_cursor = self.query_cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.query_cursor = (self.query_cursor + 1).min(self.query.chars().count());
    }

    /// Recompute matches against `rows` and pick the active match as the first
    /// at or after `origin_row` (wrapping). Called as the query changes and on
    /// model refresh so highlights track live content.
    pub fn recompute(&mut self, rows: &[(usize, String)]) {
        self.matches = find_matches(rows, &self.query);
        self.current = self
            .matches
            .iter()
            .position(|m| m.row >= self.origin_row)
            .unwrap_or(0);
    }

    /// Close the prompt, keeping highlights. Returns the row of the active match
    /// to focus, if any.
    pub fn commit(&mut self) -> Option<usize> {
        self.open = false;
        self.active_row()
    }

    pub fn next_match(&mut self) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + 1) % self.matches.len();
        self.active_row()
    }

    pub fn prev_match(&mut self) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + self.matches.len() - 1) % self.matches.len();
        self.active_row()
    }

    fn active_row(&self) -> Option<usize> {
        self.matches.get(self.current).map(|m| m.row)
    }

    /// Row of the active match, for live preview while typing.
    pub fn current_row(&self) -> Option<usize> {
        self.active_row()
    }

    /// Match ranges on `row` paired with whether each is the active match,
    /// for the renderer to paint.
    pub fn ranges_for(&self, row: usize) -> Vec<(Range<usize>, bool)> {
        self.matches
            .iter()
            .enumerate()
            .filter(|(_, m)| m.row == row)
            .map(|(i, m)| (m.range.clone(), i == self.current))
            .collect()
    }
}

/// Plain-substring matches with smartcase: case-insensitive unless `query` has
/// an uppercase letter. Empty query matches nothing. Byte ranges index the
/// original row text (ASCII case-folding preserves byte length).
pub fn find_matches(rows: &[(usize, String)], query: &str) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let case_insensitive = !query.chars().any(char::is_uppercase);
    let needle = fold(query, case_insensitive);
    let mut out = Vec::new();
    for (row, text) in rows {
        let hay = fold(text, case_insensitive);
        let mut from = 0;
        while let Some(pos) = hay.get(from..).and_then(|h| h.find(&needle)) {
            let start = from + pos;
            let end = start + needle.len();
            out.push(Match {
                row: *row,
                range: start..end,
            });
            from = end.max(start + 1);
        }
    }
    out
}

fn fold(s: &str, case_insensitive: bool) -> String {
    if case_insensitive {
        s.to_ascii_lowercase()
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(items: &[(usize, &str)]) -> Vec<(usize, String)> {
        items.iter().map(|(r, t)| (*r, (*t).to_owned())).collect()
    }

    fn search(origin: usize, query: &str, rows: &[(usize, String)]) -> Search {
        let mut s = Search::start(origin);
        for c in query.chars() {
            s.insert(c);
        }
        s.recompute(rows);
        s
    }

    #[test]
    fn smartcase_is_insensitive_until_an_uppercase_appears() {
        let r = rows(&[(0, "Stall Width"), (1, "stallwidth")]);
        assert_eq!(find_matches(&r, "stall").len(), 2);
        assert_eq!(find_matches(&r, "Stall").len(), 1);
    }

    #[test]
    fn matches_carry_byte_ranges_in_the_original_text() {
        let r = rows(&[(7, "let width = 1;")]);
        let m = find_matches(&r, "width");
        assert_eq!(
            m,
            vec![Match {
                row: 7,
                range: 4..9
            }]
        );
    }

    #[test]
    fn multiple_matches_per_row_do_not_overlap() {
        let r = rows(&[(0, "aaa")]);
        let m = find_matches(&r, "aa");
        assert_eq!(m.len(), 1, "non-overlapping scan: {m:?}");
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(find_matches(&rows(&[(0, "anything")]), "").is_empty());
    }

    #[test]
    fn next_and_prev_wrap_and_track_the_active_row() {
        let r = rows(&[(0, "x"), (2, "x"), (5, "x")]);
        let mut s = search(0, "x", &r);
        assert_eq!(s.count(), (1, 3));
        assert_eq!(s.next_match(), Some(2));
        assert_eq!(s.next_match(), Some(5));
        assert_eq!(s.next_match(), Some(0), "wraps to the first");
        assert_eq!(s.prev_match(), Some(5), "wraps to the last");
    }

    #[test]
    fn active_match_is_the_first_at_or_after_the_origin() {
        let r = rows(&[(0, "x"), (4, "x"), (9, "x")]);
        let mut s = search(4, "x", &r);
        assert_eq!(s.commit(), Some(4));
    }

    #[test]
    fn ranges_for_flags_the_active_match() {
        let r = rows(&[(1, "foo foo")]);
        let s = search(0, "foo", &r);
        let ranges = s.ranges_for(1);
        assert_eq!(ranges.len(), 2);
        assert!(ranges[0].1, "first is active");
        assert!(!ranges[1].1);
    }
}
