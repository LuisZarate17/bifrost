//! What was said, kept so it can be laid out for any width: the interface owns its scrollback,
//! as rows, and shows a window onto them.
//!
//! Each thing said ([`Said`]) is wrapped once per width ([`Transcript::lay`]); a new width lays
//! the whole of it out again, and a new entry only itself. Every prompt of the person's own is
//! marked, so that when the window has scrolled past one, [`Transcript::view`] can give its first
//! row back to be pinned on the top row.

use ratatui::text::Line;

use crate::render::{self, Theme};
use crate::ui::{Approval, Event};

/// The most entries kept; when there are more the oldest quarter goes.
const HISTORY: usize = 4000;

/// Something said into the transcript, kept as it was said so it can be laid out again for
/// another width (the start screen, for one, depends on it).
pub enum Said {
    Event(Event),
    Ask(Approval),
}

impl Said {
    fn lines(&self, theme: &Theme, width: u16) -> Vec<Line<'static>> {
        match self {
            Said::Event(e) => render::scrollback(e, theme, width),
            Said::Ask(a) => render::approval(a, theme, width),
        }
    }
}

/// Where one of the person's prompts begins, and what it said.
struct Mark {
    row: usize,
    text: String,
}

/// The rows to show: the prompt to pin on the top row, if one is, and the rows below it.
pub struct View {
    pub sticky: Option<Line<'static>>,
    pub rows: Vec<Line<'static>>,
}

#[derive(Default)]
pub struct Transcript {
    history: Vec<Said>,
    /// The width the rows are laid out for, and how many entries of the history are in them.
    width: u16,
    laid: usize,
    rows: Vec<Line<'static>>,
    marks: Vec<Mark>,
}

/// A row with nothing in it.
fn is_blank(line: &Line) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

impl Transcript {
    pub fn push(&mut self, said: Said) {
        if self.history.len() >= HISTORY {
            self.history.drain(..HISTORY / 4);
            // The rows were laid out from entries that are gone.
            self.width = 0;
        }
        self.history.push(said);
    }

    /// How many rows there are, as last laid out.
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// Lay out what is not laid out yet, all of it again if `width` is new. Returns how many rows
    /// were added, or 0 after laying out again (where the rows were, nobody can say).
    pub fn lay(&mut self, width: u16, theme: &Theme) -> usize {
        let again = width != self.width;
        if again {
            self.width = width;
            self.rows.clear();
            self.marks.clear();
            self.laid = 0;
        }
        let before = self.rows.len();
        for i in self.laid..self.history.len() {
            let lines = self.history[i].lines(theme, width);
            if lines.is_empty() {
                continue;
            }
            if let Said::Event(Event::Prompt(text)) = &self.history[i] {
                // A blank row sets a prompt apart, unless the row above is blank already (the start
                // screen ends with one) or there is no row above.
                if self.rows.last().is_some_and(|l| !is_blank(l)) {
                    self.rows.push(Line::default());
                }
                self.marks.push(Mark { row: self.rows.len(), text: text.clone() });
            }
            self.rows.extend(lines);
        }
        self.laid = self.history.len();
        if again { 0 } else { self.rows.len() - before }
    }

