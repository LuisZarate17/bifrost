//! What Hugin's terminal interface looks like, as pure functions: an [`Event`] becomes the
//! rows of the transcript, and the whole screen (the transcript's window, the input between two
//! rules, and the status line under it) is drawn into a ratatui frame. Nothing here touches a
//! terminal, so the tests draw into a `TestBackend` and compare line by line.
//!
//! **Quiet color.** Raven Violet is on the prompt marker `›` (bold, on a gray band that sets the
//! person's own words apart), the word `hugin` in the
//! header, approvals and the `●` of a result, and nowhere else; a diff is green and red,
//! its context dim. `NO_COLOR` removes every color; bold and dim stay.

use bifrost_core::colors::palette;
use bifrost_core::tools::{PlanStatus, PlanStep};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::input::Input;
use crate::transcript::View;
use crate::ui::{Approval, Event, ToolLine};
use crate::view;

/// The most lines the input box grows to.
pub const INPUT_ROWS: usize = 8;

pub const HINT: &str = "enter send · shift+enter new line · esc stop · ctrl+d quit";
/// Added to the hint when there is room: the mouse is the program's, so a selection is made with
/// the shift key held.
pub const SELECT: &str = " · shift+drag select";
/// What the hint line says when the view is scrolled up.
pub const BELOW: &str = "↓ ctrl+end";
pub const BELOW_NEW: &str = "↓ new below · ctrl+end";
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
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    /// Raven Violet, the dark shade.
    fn accent(&self) -> Style {
        self.fg(hex(&palette().hugin.dark))
    }

    /// The band behind a prompt. A dark neutral gray with its own light text, so that it reads the
    /// same on a dark terminal and a light one: on a light one it is a dark strip, never a pale
    /// wash that would hide the text. Without color there is no band.
    fn band(&self) -> Style {
        if self.color {
            Style::default()
                .bg(Color::Rgb(0x2b, 0x2b, 0x33))
                .fg(Color::Rgb(0xe8, 0xe8, 0xee))
        } else {
            Style::default()
        }
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
        Event::Header {
            folder,
            branch,
            changed,
            model,
            shell,
        } => start_screen(theme, folder, branch.as_deref(), *changed, model, shell, width),
        Event::Prompt(text) => return prompt(text, theme, width),
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
            .map(|(n, l)| {
                if n == 0 {
                    Line::from(vec![span("! ", theme.bold()), Span::raw(l.to_string())])
                } else {
                    plain(l)
                }
            })
            .collect(),
        // Marked like an error, and in words: the cross is not the only thing that says so.
        Event::Unfinished(text) => vec![Line::from(vec![span("✗ ", theme.bold()), Span::raw(text.clone())])],
        Event::Stopped => vec![Line::from(span("· stopped", theme.dim()))],
        Event::Status(_) | Event::TurnEnd => Vec::new(),
    };
    wrap(lines, width)
}

/// The person's own words: the marker in the accent and bold on the first row, the rows indented,
/// all of it on the band across the full width.
fn prompt(text: &str, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let lines: Vec<Line<'static>> = text
        .split('\n')
        .enumerate()
        .map(|(n, l)| {
            let lead = if n == 0 {
                span("› ", theme.accent().add_modifier(Modifier::BOLD))
            } else {
                span("  ", Style::default())
            };
            Line::from(vec![lead, Span::raw(l.to_string())])
        })
        .collect();
    wrap(lines, width).into_iter().map(|l| l.style(theme.band())).collect()
}

