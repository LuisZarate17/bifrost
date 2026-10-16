//! What Hugin's terminal interface looks like, as pure functions: an [`Event`] becomes the
//! lines that go into the terminal's scrollback, and the live area at the bottom (a status
//! line and the input box) is drawn into a ratatui frame. Nothing here touches a terminal,
//! so the tests draw into a `TestBackend` and compare line by line.
//!
//! **Quiet color.** Raven Violet is on the prompt marker `›`, the word `hugin` in the
//! header, approvals and the `●` of a result, and nowhere else; a diff is green and red,
//! its context dim. `NO_COLOR` removes every color; bold and dim stay.

use bifrost_core::colors::palette;
use bifrost_core::tools::{PlanStatus, PlanStep};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthChar;

use crate::input::Input;
use crate::ui::{Approval, Event, ToolLine};
use crate::view;

/// The most lines the input box grows to.
pub const INPUT_ROWS: usize = 8;

pub const HINT: &str = "enter send · shift+enter new line · esc stop · ctrl+d quit";
pub const ASK: &str = "approve? [y/n/a]";

/// Whether there is color to draw, and which.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    color: bool,
}

impl Theme {
    pub fn new(color: bool) -> Self {
        Self { color }
    }

    /// `NO_COLOR` set to anything but nothing turns color off (no-color.org).
    pub fn from_no_color(var: Option<&str>) -> Self {
        Self::new(var.is_none_or(str::is_empty))
    }

    pub fn from_env() -> Self {
        Self::from_no_color(std::env::var("NO_COLOR").ok().as_deref())
    }

    fn fg(&self, color: Color) -> Style {
        if self.color { Style::default().fg(color) } else { Style::default() }
    }

    /// Raven Violet, the dark shade.
    fn accent(&self) -> Style {
        self.fg(hex(&palette().hugin.dark))
    }

