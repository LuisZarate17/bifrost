//! The input box's text and cursor, as a state machine with no terminal in it: keys go
//! in, and a prompt comes out when one is sent. The interface ([`crate::inline`]) draws
//! it and decides what a key means outside the box (Stop, quit, approvals).
//!
//! A new line is made four ways, because terminals disagree about Shift+Enter (VS Code's
//! does not tell it from Enter): Shift+Enter, Alt+Enter, Ctrl+J, and a `\` at the end of
//! the line followed by Enter. A paste inserts its text, newlines and all, and never sends.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct Input {
    /// The text, one entry per line; never empty.
    lines: Vec<Vec<char>>,
    /// The cursor: a line, and a position in it counted in characters.
    row: usize,
    col: usize,
    /// The prompts sent this session, oldest first.
    history: Vec<String>,
    /// The history entry on show, when Up has walked into it.
    browsing: Option<usize>,
    /// What was being typed when Up first walked into the history.
    draft: String,
}

impl Default for Input {
    fn default() -> Self {
        Self { lines: vec![Vec::new()], row: 0, col: 0, history: Vec::new(), browsing: None, draft: String::new() }
    }
}

impl Input {
    /// The text, lines joined with `\n`.
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| l.iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn lines(&self) -> impl Iterator<Item = String> + '_ {
        self.lines.iter().map(|l| l.iter().collect())
    }

    /// The cursor's line and its position in that line, in characters.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub fn clear(&mut self) {
        self.set("");
        self.browsing = None;
    }

    fn set(&mut self, text: &str) {
        self.lines = text.split('\n').map(|l| l.chars().collect()).collect();
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].len();
    }

    /// Insert text at the cursor, newlines included. `\r\n` and a lone `\r` count as one.
    pub fn paste(&mut self, text: &str) {
        for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
            if c == '\n' {
                self.newline();
            } else if c == '\t' {
                (0..4).for_each(|_| self.insert(' '));
            } else if !c.is_control() {
                self.insert(c);
            }
        }
    }

    fn insert(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
    }

    fn newline(&mut self) {
        let rest = self.lines[self.row].split_off(self.col);
        self.row += 1;
        self.col = 0;
        self.lines.insert(self.row, rest);
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            self.lines[self.row].remove(self.col);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].extend(line);
        }
    }

    fn delete(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(next);
        }
    }

    fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    fn right(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else if !self.history.is_empty() {
            let at = self.browsing.map_or(self.history.len() - 1, |i| i.saturating_sub(1));
            if self.browsing.is_none() {
                self.draft = self.text();
            }
            self.browsing = Some(at);
            self.recall(at, true);
        }
    }

    fn down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else if let Some(at) = self.browsing {
            if at + 1 < self.history.len() {
                self.browsing = Some(at + 1);
                self.recall(at + 1, false);
            } else {
                self.browsing = None;
                let draft = std::mem::take(&mut self.draft);
                self.set(&draft);
            }
        }
    }

    /// Show history entry `at`. The cursor lands on the first line when walking back and
    /// on the last when walking forward, at the column it had, so the next Up or Down
    /// carries on through the history rather than through the entry's own lines.
    fn recall(&mut self, at: usize, back: bool) {
        let col = self.col;
        let entry = self.history[at].clone();
        self.set(&entry);
        if back {
            self.row = 0;
        }
        self.col = col.min(self.lines[self.row].len());
    }

    /// Take a key. `busy` is a turn running: Enter then makes no prompt, and the text
    /// waits. Returns the prompt when Enter sent one.
    pub fn key(&mut self, key: KeyEvent, busy: bool) -> Option<String> {
        let mods = key.modifiers;
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let alt = mods.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => self.newline(),
            KeyCode::Enter => {
                if self.col == self.lines[self.row].len() && self.lines[self.row].last() == Some(&'\\') {
                    self.lines[self.row].pop();
                    self.col -= 1;
                    self.newline();
                } else if !busy {
                    return self.send();
                }
            }
            KeyCode::Char('j') if ctrl && !alt => self.newline(),
            KeyCode::Char('\n') => self.newline(),
            // AltGr arrives as Ctrl+Alt on Windows, and types its character.
            KeyCode::Char(c) if (!ctrl && !alt) || (ctrl && alt) => self.insert(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.lines[self.row].len(),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            _ => {}
        }
        None
    }

    /// The text as a prompt, and the box emptied. Nothing but whitespace is not one.
    fn send(&mut self) -> Option<String> {
        let text = self.text().trim().to_string();
        if text.is_empty() {
            return None;
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.clear();
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn plain(code: KeyCode) -> KeyEvent {
        press(code, KeyModifiers::NONE)
    }

    /// Type each character.
    fn type_in(input: &mut Input, text: &str) {
        for c in text.chars() {
            input.key(plain(KeyCode::Char(c)), false);
        }
    }

    #[test]
    fn typing_and_enter_send_the_text_and_empty_the_box() {
        let mut input = Input::default();
        type_in(&mut input, "hello");
        assert_eq!(input.text(), "hello");
        assert_eq!(input.key(plain(KeyCode::Enter), false), Some("hello".into()));
        assert!(input.is_empty());
        // Nothing to send is not sent.
        assert_eq!(input.key(plain(KeyCode::Enter), false), None);
    }

    #[test]
    fn a_new_line_is_made_four_ways_and_none_of_them_sends() {
        let ways = [
            press(KeyCode::Enter, KeyModifiers::SHIFT),
            press(KeyCode::Enter, KeyModifiers::ALT),
            press(KeyCode::Char('j'), KeyModifiers::CONTROL),
        ];
        for way in ways {
            let mut input = Input::default();
            type_in(&mut input, "a");
            assert_eq!(input.key(way, false), None);
            type_in(&mut input, "b");
            assert_eq!(input.text(), "a\nb", "{way:?}");
        }
        // A backslash at the end of the line, then Enter: the backslash goes.
        let mut input = Input::default();
        type_in(&mut input, "a\\");
        assert_eq!(input.key(plain(KeyCode::Enter), false), None);
        type_in(&mut input, "b");
        assert_eq!(input.text(), "a\nb");
        // A backslash elsewhere is only a character.
        let mut input = Input::default();
        type_in(&mut input, "a\\b");
        assert_eq!(input.key(plain(KeyCode::Enter), false), Some("a\\b".into()));
    }

    #[test]
    fn a_paste_keeps_its_newlines_and_never_sends() {
        let mut input = Input::default();
        type_in(&mut input, "x");
        input.paste("one\r\ntwo\nthree");
        assert_eq!(input.text(), "xone\ntwo\nthree");
        assert_eq!(input.cursor(), (2, 5));
        assert!(!input.is_empty());
    }

    #[test]
    fn backspace_across_a_line_join_and_delete_at_a_line_end() {
        let mut input = Input::default();
        input.paste("ab\ncd");
        input.key(plain(KeyCode::Home), false);
        input.key(plain(KeyCode::Backspace), false);
        assert_eq!((input.text().as_str(), input.cursor()), ("abcd", (0, 2)));
        let mut input = Input::default();
        input.paste("ab\ncd");
        input.key(plain(KeyCode::Up), false);
        input.key(plain(KeyCode::End), false);
        input.key(plain(KeyCode::Delete), false);
        assert_eq!(input.text(), "abcd");
    }

    #[test]
    fn the_cursor_moves_by_character_across_lines_and_by_line() {
        let mut input = Input::default();
        input.paste("abc\nd");
        assert_eq!(input.cursor(), (1, 1));
        input.key(plain(KeyCode::Left), false);
        input.key(plain(KeyCode::Left), false);
        assert_eq!(input.cursor(), (0, 3));
        input.key(plain(KeyCode::Right), false);
        assert_eq!(input.cursor(), (1, 0));
        input.key(plain(KeyCode::End), false);
        input.key(plain(KeyCode::Up), false);
        assert_eq!(input.cursor(), (0, 1));
        input.key(plain(KeyCode::Down), false);
        assert_eq!(input.cursor(), (1, 1));
    }

    #[test]
    fn enter_is_refused_while_a_turn_runs_and_the_text_waits() {
        let mut input = Input::default();
        type_in(&mut input, "next");
        assert_eq!(input.key(plain(KeyCode::Enter), true), None);
        assert_eq!(input.text(), "next");
        assert_eq!(input.key(plain(KeyCode::Enter), false), Some("next".into()));
        // A new line still works while busy.
        type_in(&mut input, "a");
        input.key(press(KeyCode::Enter, KeyModifiers::SHIFT), true);
        assert_eq!(input.text(), "a\n");
    }

    #[test]
    fn up_and_down_walk_the_history_and_bring_the_draft_back() {
        let mut input = Input::default();
        for p in ["one", "two"] {
            type_in(&mut input, p);
            input.key(plain(KeyCode::Enter), false);
        }
        type_in(&mut input, "dra");
        input.key(plain(KeyCode::Up), false);
        assert_eq!(input.text(), "two");
        input.key(plain(KeyCode::Up), false);
        assert_eq!(input.text(), "one");
        input.key(plain(KeyCode::Up), false);
        assert_eq!(input.text(), "one", "the oldest stays");
        input.key(plain(KeyCode::Down), false);
        assert_eq!(input.text(), "two");
        input.key(plain(KeyCode::Down), false);
        assert_eq!(input.text(), "dra");
        input.key(plain(KeyCode::Down), false);
        assert_eq!(input.text(), "dra");
    }

    #[test]
    fn up_inside_several_lines_moves_between_them_before_it_walks_the_history() {
        let mut input = Input::default();
        type_in(&mut input, "old");
        input.key(plain(KeyCode::Enter), false);
        input.paste("a\nb");
        input.key(plain(KeyCode::Up), false);
        assert_eq!((input.text().as_str(), input.cursor().0), ("a\nb", 0));
        input.key(plain(KeyCode::Up), false);
        assert_eq!(input.text(), "old");
    }

    #[test]
    fn altgr_types_its_character_and_other_control_keys_do_nothing() {
        let mut input = Input::default();
        input.key(press(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT), false);
        input.key(press(KeyCode::Char('x'), KeyModifiers::CONTROL), false);
        input.key(press(KeyCode::Char('y'), KeyModifiers::ALT), false);
        assert_eq!(input.text(), "@");
    }
}