/// `text` cut to `room` columns, with an ellipsis where it was cut.
fn cut(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > room {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// A prompt as one row, for the top row of the screen once the prompt itself has scrolled off:
/// its lines run together and the end is cut to fit.
pub fn sticky_prompt(text: &str, theme: &Theme, width: u16) -> Line<'static> {
    let flat = text.split('\n').collect::<Vec<_>>().join(" ");
    let lead = span("› ", theme.accent().add_modifier(Modifier::BOLD));
    Line::from(vec![lead, Span::raw(cut(&flat, usize::from(width).saturating_sub(2)))]).style(theme.band())
}

// ── the start screen ────────────────────────────────────────────────────────

/// The raven, one character per pixel: `.` empty, `#` body, `w` wing, `b` bill, `o` the eye, `|` legs, `=` the perch.
pub const RAVEN: [&str; 8] = [
    ".........####....",
    "........##o#bb...",
    "........####bbb..",
    ".......ww###.....",
    ".....wwww###.....",
    "...wwwww###......",
    ".####...|.|......",
    ".....===|=|===...",
];

/// The art is this many columns wide, after a margin of [`MARGIN`] and before [`GAP`].
const ART_WIDTH: usize = 17;
const MARGIN: usize = 2;
const GAP: usize = 4;
/// Narrower than this there is no room for even the art.
const MIN_ART: usize = 21;
/// The art rows (from 0) that the text lines sit beside.
const TEXT_ROWS: [usize; 4] = [0, 1, 2, 3];

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
fn header_line(
    theme: &Theme,
    folder: &str,
    branch: Option<&str>,
    changed: Option<usize>,
    model: &str,
    shell: &str,
) -> Line<'static> {
    let text = view::header(folder, branch, changed, model, shell);
    let rest = text.strip_prefix("hugin").unwrap_or(&text).to_string();
    Line::from(vec![
        span("hugin", theme.accent().add_modifier(Modifier::BOLD)),
        span(rest, theme.dim()),
    ])
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
        Some(g) => vec![
            Span::raw(view::folder_name(folder)),
            span(format!(" · {g}"), theme.dim()),
        ],
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
    let target = if a.target.is_empty() {
        String::new()
    } else {
        format!(" {}", a.target)
    };
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

/// Where the transcript's window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    /// At the end: new rows keep it there.
    End,
    /// Scrolled up, with nothing new below.
    Up,
    /// Scrolled up, and more has been said since.
    Behind,
}

/// The line under the input, `width` columns wide.
pub fn status_line(status: &Status, theme: &Theme, width: u16, follow: Follow) -> Line<'static> {
    let below = match follow {
        Follow::End => None,
        Follow::Up => Some(BELOW),
        Follow::Behind => Some(BELOW_NEW),
    };
    match (status, below) {
        (Status::Approving, _) => Line::from(span(ASK, theme.accent())),
        (Status::Idle, Some(b)) => Line::from(span(b, theme.dim())),
        (Status::Idle, None) => {
            // The indent takes two columns.
            let room = usize::from(width) >= 2 + HINT.width() + SELECT.width();
            Line::from(span(
                if room {
                    format!("{HINT}{SELECT}")
                } else {
                    HINT.to_string()
                },
                theme.dim(),
            ))
        }
        (Status::Working(what, secs), below) => {
            let tail = below.map(|b| format!("  {b}")).unwrap_or_default();
            Line::from(span(format!("{what} · {secs}s{tail}"), theme.dim()))
        }
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
    let mut out = Rows {
        rows: Vec::new(),
        cursor: (0, 0),
    };
    for (n, line) in input.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let first_row = out.rows.len();
        let pieces = chars
            .chunks(chunk)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>();
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

/// The height of the live box: a rule, the input's rows up to [`INPUT_ROWS`], a rule and the
/// status line. The box sits on the last rows of the screen; the transcript has the rest.
pub fn live_height(input: &Input, width: u16) -> u16 {
    3 + rows(input, width).rows.len().min(INPUT_ROWS) as u16
}

/// A rule across the box. It stops one column short of the edge, so that nothing is written to
/// the last column, where some terminals wrap.
fn rule(width: u16, theme: &Theme) -> Line<'static> {
    Line::from(span("─".repeat(usize::from(width).saturating_sub(1)), theme.dim()))
}