    /// The window of `room` rows that ends `scroll` rows above the end (a `scroll` past the
    /// start is the start). When the most recent prompt at or above the window's first row is
    /// not itself in the window, it comes back as the sticky row and the window gives up its
    /// first row to it.
    pub fn view(&self, room: usize, scroll: usize, theme: &Theme, width: u16) -> View {
        let start = self.rows.len().saturating_sub(room + scroll);
        let above = self.marks.partition_point(|m| m.row <= start);
        let sticky = match above.checked_sub(1).map(|i| &self.marks[i]) {
            Some(m) if m.row < start && room > 1 => Some(render::sticky_prompt(&m.text, theme, width)),
            _ => None,
        };
        let first = start + usize::from(sticky.is_some());
        let end = (start + room).min(self.rows.len());
        let rows = self.rows.get(first..end).map(<[_]>::to_vec).unwrap_or_default();
        View { sticky, rows }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> Said {
        Said::Event(Event::Prompt(text.into()))
    }

    fn answer(text: &str) -> Said {
        Said::Event(Event::Answer(text.into()))
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    fn mono() -> Theme {
        Theme::new(false)
    }

    /// A transcript of two turns, each a prompt and a three-line answer.
    fn two_turns() -> Transcript {
        let mut t = Transcript::default();
        for (q, a) in [("first", "a1\na2\na3"), ("second", "b1\nb2\nb3")] {
            t.push(said(q));
            t.push(answer(a));
        }
        t.lay(40, &mono());
        t
    }

    #[test]
    fn a_blank_row_sets_each_prompt_apart_but_not_the_first_of_all() {
        let t = two_turns();
        assert_eq!(text(&t.rows), ["› first", "a1", "a2", "a3", "", "› second", "b1", "b2", "b3"]);
    }

    #[test]
    fn a_prompt_after_the_start_screens_own_blank_row_has_no_second_blank() {
        let mut t = Transcript::default();
        let header = Event::Header { folder: "C:\\w\\demo".into(), branch: None, changed: None, model: "m".into(), shell: String::new() };
        t.push(Said::Event(header));
        t.push(said("hi"));
        t.lay(120, &Theme::new(true));
        let rows = text(&t.rows);
        // The raven's four rows and its empty one, then the prompt.
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[4], "");
        assert_eq!(rows[5], "› hi");
        // With one header line (no color) there is no blank of its own, so one is put in.
        let mut t = Transcript::default();
        let header = Event::Header { folder: "C:\\w\\demo".into(), branch: None, changed: None, model: "m".into(), shell: String::new() };
        t.push(Said::Event(header));
        t.push(said("hi"));
        t.lay(120, &mono());
        assert_eq!(text(&t.rows)[1..], ["", "› hi"]);
    }

    #[test]
    fn the_window_at_the_end_has_no_sticky_row_when_its_prompt_is_in_it() {
        let t = two_turns();
        let v = t.view(4, 0, &mono(), 40);
        assert!(v.sticky.is_none());
        assert_eq!(text(&v.rows), ["› second", "b1", "b2", "b3"].map(String::from));
    }

    #[test]
    fn once_the_prompt_has_scrolled_off_it_is_pinned_and_takes_the_top_row() {
        let t = two_turns();
        // Three rows: "b1 b2 b3"; the prompt above them is not in the window.
        let v = t.view(3, 0, &mono(), 40);
        assert_eq!(text(&[v.sticky.unwrap()]), ["› second"]);
        assert_eq!(text(&v.rows), ["b2", "b3"]);
        // Scrolled up into the first answer: the first prompt is pinned.
        let v = t.view(3, 4, &mono(), 40);
        assert_eq!(text(&[v.sticky.unwrap()]), ["› first"]);
        assert_eq!(text(&v.rows), ["a3", ""]);
    }

    #[test]
    fn a_window_that_starts_on_a_prompt_needs_no_sticky_row() {
        let t = two_turns();
        let v = t.view(9, 0, &mono(), 40);
        assert!(v.sticky.is_none());
        assert_eq!(v.rows.len(), 9);
    }

    #[test]
    fn the_sticky_prompt_is_one_row_cut_with_an_ellipsis() {
        let mut t = Transcript::default();
        t.push(said("one two three four five six seven eight nine ten\nsecond line"));
        t.push(answer("x\ny\nz"));
        t.lay(20, &mono());
        let v = t.view(2, 0, &mono(), 20);
        let sticky = text(&[v.sticky.unwrap()]);
        assert_eq!(sticky[0].chars().count(), 20);
        assert!(sticky[0].starts_with("› one two three") && sticky[0].ends_with('…'), "{sticky:?}");
    }

    #[test]
    fn a_new_width_lays_everything_out_again_and_a_new_entry_only_itself() {
        let mut t = Transcript::default();
        t.push(answer("aaa bbb ccc"));
        // The first layout is a layout from scratch.
        assert_eq!(t.lay(20, &mono()), 0);
        assert_eq!(t.lay(20, &mono()), 0);
        t.push(answer("more"));
        assert_eq!(t.lay(20, &mono()), 1);
        // 7 columns: "aaa bbb" and "ccc".
        assert_eq!(t.lay(7, &mono()), 0);
        assert_eq!(text(&t.rows), ["aaa bbb", "ccc", "more"]);
    }

    #[test]
    fn an_empty_transcript_has_an_empty_view() {
        let mut t = Transcript::default();
        t.lay(40, &mono());
        let v = t.view(5, 3, &mono(), 40);
        assert!(v.sticky.is_none() && v.rows.is_empty());
    }
}
