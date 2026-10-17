//! The raven on Hugin's start screen: a Sixel image, nine columns by three rows, with the
//! start screen's three text lines beside it. Only Windows Terminal is known to draw Sixel
//! at exactly that size, so only it gets the image; everything else gets the text alone.

use ratatui::text::Line;

use crate::render::ansi;

/// The image: a complete Sixel string, 90 by 60 pixels, which Windows Terminal shows as
/// exactly [`COLS`] by [`ROWS`] cells.
pub const SIXEL: &str = include_str!("../assets/raven.six");
pub const COLS: u16 = 9;
pub const ROWS: u16 = 3;

/// Whether to draw the raven: in Windows Terminal (`WT_SESSION` set, `TERM_PROGRAM` not, as
/// other terminals that run under it set that), with color, and wide enough for a two-column
/// margin, the image, a two-column gap and the `longest` text line.
pub fn wanted(wt_session: bool, term_program: bool, color: bool, width: u16, longest: usize) -> bool {
    wt_session && !term_program && color && usize::from(width) >= 2 + usize::from(COLS) + 2 + longest
}

/// The bytes that draw the raven with `lines` beside it, leaving the cursor at column 1, two
/// rows below the third line. Text written over a Sixel image erases it in Windows
/// Terminal, so the image goes first, into rows made empty, and the text only into the
/// columns after it.
pub fn screen(lines: &[Line<'static>]) -> String {
    let mut out = String::new();
    // Four rows: the image's three and the blank one; this scrolls if the cursor is at the bottom.
    out.push_str(&"\r\n".repeat(4));
    out.push_str("\x1b[4A\x1b[3G");
    out.push_str("\x1b7");
    out.push_str(SIXEL);
    out.push_str("\x1b8");
    for (n, line) in lines.iter().take(usize::from(ROWS)).enumerate() {
        if n > 0 {
            out.push_str("\x1b[1B");
        }
        out.push_str("\x1b[14G");
        out.push_str(&ansi(line));
    }
    out.push_str("\r\x1b[2B");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Span;

    #[test]
    fn the_raven_is_drawn_only_in_windows_terminal_with_color_and_room() {
        assert!(wanted(true, false, true, 80, 40));
        assert!(!wanted(false, false, true, 80, 40));
        assert!(!wanted(true, true, true, 80, 40));
        assert!(!wanted(true, false, false, 80, 40));
        assert!(!wanted(true, false, true, 20, 40));
    }

    #[test]
    fn the_raven_needs_exactly_its_margin_image_gap_and_text() {
        assert!(wanted(true, false, true, 2 + 9 + 2 + 40, 40));
        assert!(!wanted(true, false, true, 2 + 9 + 2 + 40 - 1, 40));
    }

    fn lines() -> Vec<Line<'static>> {
        ["one", "two", "three"]
            .iter()
            .map(|t| Line::from(Span::styled(*t, Style::default().fg(Color::Rgb(1, 2, 3)).add_modifier(Modifier::BOLD))))
            .collect()
    }

    #[test]
    fn the_screen_makes_room_then_draws_the_image_then_the_text_beside_it() {
        let s = screen(&lines());
        assert!(s.starts_with("\r\n\r\n\r\n\r\n\x1b[4A\x1b[3G\x1b7"));
        assert_eq!(s.matches(SIXEL).count(), 1);
        let image = s.find(SIXEL).unwrap();
        assert!(s.find("\x1b7").unwrap() < image);
        assert!(image + SIXEL.len() <= s.find("\x1b8").unwrap());
        let text = &s[s.find("\x1b8").unwrap()..];
        assert_eq!(text.matches("\x1b[14G").count(), 3);
        assert_eq!(text.matches("\x1b[1B").count(), 2);
        for (word, at) in [("one", 0), ("two", 1), ("three", 2)] {
            let col = text.match_indices("\x1b[14G").nth(at).unwrap().0;
            assert!(text[col..].starts_with(&format!("\x1b[14G\x1b[38;2;1;2;3m\x1b[1m{word}\x1b[0m")));
        }
    }

    #[test]
    fn the_screen_ends_at_column_one_two_rows_down() {
        assert!(screen(&lines()).ends_with("\x1b[0m\r\x1b[2B"));
    }
}