/// Draw the live box on the last rows of `area`: a rule, the input, a rule and the status line,
/// indented. Returns where the cursor is.
pub fn draw_live(
    f: &mut Frame,
    area: Rect,
    status: &Status,
    follow: Follow,
    input: &Input,
    theme: &Theme,
) -> Option<Position> {
    let laid = rows(input, area.width);
    let shown = laid
        .rows
        .len()
        .min(INPUT_ROWS)
        .min(usize::from(area.height).saturating_sub(3));
    // The window of rows that keeps the cursor in view.
    let from = (laid.cursor.0 + 1)
        .saturating_sub(shown)
        .min(laid.rows.len().saturating_sub(shown));
    let mut lines: Vec<Line> = vec![rule(area.width, theme)];
    lines.extend(laid.rows.iter().enumerate().skip(from).take(shown).map(|(n, text)| {
        // The marker is on the input's first row only.
        let lead = if n == 0 {
            span("› ", theme.accent())
        } else {
            span("  ", Style::default())
        };
        Line::from(vec![lead, Span::raw(text.clone())])
    }));
    lines.push(rule(area.width, theme));
    let mut hint = vec![span("  ", Style::default())];
    hint.extend(status_line(status, theme, area.width, follow).spans);
    lines.push(Line::from(hint));

    // An area too short for the whole box shows its bottom rows.
    let cut = lines.len().saturating_sub(usize::from(area.height));
    let height = (lines.len() - cut) as u16;
    let top = area.bottom() - height;
    f.render_widget(Paragraph::new(lines.split_off(cut)), Rect { y: top, height, ..area });
    let (row, col) = laid.cursor;
    // The cursor's line in the box: the top rule, then the rows from `from`.
    let line = (row + 1).checked_sub(from).filter(|&n| n <= shown && n >= cut.max(1))?;
    let at = Position::new(area.x + 2 + col as u16, top + (line - cut) as u16);
    f.set_cursor_position(at);
    Some(at)
}