    fn dim(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn hex(value: &str) -> Color {
    let n = |at: usize| u8::from_str_radix(value.get(at..at + 2).unwrap_or("00"), 16).unwrap_or(0);
    Color::Rgb(n(1), n(3), n(5))
}

fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

fn plain(text: &str) -> Line<'static> {
    Line::from(text.to_string())
}

/// What goes into the scrollback for `e`, wrapped to `width`. Empty for an event that is
/// only a change in the live area (the status, the end of a turn).
pub fn scrollback(e: &Event, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let lines = match e {
        Event::Header { folder, branch, changed, model, shell } => {
            start_screen(theme, folder, branch.as_deref(), *changed, model, shell, width)
        }
        Event::Prompt(text) => text
            .split('\n')
            .enumerate()
            .map(|(n, l)| {
                let lead = if n == 0 { span("› ", theme.accent()) } else { span("  ", Style::default()) };
                Line::from(vec![lead, Span::raw(l.to_string())])
            })
            .collect(),
        Event::Allowed(a) => {
            // The card as an approval shows it, wrapped; the note is one short line.
            let mut lines = approval(a, theme, width);
            lines.push(Line::from(span(format!("  {}", view::ALLOWED_NOTE), theme.dim())));
            return lines;
        }
        Event::Tool(t) => vec![tool_line(t, theme)],
        Event::Plan(steps) => steps.iter().map(plan_line).collect(),
        Event::Answer(text) => text.split('\n').map(plain).collect(),
        Event::Error(text) => text
            .split('\n')
            .enumerate()
            .map(|(n, l)| if n == 0 { Line::from(vec![span("! ", theme.bold()), Span::raw(l.to_string())]) } else { plain(l) })
            .collect(),
        Event::Stopped => vec![Line::from(span("· stopped", theme.dim()))],
        Event::Status(_) | Event::TurnEnd => Vec::new(),
    };
    wrap(lines, width)
}

// ── the start screen ────────────────────────────────────────────────────────

/// The raven, one character per pixel: `.` empty, `#` body, `w` wing, `b` bill, `-` the line
/// in the bill, `o` the eye, `|` legs, `=` the perch.
pub const RAVEN: [&str; 22] = [
    "............................................",
    "...............................####.........",
    "..............................#######.......",
    ".............................########bb.....",
    ".............................#####o##bbbbb..",
    "............................#########bbbbbb.",
    "...........................##########bbbbb..",
    "..........................ww#########bbb....",
    "........................wwwww########.......",
    "......................wwwwwww#######........",
    "....................wwwwwwwww#######........",
    "..................wwwwwwwwww######..........",
    "................wwwwwwwwwww#######..........",
    "..............wwwwwwwwwww######.............",
    "............wwwwwwwwww#######...............",
    "..........wwwwwwwww#########................",
    ".........wwwwww###########..................",
    "......##ww############..|...................",
    "....###########.....|...|...................",
    "..#######...........|...|...................",
    ".#####..............|...|...................",
    ".............=======|===|=======............",
];

/// The art is this many columns wide, after a margin of [`MARGIN`] and before [`GAP`].
const ART_WIDTH: usize = 44;
const MARGIN: usize = 2;
const GAP: usize = 4;
/// Narrower than this there is no room for even the art.
const MIN_ART: usize = 48;
/// The art rows (from 0) that the text lines sit beside.
const TEXT_ROWS: [usize; 4] = [3, 4, 6, 7];

fn pixel_color(c: char) -> Option<Color> {
    let p = palette();
    match c {
        '#' => Some(hex(&p.hugin.dark)),
        'w' => Some(Color::Rgb(0x6a, 0x58, 0xe6)),
        'b' | '|' => Some(hex(&p.hugin.light)),
        '-' => Some(Color::Rgb(0x3c, 0x2f, 0x96)),
        'o' => Some(Color::Rgb(0xec, 0xeb, 0xe6)),
        '=' => Some(Color::Rgb(0x4a, 0x44, 0x5c)),
        _ => None,
    }
}

/// A pixel map as half blocks, two pixel rows to a terminal row: nothing is a space, the top
/// alone `▀` in its colour, the bottom alone `▄`, both `▀` with the bottom as the background.
fn half_blocks(map: &[&str]) -> Vec<Vec<(char, Style)>> {
    let pixels: Vec<Vec<char>> = map.iter().map(|r| r.chars().collect()).collect();
    let at = |row: usize, x: usize| pixels.get(row).and_then(|r| r.get(x)).copied().and_then(pixel_color);
    let width = pixels.iter().map(Vec::len).max().unwrap_or(0);
    (0..pixels.len().div_ceil(2))
        .map(|r| {
            (0..width)
                .map(|x| match (at(2 * r, x), at(2 * r + 1, x)) {
                    (None, None) => (' ', Style::default()),
                    (Some(t), None) => ('▀', Style::default().fg(t)),
                    (None, Some(b)) => ('▄', Style::default().fg(b)),
                    (Some(t), Some(b)) => ('▀', Style::default().fg(t).bg(b)),
                })
                .collect()
        })
        .collect()
}

/// The first line of a session: `hugin · <folder> · <branch>, <n> changed · <model> · <shell>`.
fn header_line(theme: &Theme, folder: &str, branch: Option<&str>, changed: Option<usize>, model: &str, shell: &str) -> Line<'static> {
    let text = view::header(folder, branch, changed, model, shell);
    let rest = text.strip_prefix("hugin").unwrap_or(&text).to_string();
    Line::from(vec![span("hugin", theme.accent().add_modifier(Modifier::BOLD)), span(rest, theme.dim())])
}

