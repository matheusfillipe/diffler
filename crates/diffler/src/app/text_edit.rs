//! The editing surface behind every multi-line text field: the readline/emacs
//! key set over a `(buffer, char cursor)` pair. Shared by the input modal and
//! the diff pane's inline comment composer, so both accept the same keys.
//! Vertical movement lives with the field, which is the only thing that knows
//! how its text wraps on screen.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::byte_index;

/// What a key meant to the field that owns the buffer. Editing keys are
/// applied in place and report [`Edit::Consumed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Consumed,
    Submit,
    Cancel,
}

/// Apply one key to `buffer` at `cursor` (a char index).
pub fn apply(buffer: &mut String, cursor: &mut usize, key: &KeyEvent) -> Edit {
    // Alt-Enter inserts a newline; Ctrl-J is the fallback for terminals
    // that swallow the alt modifier
    let newline = (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::ALT))
        || (key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL));
    // exactly one of ctrl/alt makes an emacs chord: Windows reports AltGr
    // as ctrl+alt together, and those keys carry text (@, {, €) to insert
    let ctrl =
        key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT);
    let alt =
        key.modifiers.contains(KeyModifiers::ALT) && !key.modifiers.contains(KeyModifiers::CONTROL);
    if newline {
        buffer.insert(byte_index(buffer, *cursor), '\n');
        *cursor += 1;
        return Edit::Consumed;
    }
    match key.code {
        KeyCode::Esc => return Edit::Cancel,
        KeyCode::Enter => return Edit::Submit,
        // the readline/emacs set every shell input carries; these are
        // widget-internal like Backspace and the arrows, not remappable
        // screen actions
        KeyCode::Char('a') if ctrl => *cursor = line_start(buffer, *cursor),
        KeyCode::Char('e') if ctrl => *cursor = line_end(buffer, *cursor),
        KeyCode::Char('u') if ctrl => {
            let start = line_start(buffer, *cursor);
            remove_chars(buffer, start, *cursor);
            *cursor = start;
        }
        KeyCode::Char('k') if ctrl => {
            // at line end, kill the newline itself: readline joins
            let end = line_end(buffer, *cursor).max((*cursor + 1).min(buffer.chars().count()));
            remove_chars(buffer, *cursor, end);
        }
        KeyCode::Char('w') if ctrl => {
            let start = prev_word(buffer, *cursor);
            remove_chars(buffer, start, *cursor);
            *cursor = start;
        }
        KeyCode::Backspace if alt => {
            let start = prev_word(buffer, *cursor);
            remove_chars(buffer, start, *cursor);
            *cursor = start;
        }
        KeyCode::Char('d') if alt => {
            let end = next_word(buffer, *cursor);
            remove_chars(buffer, *cursor, end);
        }
        KeyCode::Char('b') if alt => *cursor = prev_word(buffer, *cursor),
        KeyCode::Char('f') if alt => *cursor = next_word(buffer, *cursor),
        KeyCode::Char('b') if ctrl => *cursor = cursor.saturating_sub(1),
        KeyCode::Char('f') if ctrl => *cursor = (*cursor + 1).min(buffer.chars().count()),
        KeyCode::Char('d') if ctrl => {
            if *cursor < buffer.chars().count() {
                buffer.remove(byte_index(buffer, *cursor));
            }
        }
        KeyCode::Delete => {
            if *cursor < buffer.chars().count() {
                buffer.remove(byte_index(buffer, *cursor));
            }
        }
        // terminals with legacy input send ctrl-backspace as ctrl-h
        KeyCode::Char('h') if ctrl => {
            if *cursor > 0 {
                *cursor -= 1;
                buffer.remove(byte_index(buffer, *cursor));
            }
        }
        // a char with a lone ctrl/alt held is a chord, never text
        KeyCode::Char(c) if !ctrl && !alt => {
            buffer.insert(byte_index(buffer, *cursor), c);
            *cursor += 1;
        }
        KeyCode::Backspace => {
            if *cursor > 0 {
                *cursor -= 1;
                buffer.remove(byte_index(buffer, *cursor));
            }
        }
        KeyCode::Left => *cursor = cursor.saturating_sub(1),
        KeyCode::Right => *cursor = (*cursor + 1).min(buffer.chars().count()),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = buffer.chars().count(),
        _ => {}
    }
    Edit::Consumed
}

/// Char index of the current line's start (just past the previous newline).
pub fn line_start(buffer: &str, cursor: usize) -> usize {
    buffer
        .chars()
        .take(cursor)
        .enumerate()
        .filter(|&(_, c)| c == '\n')
        .last()
        .map_or(0, |(i, _)| i + 1)
}

/// Char index of the current line's end (the next newline, or the buffer end).
pub fn line_end(buffer: &str, cursor: usize) -> usize {
    buffer
        .chars()
        .enumerate()
        .skip(cursor)
        .find(|&(_, c)| c == '\n')
        .map_or_else(|| buffer.chars().count(), |(i, _)| i)
}

/// Char index of the previous word's start: skip whitespace back, then the
/// word itself. One whitespace-word rule serves both the ctrl and meta ops,
/// simpler than readline's split, and right for comment prose.
fn prev_word(buffer: &str, cursor: usize) -> usize {
    let chars: Vec<char> = buffer.chars().take(cursor).collect();
    let ws_at = |i: usize| chars.get(i).is_some_and(|c| c.is_whitespace());
    let mut i = chars.len();
    while i > 0 && ws_at(i - 1) {
        i -= 1;
    }
    while i > 0 && !ws_at(i - 1) {
        i -= 1;
    }
    i
}

/// Char index just past the next word: skip whitespace forward, then the word.
fn next_word(buffer: &str, cursor: usize) -> usize {
    let chars: Vec<char> = buffer.chars().collect();
    let ws_at = |i: usize| chars.get(i).is_some_and(|c| c.is_whitespace());
    let mut i = cursor.min(chars.len());
    while i < chars.len() && ws_at(i) {
        i += 1;
    }
    while i < chars.len() && !ws_at(i) {
        i += 1;
    }
    i
}

/// Remove the chars in `[start, end)` (char indices) from `buffer`.
fn remove_chars(buffer: &mut String, start: usize, end: usize) {
    if start >= end {
        return;
    }
    let from = byte_index(buffer, start);
    let to = byte_index(buffer, end);
    buffer.replace_range(from..to, "");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(buffer: &mut String, cursor: &mut usize, code: KeyCode) -> Edit {
        apply(buffer, cursor, &KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn enter_submits_and_esc_cancels_without_touching_the_buffer() {
        let (mut buffer, mut cursor) = ("hi".to_owned(), 2);
        assert_eq!(
            press(&mut buffer, &mut cursor, KeyCode::Enter),
            Edit::Submit
        );
        assert_eq!(press(&mut buffer, &mut cursor, KeyCode::Esc), Edit::Cancel);
        assert_eq!(buffer, "hi");
    }

    #[test]
    fn alt_enter_inserts_a_newline_at_the_cursor() {
        let (mut buffer, mut cursor) = ("ab".to_owned(), 1);
        let edit = apply(
            &mut buffer,
            &mut cursor,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        );
        assert_eq!(edit, Edit::Consumed);
        assert_eq!(buffer, "a\nb");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn a_multibyte_buffer_edits_by_char_not_byte() {
        let (mut buffer, mut cursor) = ("héllo".to_owned(), 2);
        press(&mut buffer, &mut cursor, KeyCode::Backspace);
        assert_eq!(buffer, "hllo");
        assert_eq!(cursor, 1);
    }
}