/// Draw the whole screen: the pinned prompt (if any) and the transcript's rows from the top, and
/// the live box on the last rows. The caller has made `view` for the rows the box leaves.
pub fn draw_screen(
    f: &mut Frame,
    view: &View,
    status: &Status,
    follow: Follow,
    input: &Input,
    theme: &Theme,
) -> Option<Position> {
    let area = f.area();
    let boxed = live_height(input, area.width).min(area.height);
    let room = area.height - boxed;
    for (y, line) in (area.y..).zip(view.sticky.iter().chain(&view.rows).take(usize::from(room))) {
        f.render_widget(line.clone(), Rect { y, height: 1, ..area });
    }
    draw_live(
        f,
        Rect {
            y: area.y + room,
            height: boxed,
            ..area
        },
        status,
        follow,
        input,
        theme,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{Said, Transcript};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;

    fn lit() -> Theme {
        Theme::new(true)
    }

    fn mono() -> Theme {
        Theme::new(false)
    }

    /// Paint lines into a buffer the height of the lines, a row each, as the screen does.
    fn paint(lines: &[Line<'static>], buf: &mut Buffer) {
        for (y, line) in (buf.area.y..).zip(lines) {
            line.clone().render(
                Rect {
                    y,
                    height: 1,
                    ..buf.area
                },
                buf,
            );
        }
    }

    /// A buffer's rows as text, with the blanks at the end of each cut off.
    fn text_of(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
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
        term.draw(|f| {
            draw_live(f, f.area(), status, Follow::End, input, theme);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (text_of(&buf), buf)
    }

    fn colored(buf: &Buffer) -> bool {
        buf.content()
            .iter()
            .any(|c| c.fg != Color::Reset || c.bg != Color::Reset)
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
        PlanStep {
            text: text.into(),
            status,
        }
    }

    // ── the live area ──

    /// A rule across a screen `w` wide: one column short of the edge.
    fn bar(w: u16) -> String {
        "─".repeat(w as usize - 1)
    }

    #[test]
    fn idle_the_box_is_a_rule_an_empty_prompt_a_rule_and_a_quiet_hint() {
        let (rows, _) = live(&Status::Idle, &Input::default(), &lit(), 70, 4);
        assert_eq!(rows, [bar(70), "›".into(), bar(70), format!("  {HINT}")]);
        assert_eq!(live_height(&Input::default(), 70), 4);
    }

    #[test]
    fn the_box_sits_on_the_last_rows_and_the_rows_above_it_are_blank() {
        let (rows, _) = live(&Status::Idle, &Input::default(), &lit(), 70, 7);
        assert_eq!(
            rows,
            ["", "", "", bar(70).as_str(), "›", &bar(70), &format!("  {HINT}")]
        );
    }

    #[test]
    fn the_rules_are_dim_and_plain_characters_without_color() {
        let (rows, buf) = live(&Status::Idle, &Input::default(), &mono(), 30, 4);
        assert_eq!(rows[0], bar(30));
        assert!(buf[(0, 0)].modifier.contains(Modifier::DIM));
        assert_eq!(buf[(29, 0)].symbol(), " ");
        assert!(!colored(&buf));
    }

    #[test]
    fn a_running_turn_says_what_it_waits_for_and_for_how_long() {
        let (rows, _) = live(
            &Status::Working("thinking".into(), 12),
            &Input::default(),
            &lit(),
            40,
            4,
        );
        assert_eq!(rows[3], "  thinking · 12s");
        let (rows, _) = live(
            &Status::Working("running cargo check".into(), 4),
            &Input::default(),
            &lit(),
            40,
            4,
        );
        assert_eq!(rows[3], "  running cargo check · 4s");
        let (rows, _) = live(
            &Status::Working("Starting Ollama…".into(), 0),
            &Input::default(),
            &lit(),
            40,
            4,
        );
        assert_eq!(rows[3], "  Starting Ollama… · 0s");
    }

    #[test]
    fn an_approval_asks_in_the_hint_line_in_the_accent() {
        let (rows, buf) = live(&Status::Approving, &Input::default(), &lit(), 40, 4);
        assert_eq!(rows[3], "  approve? [y/n/a]");
        assert_eq!(buf[(2, 3)].fg, hex("#8b7cf8"));
    }

    #[test]
    fn a_two_line_input_takes_two_rows_and_the_cursor_follows_the_text() {
        let mut input = Input::default();
        type_in(&mut input, "first");
        input.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), false);
        type_in(&mut input, "second");
        assert_eq!(live_height(&input, 62), 5);
        let mut term = Terminal::new(TestBackend::new(62, 5)).unwrap();
        term.draw(|f| {
            draw_live(f, f.area(), &Status::Idle, Follow::End, &input, &lit());
        })
        .unwrap();
        assert_eq!(
            text_of(term.backend().buffer()),
            [
                bar(62),
                "› first".into(),
                "  second".into(),
                bar(62),
                format!("  {HINT}")
            ]
        );
        term.backend_mut().assert_cursor_position((8, 2));
    }

    #[test]
    fn the_input_wraps_and_grows_only_to_eight_rows() {
        let mut input = Input::default();
        type_in(&mut input, &"x".repeat(25));
        // Rows of width - 3 = 7 characters: 25 characters take 4.
        assert_eq!(live_height(&input, 10), 3 + 4);
        let mut input = Input::default();
        input.paste(
            &"line
"
            .repeat(20),
        );
        assert_eq!(live_height(&input, 40), 3 + INPUT_ROWS as u16);
        let (rows, _) = live(&Status::Idle, &input, &lit(), 40, 3 + INPUT_ROWS as u16);
        // The cursor is on the last, empty line, so the window ends there.
        assert_eq!(rows[INPUT_ROWS], "");
        assert_eq!(rows[INPUT_ROWS - 1], "  line");
        assert_eq!(rows[INPUT_ROWS + 1], bar(40));
        // The hint is cut at the width.
        assert!(rows[INPUT_ROWS + 2].starts_with("  enter send"));
    }

    #[test]
    fn a_frame_too_short_for_the_box_shows_its_bottom_rows() {
        let mut input = Input::default();
        type_in(&mut input, "hi");
        let (rows, _) = live(&Status::Idle, &input, &lit(), 70, 2);
        assert_eq!(rows, [bar(70), format!("  {HINT}")]);
        let (rows, _) = live(&Status::Idle, &input, &lit(), 70, 1);
        assert_eq!(rows, [format!("  {HINT}")]);
    }

    #[test]
    fn the_cursor_is_returned_where_it_is_set() {
        let mut input = Input::default();
        type_in(&mut input, "hey");
        let mut term = Terminal::new(TestBackend::new(30, 6)).unwrap();
        let mut at = None;
        term.draw(|f| at = draw_live(f, f.area(), &Status::Idle, Follow::End, &input, &lit()))
            .unwrap();
        assert_eq!(at, Some(Position::new(5, 3)));
        term.backend_mut().assert_cursor_position((5, 3));
    }

    #[test]
    fn without_color_the_live_area_has_none() {
        let mut input = Input::default();
        type_in(&mut input, "hi");
        for status in [Status::Idle, Status::Approving, Status::Working("thinking".into(), 1)] {
            let (_, buf) = live(&status, &input, &mono(), 40, 4);
            assert!(!colored(&buf), "{status:?}");
        }
        let (_, buf) = live(&Status::Idle, &input, &lit(), 40, 4);
        assert!(colored(&buf));
    }

    // ── the scrollback ──

    #[test]
    fn the_header_names_the_folder_the_branch_and_the_model() {
        assert_eq!(
            event_text(&header(), &mono(), 40),
            ["hugin · demo · main, 2 changed · qwen"]
        );
        let lines = scrollback(&header(), &lit(), 20);
        assert_eq!(lines[0].spans[0].style.fg, Some(hex("#8b7cf8")));
        assert_eq!(lines[0].spans[1].style.fg, None);
    }

    #[test]
    fn a_prompt_has_its_marker_and_indents_its_other_lines() {
        let e = Event::Prompt("fix it\nthen test".into());
        assert_eq!(event_text(&e, &lit(), 80), ["› fix it", "  then test"]);
        assert_eq!(scrollback(&e, &lit(), 80)[0].spans[0].style.fg, Some(hex("#8b7cf8")));
    }

    #[test]
    fn a_tool_line_is_a_bullet_a_verb_a_target_and_an_outcome() {
        let e = Event::Tool(ToolLine {
            name: "edit".into(),
            target: "a.rs +1 −1".into(),
            outcome: "applied".into(),
        });
        assert_eq!(event_text(&e, &lit(), 80), ["● edit a.rs +1 −1 → applied"]);
        let bare = Event::Tool(ToolLine {
            name: "plan".into(),
            target: String::new(),
            outcome: String::new(),
        });
        assert_eq!(event_text(&bare, &lit(), 80), ["● plan"]);
        assert_eq!(scrollback(&e, &lit(), 80)[0].spans[0].style.fg, Some(hex("#8b7cf8")));
    }

    fn edit_approval() -> Approval {
        Approval {
            name: "edit".into(),
            target: "a.rs +1 −1".into(),
            detail: vec![
                " fn a() {".into(),
                "-    old();".into(),
                "+    new();".into(),
                "⋯".into(),
                " }".into(),
            ],
        }
    }

    #[test]
    fn an_approval_shows_its_diff_with_a_separator_between_hunks() {
        let lines = approval(&edit_approval(), &lit(), 80);
        let want = [
            "? edit a.rs +1 −1",
            "   fn a() {",
            "  -    old();",
            "  +    new();",
            "  ⋯",
            "   }",
        ];
        assert_eq!(text_of(&shown(&lines, 80)), want);
        let fg = |n: usize| lines[n].spans[0].style.fg;
        assert_eq!(fg(0), Some(hex("#8b7cf8")));
        assert_eq!((fg(2), fg(3)), (Some(Color::Red), Some(Color::Green)));
        assert_eq!(fg(1), None);
        assert!(lines[1].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_plan_is_checkboxes_an_answer_is_plain_and_an_error_and_a_stop_are_marked() {
        let plan = Event::Plan(vec![
            step("read it", PlanStatus::Done),
            step("fix it", PlanStatus::Doing),
            step("test it", PlanStatus::Pending),
        ]);
        assert_eq!(
            event_text(&plan, &lit(), 80),
            ["[x] read it", "[ ] fix it", "[ ] test it"]
        );
        assert_eq!(
            event_text(&Event::Answer("one\n\ntwo".into()), &lit(), 80),
            ["one", "", "two"]
        );
        assert_eq!(event_text(&Event::Error("no model".into()), &lit(), 80), ["! no model"]);
        assert_eq!(event_text(&Event::Stopped, &lit(), 80), ["· stopped"]);
        assert!(scrollback(&Event::Status("x".into()), &lit(), 80).is_empty());
        assert!(scrollback(&Event::TurnEnd, &lit(), 80).is_empty());
    }

    #[test]
    fn a_turn_that_ended_with_its_work_not_done_says_so_in_words_after_a_bold_cross() {
        let e = Event::Unfinished("the check failed — not done".into());
        assert_eq!(event_text(&e, &lit(), 80), ["✗ the check failed — not done"]);
        // Words, not just a mark, and the same with no color at all.
        assert_eq!(event_text(&e, &mono(), 80), ["✗ the check failed — not done"]);
        let lines = scrollback(&e, &mono(), 80);
        assert!(
            lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD),
            "the cross is bold, like an error's mark"
        );
        assert!(!colored(&shown(&lines, 80)));
        assert_eq!(event_text(&e, &lit(), 12), ["✗ the check", "failed — not", "done"]);
    }

    #[test]
    fn long_lines_wrap_at_spaces_and_cut_words_longer_than_the_line() {
        let e = Event::Answer("aaa bbb ccc ddd".into());
        assert_eq!(event_text(&e, &lit(), 8), ["aaa bbb", "ccc ddd"]);
        assert_eq!(
            event_text(&Event::Answer("abcdefghij".into()), &lit(), 4),
            ["abcd", "efgh", "ij"]
        );
    }

    #[test]
    fn without_color_no_scrollback_line_has_any() {
        let events = [
            header(),
            Event::Prompt("hi".into()),
            Event::Tool(ToolLine {
                name: "read".into(),
                target: "a.rs".into(),
                outcome: "2 lines".into(),
            }),
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
        assert_eq!(
            row[2],
            (
                '▀',
                Style::default()
                    .fg(Color::Rgb(0x6a, 0x58, 0xe6))
                    .bg(Color::Rgb(0x3c, 0x2f, 0x96))
            )
        );
        assert_eq!(row[3], (' ', Style::default()));
        // An odd last row has an empty bottom.
        assert_eq!(half_blocks(&["#", ".", "o"]).len(), 2);
        assert_eq!(
            half_blocks(&["#", ".", "o"])[1][0],
            ('▀', Style::default().fg(Color::Rgb(0xec, 0xeb, 0xe6)))
        );
    }

    #[test]
    fn the_raven_is_17_by_8_pixels_in_4_rows() {
        assert!(RAVEN.iter().all(|r| r.chars().count() == ART_WIDTH));
        assert_eq!(half_blocks(&RAVEN).len(), 4);
    }

    #[test]
    fn the_start_screen_sets_the_text_beside_the_raven() {
        let lines = start(&lit(), Some("main"), "Git Bash", 100);
        let rows = text_of(&shown(&lines, 100));
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4], "");
        let side = |r: usize| rows[r].chars().skip(MARGIN + ART_WIDTH + GAP).collect::<String>();
        assert_eq!(side(0), "Hugin");
        assert_eq!(side(1), "thought, sent out to work");
        assert_eq!(side(2), "demo · main, 2 changed");
        assert_eq!(side(3), "qwen · Git Bash");
        // Two spaces of margin, and the art starts there.
        assert!(rows[1].starts_with("  "));
        assert_eq!(rows[0].chars().nth(MARGIN + 9), Some('▀'));
        // Styles: Hugin bold violet, the rest of the place line dim.
        let hugin = lines[0].spans.iter().find(|s| s.content == "Hugin").unwrap();
        assert_eq!(hugin.style.fg, Some(hex("#8b7cf8")));
        assert!(hugin.style.add_modifier.contains(Modifier::BOLD));
        let dim = lines[2].spans.last().unwrap();
        assert!(dim.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(lines[2].spans[lines[2].spans.len() - 2].content, "demo");
    }

    #[test]
    fn outside_git_and_without_a_shell_the_text_has_no_branch_and_no_shell() {
        let rows = text_of(&shown(&start(&lit(), None, "", 100), 100));
        assert!(rows[2].ends_with("demo"));
        assert!(rows[3].ends_with("qwen"));
    }

    #[test]
    fn too_narrow_for_the_text_beside_it_the_art_comes_first_and_the_text_under_it() {
        // 2 + 17 + 4 + 25 = 48 is the least for side by side.
        let rows = text_of(&shown(&start(&lit(), Some("main"), "", 47), 47));
        assert_eq!(rows.len(), 4 + 4 + 1);
        assert!(rows[0..4].iter().all(|r| r.chars().count() <= MARGIN + ART_WIDTH));
        assert_eq!(
            &rows[4..],
            [
                "  Hugin",
                "  thought, sent out to work",
                "  demo · main, 2 changed",
                "  qwen",
                ""
            ]
        );
        let rows = text_of(&shown(&start(&lit(), Some("main"), "", 48), 48));
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn under_21_columns_or_without_color_the_start_is_the_one_header_line() {
        let want = ["hugin · demo · main, 2 changed · qwen"];
        assert_eq!(text_of(&shown(&start(&lit(), Some("main"), "", 20), 60)), want);
        assert_eq!(text_of(&shown(&start(&mono(), Some("main"), "", 120), 120)), want);
        assert!(!colored(&shown(&start(&mono(), Some("main"), "", 120), 120)));
        assert_eq!(start(&lit(), Some("main"), "", 21).len(), 9);
    }

    #[test]
    fn line_mode_keeps_its_plain_header() {
        assert_eq!(
            view::header("C:\\work\\demo", Some("main"), Some(2), "qwen", ""),
            "hugin · demo · main, 2 changed · qwen"
        );
        assert_eq!(
            view::header("C:\\work\\demo", None, None, "qwen", "bash"),
            "hugin · demo · qwen · bash"
        );
    }

    // ── the screen ──

    #[test]
    fn a_prompt_is_on_a_band_across_the_full_width_with_its_marker_in_the_accent_and_bold() {
        let lines = scrollback(
            &Event::Prompt(
                "fix it
then test"
                    .into(),
            ),
            &lit(),
            30,
        );
        let buf = shown(&lines, 30);
        let band = Color::Rgb(0x2b, 0x2b, 0x33);
        for y in 0..2 {
            // The band reaches the last column of each row, and the words have a light color of their own.
            assert_eq!(buf[(29, y)].bg, band);
            assert_eq!(buf[(10, y)].bg, band);
        }
        assert_eq!(buf[(5, 0)].fg, Color::Rgb(0xe8, 0xe8, 0xee));
        assert_eq!(buf[(0, 0)].fg, hex("#8b7cf8"));
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn without_color_a_prompt_is_bold_and_has_no_band() {
        let lines = scrollback(&Event::Prompt("hi".into()), &mono(), 30);
        let buf = shown(&lines, 30);
        assert!(!colored(&buf));
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(text_of(&buf), ["› hi"]);
    }

    #[test]
    fn a_wrapped_prompt_keeps_its_band_on_every_row() {
        let lines = scrollback(&Event::Prompt("aaaa bbbb cccc dddd".into()), &lit(), 12);
        let buf = shown(&lines, 12);
        assert_eq!(lines.len(), 2);
        assert_eq!(buf[(11, 1)].bg, Color::Rgb(0x2b, 0x2b, 0x33));
    }

    #[test]
    fn the_sticky_prompt_is_one_row_on_the_band() {
        let line = sticky_prompt(
            "one two three four
second",
            &lit(),
            12,
        );
        let buf = shown(&[line], 12);
        assert_eq!(text_of(&buf), ["› one two t…"]);
        assert_eq!(buf[(11, 0)].bg, Color::Rgb(0x2b, 0x2b, 0x33));
        assert_eq!(text_of(&shown(&[sticky_prompt("short", &mono(), 12)], 12)), ["› short"]);
    }

    #[test]
    fn the_status_line_says_when_the_view_is_scrolled_up() {
        let said = |status: &Status, follow| status_line(status, &lit(), 100, follow).spans[0].content.to_string();
        assert_eq!(said(&Status::Idle, Follow::Up), "↓ ctrl+end");
        assert_eq!(said(&Status::Idle, Follow::Behind), "↓ new below · ctrl+end");
        assert_eq!(
            said(&Status::Working("thinking".into(), 3), Follow::Behind),
            "thinking · 3s  ↓ new below · ctrl+end"
        );
        // An approval is asked as it always was.
        assert_eq!(said(&Status::Approving, Follow::Behind), ASK);
    }

    #[test]
    fn the_hint_adds_shift_drag_select_only_when_there_is_room_for_it() {
        let said = |w| {
            status_line(&Status::Idle, &lit(), w, Follow::End).spans[0]
                .content
                .to_string()
        };
        assert_eq!(said(80), format!("{HINT}{SELECT}"));
        assert_eq!(said(79), HINT);
    }

    #[test]
    fn the_screen_is_the_pinned_prompt_the_rows_and_the_box_on_the_last_rows() {
        let mut t = Transcript::default();
        t.push(Said::Event(Event::Prompt("ask".into())));
        t.push(Said::Event(Event::Answer(
            "a1
a2
a3
a4"
            .into(),
        )));
        t.lay(30, &mono());
        let view = t.view(4, 0, &mono(), 30);
        let mut term = Terminal::new(TestBackend::new(30, 8)).unwrap();
        term.draw(|f| {
            draw_screen(f, &view, &Status::Idle, Follow::End, &Input::default(), &mono());
        })
        .unwrap();
        let rows = text_of(term.backend().buffer());
        assert_eq!(rows[..4], ["› ask", "a2", "a3", "a4"]);
        assert_eq!(
            rows[4..],
            [
                bar(30),
                "›".to_string(),
                bar(30),
                format!("  {HINT}").chars().take(30).collect::<String>()
            ]
        );
        term.backend_mut().assert_cursor_position((2, 5));
    }
}