/// What opens a session in the scrollback: the raven with the folder, the model and the shell
/// beside it, then an empty line. With no room for the art, or no color, the one header line.
pub fn start_screen(
    theme: &Theme,
    folder: &str,
    branch: Option<&str>,
    changed: Option<usize>,
    model: &str,
    shell: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let width = width as usize;
    if !theme.color || width < MIN_ART {
        return vec![header_line(theme, folder, branch, changed, model, shell)];
    }
    let place = match view::git_note(branch, changed) {
        Some(g) => vec![Span::raw(view::folder_name(folder)), span(format!(" · {g}"), theme.dim())],
        None => vec![Span::raw(view::folder_name(folder))],
    };
    let mut who = vec![Span::raw(model.to_string())];
    if !shell.is_empty() {
        who.push(span(format!(" · {shell}"), theme.dim()));
    }
    let text = vec![
        Line::from(span("Hugin", theme.accent().add_modifier(Modifier::BOLD))),
        Line::from(span("thought, sent out to work", theme.dim())),
        Line::from(place),
        Line::from(who),
    ];
    let longest = text.iter().map(Line::width).max().unwrap_or(0);
    let side_by_side = width >= MARGIN + ART_WIDTH + GAP + longest;

    let margin = || span(" ".repeat(MARGIN), Style::default());
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (r, cells) in half_blocks(&RAVEN).iter().enumerate() {
        let mut spans = vec![margin()];
        spans.extend(from_cells(cells).spans);
        if side_by_side && let Some(n) = TEXT_ROWS.iter().position(|&t| t == r) {
            spans.push(span(" ".repeat(GAP), Style::default()));
            spans.extend(text[n].spans.clone());
        }
        lines.push(Line::from(spans));
    }
    if !side_by_side {
        for l in text {
            let mut spans = vec![margin()];
            spans.extend(l.spans);
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::default());
    lines
}

/// An approval's header and the lines to read before answering.
pub fn approval(a: &Approval, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let target = if a.target.is_empty() { String::new() } else { format!(" {}", a.target) };
    let mut lines = vec![Line::from(span(format!("? {}{target}", a.name), theme.accent()))];
    for d in &a.detail {
        // A command is not a diff: its `-la` is not a removed line.
        let style = match d.chars().next() {
            Some('+') if a.name != "command" => theme.fg(Color::Green),
            Some('-') if a.name != "command" => theme.fg(Color::Red),
            _ => theme.dim(),
        };
        lines.push(Line::from(span(format!("  {d}"), style)));
    }
    wrap(lines, width)
}

fn tool_line(t: &ToolLine, theme: &Theme) -> Line<'static> {
    let mut spans = vec![span("●", theme.accent()), span(format!(" {}", t.name), theme.bold())];
    if !t.target.is_empty() {
        spans.push(Span::raw(format!(" {}", t.target)));
    }
    if !t.outcome.is_empty() {
        spans.push(span(format!(" → {}", t.outcome), theme.dim()));
    }
    Line::from(spans)
}

fn plan_line(step: &PlanStep) -> Line<'static> {
    let mark = if step.status == PlanStatus::Done { 'x' } else { ' ' };
    Line::from(format!("[{mark}] {}", step.text))
}

/// Paint lines into a buffer the height of the lines, as `insert_before` wants.
pub fn paint(lines: &[Line<'static>], buf: &mut Buffer) {
    Paragraph::new(lines.to_vec()).render(buf.area, buf);
}

// ── wrapping ────────────────────────────────────────────────────────────────

/// Every line cut to `width`: at the last space that fits, or in the middle of a word
/// that is longer than the line.
pub fn wrap(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = (width as usize).max(1);
    lines.into_iter().flat_map(|l| wrap_line(l, width)).collect()
}

fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    if line.width() <= width {
        return vec![line];
    }
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style.patch(line.style))))
        .collect();
    let w = |c: char| c.width().unwrap_or(0);
    let mut out = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let (mut end, mut used) = (start, 0);
        while end < cells.len() && used + w(cells[end].0) <= width {
            used += w(cells[end].0);
            end += 1;
        }
        if end == start {
            end += 1;
        }
        if end == cells.len() {
            out.push(from_cells(&cells[start..end]));
            break;
        }
        match (start + 1..=end).rev().find(|&i| cells[i].0 == ' ') {
            Some(space) => {
                out.push(from_cells(&cells[start..space]));
                start = space + 1;
            }
            None => {
                out.push(from_cells(&cells[start..end]));
                start = end;
            }
        }
    }
    out
}

fn from_cells(cells: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for &(c, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    Line::from(spans)
}

// ── the live area ───────────────────────────────────────────────────────────

/// What the status line says.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    /// What the turn is waiting for, and for how many seconds.
    Working(String, u64),
    Approving,
}

pub fn status_line(status: &Status, theme: &Theme) -> Line<'static> {
    match status {
        Status::Idle => Line::from(span(HINT, theme.dim())),
        Status::Working(what, secs) => Line::from(span(format!("{what} · {secs}s"), theme.dim())),
        Status::Approving => Line::from(span(ASK, theme.accent())),
    }
}

