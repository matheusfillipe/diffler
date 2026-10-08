//! fzf-style list state shared by the command palette and the pick-one
//! dialogs. Two focuses, toggled with Tab: the list keeps every classic
//! single-key shortcut (j/k, g/G, the dialog's own keys), the input turns
//! printable keys into a filter whose best match the selection follows.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use super::byte_index;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FuzzyFocus {
    /// Keys navigate and hit the dialog's own shortcuts.
    #[default]
    List,
    /// Keys type into the filter query.
    Input,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FuzzyList {
    pub focus: FuzzyFocus,
    pub query: String,
    /// Character index into `query`.
    pub cursor: usize,
    /// Index into `matches`.
    pub selected: usize,
    /// Ranked haystack indices, best first: the one place the ranking is
    /// computed (on open and on each edit), read by handler and renderer.
    pub matches: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FuzzyKey {
    Submit,
    Cancel,
    /// Query changed; the caller re-ranks and the selection reset to the top.
    Edited,
    Consumed,
    /// Not a list/input key: the dialog's own shortcuts get their turn.
    Other,
}

impl FuzzyList {
    pub(crate) fn typing() -> Self {
        Self {
            focus: FuzzyFocus::Input,
            ..Self::default()
        }
    }

    pub(crate) fn feed(&mut self, key: &KeyEvent) -> FuzzyKey {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return FuzzyKey::Cancel,
            KeyCode::Enter => return FuzzyKey::Submit,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    FuzzyFocus::List => FuzzyFocus::Input,
                    FuzzyFocus::Input => FuzzyFocus::List,
                };
                return FuzzyKey::Consumed;
            }
            KeyCode::Down => {
                self.step(true);
                return FuzzyKey::Consumed;
            }
            KeyCode::Up => {
                self.step(false);
                return FuzzyKey::Consumed;
            }
            KeyCode::Char('n') if ctrl => {
                self.step(true);
                return FuzzyKey::Consumed;
            }
            KeyCode::Char('p') if ctrl => {
                self.step(false);
                return FuzzyKey::Consumed;
            }
            _ => {}
        }
        match self.focus {
            FuzzyFocus::List => self.feed_list(key),
            FuzzyFocus::Input => self.feed_input(key, ctrl),
        }
    }

    fn feed_list(&mut self, key: &KeyEvent) -> FuzzyKey {
        match key.code {
            KeyCode::Char('j') => self.step(true),
            KeyCode::Char('k') => self.step(false),
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => self.selected = self.matches.len().saturating_sub(1),
            KeyCode::Char('q') => return FuzzyKey::Cancel,
            _ => return FuzzyKey::Other,
        }
        FuzzyKey::Consumed
    }

    fn feed_input(&mut self, key: &KeyEvent, ctrl: bool) -> FuzzyKey {
        match key.code {
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let start = byte_index(&self.query, self.cursor - 1);
                    let end = byte_index(&self.query, self.cursor);
                    self.query.replace_range(start..end, "");
                    self.cursor -= 1;
                    self.selected = 0;
                }
                FuzzyKey::Edited
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.cursor = 0;
                self.selected = 0;
                FuzzyKey::Edited
            }
            KeyCode::Char('a') if ctrl => {
                self.cursor = 0;
                FuzzyKey::Consumed
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = self.query.chars().count();
                FuzzyKey::Consumed
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                FuzzyKey::Consumed
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.query.chars().count());
                FuzzyKey::Consumed
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let at = byte_index(&self.query, self.cursor);
                self.query.insert(at, c);
                self.cursor += 1;
                self.selected = 0;
                FuzzyKey::Edited
            }
            _ => FuzzyKey::Other,
        }
    }

    /// Move the selection one row, for a caller that is not a key press.
    pub(crate) fn step_selection(&mut self, forward: bool) {
        self.step(forward);
    }

    fn step(&mut self, forward: bool) {
        let len = self.matches.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % len
        } else {
            (self.selected + len - 1) % len
        };
    }

    /// Recompute the ranking; call on open and after every edit. Keeps the
    /// selection when it survives the new match set.
    pub(crate) fn rerank(&mut self, haystacks: &[String]) {
        self.matches = rank(&self.query, haystacks);
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    /// [`Self::rerank`] matching from word starts, for a list of prose labels.
    pub(crate) fn rerank_words(&mut self, haystacks: &[String]) {
        self.matches = rank_words(&self.query, haystacks);
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }
}