/// The input as the rows it takes on a screen `width` wide, and where the cursor is. Rows
/// are cut by character, one short of the width, so the cursor at the end of a full row
/// still has a column.
struct Rows {
    rows: Vec<String>,
    cursor: (usize, usize),
}

fn rows(input: &Input, width: u16) -> Rows {
    let chunk = (width as usize).saturating_sub(3).max(1);
    let (crow, ccol) = input.cursor();
    let mut out = Rows { rows: Vec::new(), cursor: (0, 0) };
    for (n, line) in input.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let first_row = out.rows.len();
        let pieces = chars.chunks(chunk).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>();
        let pieces = if pieces.is_empty() { vec![String::new()] } else { pieces };
        out.rows.extend(pieces);
        if n == crow {
            out.cursor = if ccol > 0 && ccol % chunk == 0 && ccol == chars.len() {
                (first_row + ccol / chunk - 1, chunk)
            } else {
                (first_row + ccol / chunk, ccol % chunk)
            };
        }
    }
    out
}

/// The height of the live area: the status line and the input's rows, up to [`INPUT_ROWS`].
pub fn live_height(input: &Input, width: u16) -> u16 {
    1 + rows(input, width).rows.len().min(INPUT_ROWS) as u16
}

/// Draw the status line and the input box into the frame's whole area.
pub fn draw_live(f: &mut Frame, status: &Status, input: &Input, theme: &Theme) {
    let area = f.area();
    f.render_widget(Paragraph::new(status_line(status, theme)), Rect { height: 1, ..area });
    if area.height < 2 {
        return;
    }
    let laid = rows(input, area.width);
    let shown = (area.height as usize - 1).min(INPUT_ROWS);
    // The window of rows that keeps the cursor in view.
    let from = (laid.cursor.0 + 1).saturating_sub(shown).min(laid.rows.len().saturating_sub(shown));
    let lines: Vec<Line> = laid
        .rows
        .iter()
        .enumerate()
        .skip(from)
        .take(shown)
        .map(|(n, text)| {
            // The marker is on the box's first row only.
            let lead = if n == 0 { span("› ", theme.accent()) } else { span("  ", Style::default()) };
            Line::from(vec![lead, Span::raw(text.clone())])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), Rect { y: area.y + 1, height: area.height - 1, ..area });
    let (row, col) = laid.cursor;
    f.set_cursor_position((area.x + 2 + col as u16, area.y + 1 + (row - from) as u16));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn lit() -> Theme {
        Theme::new(true)
    }

    fn mono() -> Theme {
        Theme::new(false)
    }

    /// A buffer's rows as text, with the blanks at the end of each cut off.
    fn text_of(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
            .collect()
    }

    /// The lines an event adds to the scrollback, as the terminal would show them.
    fn shown(lines: &[Line<'static>], width: u16) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, lines.len() as u16));
        paint(lines, &mut buf);
        buf
    }

    fn event_text(e: &Event, theme: &Theme, width: u16) -> Vec<String> {
        text_of(&shown(&scrollback(e, theme, width), width))
    }

    fn type_in(input: &mut Input, text: &str) {
        for c in text.chars() {
            input.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), false);
        }
    }

    fn live(status: &Status, input: &Input, theme: &Theme, w: u16, h: u16) -> (Vec<String>, Buffer) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw_live(f, status, input, theme)).unwrap();
        let buf = term.backend().buffer().clone();
        (text_of(&buf), buf)
    }

    fn colored(buf: &Buffer) -> bool {
        buf.content().iter().any(|c| c.fg != Color::Reset || c.bg != Color::Reset)
    }

    fn header() -> Event {
        Event::Header {
            folder: "C:\\work\\demo".into(),
            branch: Some("main".into()),
            changed: Some(2),
            model: "qwen".into(),
            shell: String::new(),
        }
    }

    fn step(text: &str, status: PlanStatus) -> PlanStep {
        PlanStep { text: text.into(), status }
    }

    // ── the live area ──

    #[test]
    fn idle_the_status_line_is_a_quiet_hint_above_an_empty_prompt() {
        let (rows, _) = live(&Status::Idle, &Input::default(), &lit(), 70, 2);
        assert_eq!(rows, [HINT, "›"]);
    }

    #[test]
    fn a_running_turn_says_what_it_waits_for_and_for_how_long() {
        let (rows, _) = live(&Status::Working("thinking".into(), 12), &Input::default(), &lit(), 40, 2);
        assert_eq!(rows[0], "thinking · 12s");
        let (rows, _) = live(&Status::Working("running cargo check".into(), 4), &Input::default(), &lit(), 40, 2);
        assert_eq!(rows[0], "running cargo check · 4s");
        let (rows, _) = live(&Status::Working("Starting Ollama…".into(), 0), &Input::default(), &lit(), 40, 2);
        assert_eq!(rows[0], "Starting Ollama… · 0s");
    }

    #[test]
    fn an_approval_asks_in_the_status_line_in_the_accent() {
        let (rows, buf) = live(&Status::Approving, &Input::default(), &lit(), 40, 2);
        assert_eq!(rows[0], "approve? [y/n/a]");
        assert_eq!(buf[(0, 0)].fg, hex("#8b7cf8"));
    }

    #[test]
    fn a_two_line_input_takes_two_rows_and_the_cursor_follows_the_text() {
        let mut input = Input::default();
        type_in(&mut input, "first");
        input.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), false);
        type_in(&mut input, "second");
        assert_eq!(live_height(&input, 62), 3);
        let mut term = Terminal::new(TestBackend::new(62, 3)).unwrap();
        term.draw(|f| draw_live(f, &Status::Idle, &input, &lit())).unwrap();
        assert_eq!(text_of(term.backend().buffer()), [HINT, "› first", "  second"]);
        term.backend_mut().assert_cursor_position((8, 2));
    }

    #[test]
    fn the_input_wraps_and_grows_only_to_eight_rows() {
        let mut input = Input::default();
        type_in(&mut input, &"x".repeat(25));
        // Rows of width - 3 = 7 characters: 25 characters take 4.
        assert_eq!(live_height(&input, 10), 1 + 4);
        let mut input = Input::default();
        input.paste(&"line\n".repeat(20));
        assert_eq!(live_height(&input, 40), 1 + INPUT_ROWS as u16);
        let (rows, _) = live(&Status::Idle, &input, &lit(), 40, 1 + INPUT_ROWS as u16);
        // The cursor is on the last, empty line, so the window ends there.
        assert_eq!(rows[INPUT_ROWS], "");
        assert_eq!(rows[INPUT_ROWS - 1], "  line");
    }

    #[test]
    fn without_color_the_live_area_has_none() {
        let mut input = Input::default();
        type_in(&mut input, "hi");
        for status in [Status::Idle, Status::Approving, Status::Working("thinking".into(), 1)] {
            let (_, buf) = live(&status, &input, &mono(), 40, 2);
            assert!(!colored(&buf), "{status:?}");
        }
        let (_, buf) = live(&Status::Idle, &input, &lit(), 40, 2);
        assert!(colored(&buf));
    }

    // ── the scrollback ──

    #[test]
    fn the_header_line_names_the_folder_the_branch_and_the_model() {
        // Too narrow for the raven.
        assert_eq!(event_text(&header(), &lit(), 40), ["hugin · demo · main, 2 changed · qwen"]);
        let lines = scrollback(&header(), &lit(), 40);
        assert_eq!(lines[0].spans[0].style.fg, Some(hex("#8b7cf8")));
        assert_eq!(lines[0].spans[1].style.fg, None);
    }

    fn start(theme: &Theme, branch: Option<&str>, shell: &str, width: u16) -> Vec<Line<'static>> {
        start_screen(theme, "C:\\work\\demo", branch, branch.map(|_| 2), "qwen", shell, width)
    }

    #[test]
    fn half_blocks_put_two_pixel_rows_in_one_terminal_row() {
        let cells = half_blocks(&["#.w.", ".b-."]);
        assert_eq!(cells.len(), 1);
        let dark = hex("#8b7cf8");
        let light = hex("#5a47d1");
        let row = &cells[0];
        assert_eq!(row[0], ('▀', Style::default().fg(dark)));
        assert_eq!(row[1], ('▄', Style::default().fg(light)));
        assert_eq!(row[2], ('▀', Style::default().fg(Color::Rgb(0x6a, 0x58, 0xe6)).bg(Color::Rgb(0x3c, 0x2f, 0x96))));
        assert_eq!(row[3], (' ', Style::default()));
        // An odd last row has an empty bottom.
        assert_eq!(half_blocks(&["#", ".", "o"]).len(), 2);
        assert_eq!(half_blocks(&["#", ".", "o"])[1][0], ('▀', Style::default().fg(Color::Rgb(0xec, 0xeb, 0xe6))));
    }

    #[test]
    fn the_raven_is_44_by_22_pixels_in_11_rows() {
        assert!(RAVEN.iter().all(|r| r.chars().count() == ART_WIDTH));
        assert_eq!(half_blocks(&RAVEN).len(), 11);
    }

    #[test]
    fn the_start_screen_sets_the_text_beside_the_raven() {
        let lines = start(&lit(), Some("main"), "Git Bash", 100);
        let rows = text_of(&shown(&lines, 100));
        assert_eq!(rows.len(), 12);
        assert_eq!(rows[11], "");
        let side = |r: usize| rows[r].chars().skip(MARGIN + ART_WIDTH + GAP).collect::<String>();
        assert_eq!(side(3), "Hugin");
        assert_eq!(side(4), "thought, sent out to work");
        assert_eq!(side(6), "demo · main, 2 changed");
        assert_eq!(side(7), "qwen · Git Bash");
        for r in [0, 1, 2, 5, 8, 9, 10] {
            assert_eq!(side(r).trim(), "", "row {r}");
        }
        // Two spaces of margin, and the art starts there.
        assert!(rows[1].starts_with("  "));
        assert_eq!(rows[1].chars().nth(MARGIN + 31), Some('▀'));
        // Styles: Hugin bold violet, the rest of the place line dim.
        let hugin = lines[3].spans.iter().find(|s| s.content == "Hugin").unwrap();
        assert_eq!(hugin.style.fg, Some(hex("#8b7cf8")));
        assert!(hugin.style.add_modifier.contains(Modifier::BOLD));
        let dim = lines[6].spans.last().unwrap();
        assert!(dim.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(lines[6].spans[lines[6].spans.len() - 2].content, "demo");
    }

    #[test]
    fn outside_git_and_without_a_shell_the_text_has_no_branch_and_no_shell() {
        let rows = text_of(&shown(&start(&lit(), None, "", 100), 100));
        assert!(rows[6].ends_with("demo"));
        assert!(rows[7].ends_with("qwen"));
    }

    #[test]
    fn too_narrow_for_the_text_beside_it_the_art_comes_first_and_the_text_under_it() {
        // 2 + 44 + 4 + 25 = 75 is the least for side by side.
        let rows = text_of(&shown(&start(&lit(), Some("main"), "", 74), 74));
        assert_eq!(rows.len(), 11 + 4 + 1);
        assert!(rows[0..11].iter().all(|r| r.chars().count() <= MARGIN + ART_WIDTH));
        assert_eq!(&rows[11..], ["  Hugin", "  thought, sent out to work", "  demo · main, 2 changed", "  qwen", ""]);
        let rows = text_of(&shown(&start(&lit(), Some("main"), "", 75), 75));
        assert_eq!(rows.len(), 12);
    }

    #[test]
    fn under_48_columns_or_without_color_the_start_is_the_one_header_line() {
        let want = ["hugin · demo · main, 2 changed · qwen"];
        assert_eq!(text_of(&shown(&start(&lit(), Some("main"), "", 47), 47)), want);
        assert_eq!(text_of(&shown(&start(&mono(), Some("main"), "", 120), 120)), want);
        assert!(!colored(&shown(&start(&mono(), Some("main"), "", 120), 120)));
        assert_eq!(start(&lit(), Some("main"), "", 48).len(), 16);
    }

    #[test]
    fn line_mode_keeps_its_plain_header() {
        assert_eq!(view::header("C:\\work\\demo", Some("main"), Some(2), "qwen", ""), "hugin · demo · main, 2 changed · qwen");
        assert_eq!(view::header("C:\\work\\demo", None, None, "qwen", "bash"), "hugin · demo · qwen · bash");
    }

    #[test]
    fn a_prompt_has_its_marker_and_indents_its_other_lines() {
        let e = Event::Prompt("fix it\nthen test".into());
        assert_eq!(event_text(&e, &lit(), 80), ["› fix it", "  then test"]);
        assert_eq!(scrollback(&e, &lit(), 80)[0].spans[0].style.fg, Some(hex("#8b7cf8")));
    }

    #[test]
    fn a_tool_line_is_a_bullet_a_verb_a_target_and_an_outcome() {
        let e = Event::Tool(ToolLine { name: "edit".into(), target: "a.rs +1 −1".into(), outcome: "applied".into() });
        assert_eq!(event_text(&e, &lit(), 80), ["● edit a.rs +1 −1 → applied"]);
        let bare = Event::Tool(ToolLine { name: "plan".into(), target: String::new(), outcome: String::new() });
        assert_eq!(event_text(&bare, &lit(), 80), ["● plan"]);
        assert_eq!(scrollback(&e, &lit(), 80)[0].spans[0].style.fg, Some(hex("#8b7cf8")));
    }

    fn edit_approval() -> Approval {
        Approval {
            name: "edit".into(),
            target: "a.rs +1 −1".into(),
            detail: vec![" fn a() {".into(), "-    old();".into(), "+    new();".into(), "⋯".into(), " }".into()],
        }
    }

    #[test]
    fn an_approval_shows_its_diff_with_a_separator_between_hunks() {
        let lines = approval(&edit_approval(), &lit(), 80);
        let want = ["? edit a.rs +1 −1", "   fn a() {", "  -    old();", "  +    new();", "  ⋯", "   }"];
        assert_eq!(text_of(&shown(&lines, 80)), want);
        let fg = |n: usize| lines[n].spans[0].style.fg;
        assert_eq!(fg(0), Some(hex("#8b7cf8")));
        assert_eq!((fg(2), fg(3)), (Some(Color::Red), Some(Color::Green)));
        assert_eq!(fg(1), None);
        assert!(lines[1].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_plan_is_checkboxes_an_answer_is_plain_and_an_error_and_a_stop_are_marked() {
        let plan = Event::Plan(vec![step("read it", PlanStatus::Done), step("fix it", PlanStatus::Doing), step("test it", PlanStatus::Pending)]);
        assert_eq!(event_text(&plan, &lit(), 80), ["[x] read it", "[ ] fix it", "[ ] test it"]);
        assert_eq!(event_text(&Event::Answer("one\n\ntwo".into()), &lit(), 80), ["one", "", "two"]);
        assert_eq!(event_text(&Event::Error("no model".into()), &lit(), 80), ["! no model"]);
        assert_eq!(event_text(&Event::Stopped, &lit(), 80), ["· stopped"]);
        assert!(scrollback(&Event::Status("x".into()), &lit(), 80).is_empty());
        assert!(scrollback(&Event::TurnEnd, &lit(), 80).is_empty());
    }

    #[test]
    fn long_lines_wrap_at_spaces_and_cut_words_longer_than_the_line() {
        let e = Event::Answer("aaa bbb ccc ddd".into());
        assert_eq!(event_text(&e, &lit(), 8), ["aaa bbb", "ccc ddd"]);
        assert_eq!(event_text(&Event::Answer("abcdefghij".into()), &lit(), 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn without_color_no_scrollback_line_has_any() {
        let events = [
            header(),
            Event::Prompt("hi".into()),
            Event::Tool(ToolLine { name: "read".into(), target: "a.rs".into(), outcome: "2 lines".into() }),
            Event::Stopped,
        ];
        for e in &events {
            let lines = scrollback(e, &mono(), 80);
            assert!(!colored(&shown(&lines, 80)), "{e:?}");
        }
        let lines = approval(&edit_approval(), &mono(), 80);
        assert!(!colored(&shown(&lines, 80)));
        let lines = approval(&edit_approval(), &lit(), 80);
        assert!(colored(&shown(&lines, 80)));
    }

    #[test]
    fn no_color_set_to_anything_but_nothing_turns_color_off() {
        assert!(Theme::from_no_color(None).color);
        assert!(Theme::from_no_color(Some("")).color);
        assert!(!Theme::from_no_color(Some("1")).color);
        assert!(!Theme::from_no_color(Some("0")).color);
    }
}