/// Indices of `haystacks` matching `query`, best first; everything in
/// original order when the query is empty.
pub(crate) fn rank(query: &str, haystacks: &[String]) -> Vec<usize> {
    if query.is_empty() {
        return (0..haystacks.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(index, hay)| {
            let hay = nucleo_matcher::Utf32Str::new(hay, &mut buf);
            pattern.score(hay, &mut matcher).map(|score| (score, index))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
}

/// [`rank`] for prose labels: a label matches only when every query letter
/// follows the previous one directly or opens a word of the label, so a
/// query finds the words it spells and never letters scattered across them.
pub(crate) fn rank_words(query: &str, haystacks: &[String]) -> Vec<usize> {
    rank(query, haystacks)
        .into_iter()
        .filter(|&index| {
            haystacks
                .get(index)
                .is_some_and(|hay| matches_word_starts(query, hay))
        })
        .collect()
}

/// Whether `query` spells `hay` from word starts: "amend" finds "amend the
/// last commit" and "rlc" finds "reword last commit". A space in the query
/// makes the next letter open a word.
fn matches_word_starts(query: &str, hay: &str) -> bool {
    let hay: Vec<char> = hay.to_lowercase().chars().collect();
    let mut letters = Vec::new();
    let mut after_space = true;
    for ch in query.to_lowercase().chars() {
        if ch.is_whitespace() {
            after_space = true;
        } else {
            letters.push((ch, after_space));
            after_space = false;
        }
    }
    let opens_word =
        |at: usize| at == 0 || hay.get(at - 1).is_some_and(|prev| !prev.is_alphanumeric());
    let mut failed = std::collections::HashSet::new();
    spell(&letters, &hay, 0, None, &opens_word, &mut failed)
}

/// Whether `letters[next..]` can be matched in `hay` after position `prev`.
fn spell(
    letters: &[(char, bool)],
    hay: &[char],
    next: usize,
    prev: Option<usize>,
    opens_word: &dyn Fn(usize) -> bool,
    failed: &mut std::collections::HashSet<(usize, Option<usize>)>,
) -> bool {
    let Some(&(letter, must_open)) = letters.get(next) else {
        return true;
    };
    if failed.contains(&(next, prev)) {
        return false;
    }
    let from = prev.map_or(0, |at| at + 1);
    for (at, &ch) in hay.iter().enumerate().skip(from) {
        let follows = prev.is_some_and(|p| p + 1 == at) && !must_open;
        if ch == letter
            && (follows || opens_word(at))
            && spell(letters, hay, next + 1, Some(at), opens_word, failed)
        {
            return true;
        }
    }
    failed.insert((next, prev));
    false
}

/// The item the list's selection points at, through the current ranking.
pub(crate) fn selected<'a, T>(list: &FuzzyList, items: &'a [T]) -> Option<&'a T> {
    list.matches
        .get(list.selected)
        .and_then(|index| items.get(*index))
}

pub(crate) fn branch_haystack(branches: &[diffler_core::vcs::BranchInfo]) -> Vec<String> {
    branches.iter().map(|b| b.name.clone()).collect()
}

pub(crate) fn name_haystack(names: &[String]) -> Vec<String> {
    names.to_vec()
}

pub(crate) fn rev_haystack(entries: &[super::RevChoice]) -> Vec<String> {
    entries.iter().map(|e| e.label.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hay(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn empty_query_keeps_original_order() {
        assert_eq!(rank("", &hay(&["b", "a"])), vec![0, 1]);
    }

    #[test]
    fn subsequence_matches_and_ranks_tighter_hits_first() {
        let items = hay(&["pull from upstream", "push to upstream", "unstage all"]);
        let ranked = rank("pus", &items);
        assert_eq!(ranked.first(), Some(&1));
        assert!(!ranked.contains(&2) || ranked[0] == 1);
    }

    #[test]
    fn a_label_matches_only_from_word_starts() {
        let items = hay(&[
            "amend the last commit and edit its message",
            "review all uncommitted changes",
            "blame this file: see who last changed each line",
            "reword the last commit's message",
        ]);
        assert_eq!(rank_words("amend", &items), vec![0]);
        assert_eq!(rank_words("rlc", &items), vec![3], "initials of words");
        assert_eq!(rank_words("last com", &items).len(), 2);
        assert!(rank_words("xyz", &items).is_empty());
    }

    #[test]
    fn non_matching_items_drop_out() {
        let items = hay(&["stage file", "quit"]);
        assert_eq!(rank("stg", &items), vec![0]);
    }

    #[test]
    fn list_focus_keeps_classic_keys_and_tab_switches_to_typing() {
        let items = hay(&["stage file", "stash", "search"]);
        let mut list = FuzzyList::default();
        list.rerank(&items);
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // list focus: j/k move, other printables fall through to the dialog
        list.feed(&press(KeyCode::Char('j')));
        assert_eq!(list.selected, 1);
        list.feed(&press(KeyCode::Char('k')));
        assert_eq!(list.selected, 0);
        assert_eq!(list.feed(&press(KeyCode::Char('d'))), FuzzyKey::Other);
        assert!(list.query.is_empty(), "list focus never types");
        assert_eq!(list.feed(&press(KeyCode::Char('q'))), FuzzyKey::Cancel);

        // tab into the input: printables filter, the selection rides the top
        list.feed(&press(KeyCode::Tab));
        assert_eq!(list.focus, FuzzyFocus::Input);
        assert_eq!(list.feed(&press(KeyCode::Char('s'))), FuzzyKey::Edited);
        list.rerank(&items);
        list.feed(&press(KeyCode::Char('t')));
        list.rerank(&items);
        assert_eq!(list.query, "st");
        assert_eq!(list.matches.len(), 2, "search drops out of 'st'");
        list.feed(&press(KeyCode::Down));
        assert_eq!(list.selected, 1);
        list.feed(&press(KeyCode::Down));
        assert_eq!(list.selected, 0, "selection wraps");
        // tab back out: classic keys again
        list.feed(&press(KeyCode::Tab));
        assert_eq!(list.focus, FuzzyFocus::List);
        assert_eq!(list.feed(&press(KeyCode::Esc)), FuzzyKey::Cancel);
    }
}
