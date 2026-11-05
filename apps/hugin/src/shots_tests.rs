//! The pictures in the README: Hugin's four screens, drawn by its own code from scripted data —
//! no Ollama, no model — and written as HTML for `tools/hugin-shots.mjs` to photograph.
//!
//! The two interactive screens are the real first frame and a real frame of a scripted session
//! (inline_tests.rs's machinery, read from a `TestBackend` at the moment it is wanted). The two
//! reports are the text `hugin -p` and `hugin --ask` print, from their own `to_text`, set under the
//! command that would have printed it.
//!
//! **The test always draws the four, so a screen that stops drawing fails CI**; it writes the HTML
//! only when `HUGIN_SHOTS_DIR` is set, which is what the script does.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bifrost_core::agent::round::{Reply, Round};
use bifrost_core::agent::run::Ran;
use bifrost_core::agent::ticket::{DiffView, PendingCall};
use bifrost_core::edit::Hunk;
use bifrost_core::tools::Lines;
use crossterm::event::{Event as Term, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use serde_json::json;
use unicode_width::UnicodeWidthStr;

use crate::ask::LookedAt;
use crate::inline::{At, Keys, Worker, run};
use crate::inline_tests::{Peek, session};
use crate::print::{CheckReport, FileChange};
use crate::render::Theme;
use crate::session::Session;
use crate::tests::{dl, raw, round_of};
use crate::ui::Event;

const TASK: &str = "show the check's timeout in tenths of a second";
const CHECK: &str = "cargo test -p bifrost-core";
const FILE: &str = "crates/bifrost-core/src/check.rs";
const MODEL: &str = "qwen3.6:35b-a3b";
const PROMPT: &str = r"PS C:\src\bifrost> ";

// ── the interactive screens ─────────────────────────────────────────────────

fn header() -> Event {
    Event::Header {
        folder: r"C:\src\bifrost".into(),
        branch: Some("main".into()),
        changed: Some(3),
        model: MODEL.into(),
        shell: "powershell".into(),
    }
}

/// Stands where the keyboard does: types `keys` while idle, and when the interface is `want` — in the
/// state the picture is of — keeps the screen as drawn and leaves, by whatever door that state has.
struct Camera {
    screen: Peek,
    keys: VecDeque<Term>,
    want: At,
    calls: u32,
    started: Instant,
    shot: Option<(Buffer, Position)>,
}

fn key(code: KeyCode, mods: KeyModifiers) -> Term {
    Term::Key(KeyEvent::new(code, mods))
}

impl Keys for Camera {
    fn next(&mut self, wait: Duration, at: At) -> io::Result<Option<Term>> {
        assert!(
            self.started.elapsed() < Duration::from_secs(20),
            "the scripted session hung"
        );
        self.calls += 1;
        if self.shot.is_none() {
            if at == At::Idle
                && let Some(k) = self.keys.pop_front()
            {
                return Ok(Some(k));
            }
            // The first call comes before the first frame; every later one comes after one.
            if at == self.want && self.keys.is_empty() && self.calls > 1 {
                self.shot = Some(self.screen.with(|b| {
                    let at = b.get_cursor_position().unwrap_or_default();
                    (b.buffer().clone(), at)
                }));
            }
        }
        if self.shot.is_some() {
            match at {
                // Esc at an approval declines it and stops the turn, which leaves the way to Ctrl+D.
                At::Approval => return Ok(Some(key(KeyCode::Esc, KeyModifiers::NONE))),
                At::Idle => return Ok(Some(key(KeyCode::Char('d'), KeyModifiers::CONTROL))),
                At::Turn => {}
            }
        }
        std::thread::sleep(wait.min(Duration::from_millis(2)));
        Ok(None)
    }
}

/// The screen of a session, `width` by `height`, at the moment the interface is `want`, after
/// `typed` and Enter.
fn film(
    (width, height): (u16, u16),
    header: Option<Event>,
    typed: &str,
    want: At,
    make: impl FnOnce() -> Session + Send + 'static,
) -> Buffer {
    let screen = Peek::new(width, height);
    let (_keep, notices) = mpsc::channel();
    let mut keys: VecDeque<Term> = typed
        .chars()
        .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE))
        .collect();
    if !typed.is_empty() {
        keys.push_back(key(KeyCode::Enter, KeyModifiers::NONE));
    }
    let mut camera = Camera {
        screen: screen.clone(),
        keys,
        want,
        calls: 0,
        started: Instant::now(),
        shot: None,
    };
    run(
        screen,
        &mut camera,
        Theme::new(true),
        header,
        notices,
        Worker::spawn(make),
    )
    .unwrap();
    let (mut buf, cursor) = camera
        .shot
        .expect("the interface never came to the state to photograph");
    // A terminal draws its cursor as a block; the buffer only knows where it is.
    if cursor.x < buf.area.width && cursor.y < buf.area.height {
        buf.set_style(Rect::new(cursor.x, cursor.y, 1, 1), Modifier::REVERSED);
    }
    buf
}

fn start_screen() -> Buffer {
    film(
        (100, 9),
        Some(header()),
        "",
        At::Idle,
        session(vec![], vec![], Duration::ZERO),
    )
}

/// The model searches, reads the lines around the answer, and asks to change one.
fn session_rounds() -> Vec<Result<Round, String>> {
    let search = round_of(Reply::Auto {
        name: "search".into(),
        path: "\"time limit\"".into(),
        range: None,
        result: format!(
            "4 matches for \"time limit\" in 1 file:\n{FILE}:1024: Ended::TimedOut => format!(\"ran for\")\n"
        ),
        ok: true,
        stopped: false,
        plan: None,
        raw_call: raw("search", json!({ "text": "time limit" })),
    });
    let lines: Vec<String> = (1000..=1040).map(|n| format!("line {n}")).collect();
    let read = round_of(Reply::Auto {
        name: "read_file".into(),
        path: FILE.into(),
        range: Some(Lines {
            start: 1000,
            end: Some(1040),
        }),
        result: format!("[lines 1000–1040 of 2314]\n{}", lines.join("\n")),
        ok: true,
        stopped: false,
        plan: None,
        raw_call: raw("read_file", json!({ "path": FILE })),
    });
    let hunk = Hunk {
        lines: vec![
            dl(
                " ",
                r#"        Ended::Exited(None) => format!("`{name}` ended without an exit code after {secs:.1} s."),"#,
            ),
            dl(
                "-",
                r#"        Ended::TimedOut => format!("`{name}` ran for {secs:.0} s, past its time limit, and was stopped."),"#,
            ),
            dl(
                "+",
                r#"        Ended::TimedOut => format!("`{name}` ran for {secs:.1} s, past its time limit, and was stopped."),"#,
            ),
            dl(
                " ",
                r#"        Ended::Stopped => format!("`{name}` was stopped by the user after {secs:.1} s."),"#,
            ),
        ],
    };
    let edit = round_of(Reply::Call {
        call: PendingCall {
            token: "t-check".into(),
            name: "edit_file".into(),
            path: FILE.into(),
            diff: Some(DiffView {
                hunks: vec![hunk],
                added: 1,
                removed: 1,
                create: false,
            }),
            range: None,
            changed_build_files: Vec::new(),
            to: None,
            folders: Vec::new(),
            size: None,
        },
        raw_call: raw("edit_file", json!({ "path": FILE })),
    });
    vec![search, read, edit]
}

fn session_screen() -> Buffer {
    let runs = vec![Ok(Ran::text(format!("Edited {FILE}: 1 line removed, 1 added.")))];
    film(
        (112, 17),
        Some(header()),
        TASK,
        At::Approval,
        session(session_rounds(), runs, Duration::ZERO),
    )
}

// ── the two reports ─────────────────────────────────────────────────────────

/// `text` as a console shows it under `command`: the prompt, what was typed, the output, and a new
/// prompt with the cursor on it, as wide as its longest line. A line wider than `most` columns would
/// wrap in a real console, and the picture would be of something else, so it is refused.
fn console(command: &str, text: &str, most: u16) -> Buffer {
    let mut lines = vec![format!("{PROMPT}{command}")];
    lines.extend(text.lines().map(str::to_string));
    lines.push(String::new());
    lines.push(PROMPT.to_string());
    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let cols = u16::try_from(longest).unwrap();
    assert!(cols <= most, "a line is wider than {most}: {lines:?}");
    let mut buf = Buffer::empty(Rect::new(0, 0, cols, u16::try_from(lines.len()).unwrap()));
    let dim = Style::default().add_modifier(Modifier::DIM);
    let prompt = u16::try_from(PROMPT.len()).unwrap();
    for (y, l) in (0..).zip(&lines) {
        buf.set_string(0, y, l, Style::default());
    }
    buf.set_style(Rect::new(0, 0, prompt, 1), dim);
    let last = buf.area.height - 1;
    buf.set_style(Rect::new(0, last, prompt, 1), dim);
    buf.set_style(Rect::new(prompt, last, 1, 1), Modifier::REVERSED);
    buf
}

fn run_report() -> crate::print::Report {
    let patch = format!(
        "diff --git a/{FILE} b/{FILE}\n\
         index 8c1d2e4..5a7f9b0 100644\n\
         --- a/{FILE}\n\
         +++ b/{FILE}\n\
         @@ -1021,7 +1021,7 @@ pub fn for_model(argv: &[String], outcome: &Outcome) -> String {{\n"
    ) + concat!(
        r#"         Ended::Exited(Some(0)) => format!("`{name}` passed (exit 0, {secs:.1} s)."),"#,
        "\n",
        r#"         Ended::Exited(Some(code)) => format!("`{name}` failed (exit {code}, {secs:.1} s)."),"#,
        "\n",
        r#"         Ended::Exited(None) => format!("`{name}` ended without an exit code after {secs:.1} s."),"#,
        "\n",
        r#"-        Ended::TimedOut => format!("`{name}` ran for {secs:.0} s, past its time limit, and was stopped."),"#,
        "\n",
        r#"+        Ended::TimedOut => format!("`{name}` ran for {secs:.1} s, past its time limit, and was stopped."),"#,
        "\n",
        r#"         Ended::Stopped => format!("`{name}` was stopped by the user after {secs:.1} s."),"#,
        "\n",
        r#"         Ended::DiskFull => format!("`{name}` was stopped after {secs:.1} s because the disk was nearly full."),"#,
        "\n",
    );
    crate::print::Report {
        task: TASK.into(),
        folder: r"C:\src\bifrost".into(),
        repo_root: r"C:\src\bifrost".into(),
        base: "124fad7".into(),
        model: MODEL.into(),
        answer: "The timeout message in check.rs now gives the time in tenths of a second, as the other\n\
                 outcomes do. The tests for the check runner pass."
            .into(),
        patch: patch.into_bytes(),
        patch_file: Some(PathBuf::from(r"C:\Temp\hugin-p\patches\20261104-143012-18840.patch")),
        files: vec![FileChange {
            path: FILE.into(),
            from: None,
            status: "modified",
            by: "edit",
            added: Some(1),
            deleted: Some(1),
        }],
        check: Some(CheckReport {
            command: CHECK.into(),
            passed: Some(true),
            exit_code: Some(0),
            seconds: 19.6,
            timed_out: false,
            blocked: None,
            reused: false,
            cut_short: false,
            output_tail: String::new(),
            not_run: None,
        }),
        seconds: 48.3,
        time: 540,
        ..Default::default()
    }
}

/// The model's own answer, word for word and line for line, as the README quotes it.
const QUESTION: &str = "where are the run memory limits set?";
const ANSWER: &str = "The limits are constants in crates/bifrost-core/src/check.rs: PROCESS_MEMORY_LIMIT is 8 GB for\n\
any one process, and RUN_MEMORY_LIMIT is 24 GB for all the processes of a run together. Both are\n\
enforced through a Windows job object that the check runner puts every process in.";

fn ask_report() -> crate::ask::Report {
    crate::ask::Report {
        folder: r"C:\src\bifrost".into(),
        model: MODEL.into(),
        answer: ANSWER.into(),
        looked_at: vec![LookedAt {
            path: FILE.into(),
            lines: Some("50-60".into()),
        }],
        seconds: 14.2,
        time: 300,
        ..Default::default()
    }
}

// ── the page ────────────────────────────────────────────────────────────────

type Rgb = (u8, u8, u8);

/// Windows Terminal's Campbell scheme: what the terminal's own named colors are.
const ANSI: [Rgb; 16] = [
    (0x0c, 0x0c, 0x0c),
    (0xc5, 0x0f, 0x1f),
    (0x13, 0xa1, 0x0e),
    (0xc1, 0x9c, 0x00),
    (0x00, 0x37, 0xda),
    (0x88, 0x17, 0x98),
    (0x3a, 0x96, 0xdd),
    (0xcc, 0xcc, 0xcc),
    (0x76, 0x76, 0x76),
    (0xe7, 0x48, 0x56),
    (0x16, 0xc6, 0x0c),
    (0xf9, 0xf1, 0xa5),
    (0x3b, 0x78, 0xff),
    (0xb4, 0x00, 0x9e),
    (0x61, 0xd6, 0xd6),
    (0xf2, 0xf2, 0xf2),
];
const FG: Rgb = ANSI[7];
const BG: Rgb = ANSI[0];

fn rgb_of(color: Color, default: Rgb) -> Rgb {
    let named = |n: usize| ANSI[n];
    match color {
        Color::Reset => default,
        Color::Black => named(0),
        Color::Red => named(1),
        Color::Green => named(2),
        Color::Yellow => named(3),
        Color::Blue => named(4),
        Color::Magenta => named(5),
        Color::Cyan => named(6),
        Color::Gray => named(7),
        Color::DarkGray => named(8),
        Color::LightRed => named(9),
        Color::LightGreen => named(10),
        Color::LightYellow => named(11),
        Color::LightBlue => named(12),
        Color::LightMagenta => named(13),
        Color::LightCyan => named(14),
        Color::White => named(15),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i @ 0..=15) => named(usize::from(i)),
        Color::Indexed(i @ 16..=231) => {
            let n = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (step(n / 36), step(n / 6 % 6), step(n % 6))
        }
        Color::Indexed(i) => {
            let grey = 8 + 10 * (i - 232);
            (grey, grey, grey)
        }
    }
}

/// `a` with `weight` of `b` mixed in.
fn mix(a: Rgb, b: Rgb, weight: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - weight) + f32::from(y) * weight).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// How a cell looks once reverse video and dim are worked out: dim is the text a little toward the
/// ground it is on, as a terminal draws it.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
}

fn look(cell: &Cell) -> Look {
    let m = cell.modifier;
    let (mut fg, mut bg) = (rgb_of(cell.fg, FG), rgb_of(cell.bg, BG));
    if m.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if m.contains(Modifier::DIM) {
        fg = mix(fg, bg, 0.4);
    }
    Look {
        fg,
        bg,
        bold: m.contains(Modifier::BOLD),
        italic: m.contains(Modifier::ITALIC),
        underline: m.contains(Modifier::UNDERLINED),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn style_of(look: Look) -> String {
    let mut css = String::new();
    if look.fg != FG {
        css.push_str(&format!("color:{};", hex(look.fg)));
    }
    if look.bg != BG {
        css.push_str(&format!("background:{};", hex(look.bg)));
    }
    if look.bold {
        css.push_str("font-weight:700;");
    }
    if look.italic {
        css.push_str("font-style:italic;");
    }
    if look.underline {
        css.push_str("text-decoration:underline;");
    }
    css
}

/// One row of the buffer: runs of cells that look alike, each in a box as wide as it has columns.
/// The half blocks are not letters but two colors, top and bottom, so the art meets from one row to
/// the next whatever the font's block glyphs do.
fn row_html(buf: &Buffer, y: u16) -> String {
    let mut cells: Vec<(String, usize, Look)> = Vec::new();
    let mut x = 0;
    while x < buf.area.width {
        let cell = &buf[(x, y)];
        let symbol = if cell.symbol().is_empty() { " " } else { cell.symbol() };
        // A wide character takes the cells after it too.
        let cols = symbol.width().max(1);
        cells.push((symbol.to_string(), cols, look(cell)));
        x += u16::try_from(cols).unwrap();
    }
    while cells
        .last()
        .is_some_and(|(s, _, l)| s == " " && l.bg == BG && !l.underline)
    {
        cells.pop();
    }

    let mut out = String::new();
    let mut run: Option<(String, usize, Look)> = None;
    let flush = |run: &mut Option<(String, usize, Look)>, out: &mut String| {
        if let Some((text, cols, look)) = run.take() {
            out.push_str(&format!(
                "<span class=\"c\" style=\"width:{cols}ch;{}\">{}</span>",
                style_of(look),
                escape(&text)
            ));
        }
    };
    for (symbol, cols, look) in cells {
        if let Some(block) = symbol.chars().next().filter(|c| matches!(c, '▀' | '▄' | '█')) {
            flush(&mut run, &mut out);
            let (fg, bg) = (hex(look.fg), hex(look.bg));
            let ground = match block {
                '▀' => format!("linear-gradient({fg} 50%,{bg} 50%)"),
                '▄' => format!("linear-gradient({bg} 50%,{fg} 50%)"),
                _ => fg,
            };
            out.push_str(&format!(
                "<span class=\"c\" style=\"width:1ch;background:{ground}\"></span>"
            ));
        } else {
            match &mut run {
                Some((text, n, l)) if *l == look => {
                    text.push_str(&symbol);
                    *n += cols;
                }
                _ => {
                    flush(&mut run, &mut out);
                    run = Some((symbol, cols, look));
                }
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

const CSS: &str = r#"html,body{margin:0;background:#0c0c0c}
#w{display:block;width:max-content;background:#0c0c0c;color:#cccccc;font:16px/20px "Cascadia Mono","Cascadia Code",Consolas,monospace;font-variant-ligatures:none;font-feature-settings:"liga" 0,"calt" 0}
.bar{position:relative;height:32px;background:#1f1f1f;display:flex;align-items:center;padding:0 14px;gap:8px}
.bar i{width:11px;height:11px;border-radius:50%;background:#3d3d3d}
.bar b{position:absolute;left:0;right:0;text-align:center;font:500 12px "Segoe UI Variable Text","Segoe UI",sans-serif;color:#8c8c8c}
.t{padding:14px 18px 18px}
.r{height:20px;white-space:pre;overflow:hidden}
.c{display:inline-block;vertical-align:top;height:20px;overflow:hidden}"#;

/// The buffer as a page that is nothing but a terminal window. The script in it writes the window's
/// size into the page (`data-size`), which is how `tools/hugin-shots.mjs` knows what to photograph.
fn page(title: &str, buf: &Buffer) -> String {
    let rows: String = (0..buf.area.height)
        .map(|y| format!("<div class=\"r\">{}</div>\n", row_html(buf, y)))
        .collect();
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{title}</title>\n<style>\n{CSS}\n.t{{width:{}ch}}\n</style></head>\n\
         <body><div id=\"w\"><div class=\"bar\"><i></i><i></i><i></i><b>{title}</b></div>\n<div class=\"t\">\n{rows}</div></div>\n\
         <script>addEventListener('load',()=>{{const r=document.getElementById('w').getBoundingClientRect();\
         document.documentElement.dataset.size=Math.ceil(r.width)+','+Math.ceil(r.height)}})</script>\n</body></html>\n",
        buf.area.width
    )
}

fn text_of(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── the test ────────────────────────────────────────────────────────────────

#[test]
fn readme_screens_render() {
    let run_cmd = format!("hugin -p \"{TASK}\" --check \"{CHECK}\"");
    let ask_cmd = format!("hugin --ask \"{QUESTION}\"");
    let screens = [
        ("hugin-start", "hugin", start_screen()),
        ("hugin-session", "hugin", session_screen()),
        (
            "hugin-run",
            "Windows PowerShell",
            console(&run_cmd, &run_report().to_text(), 120),
        ),
        (
            "hugin-ask",
            "Windows PowerShell",
            console(&ask_cmd, &ask_report().to_text(), 100),
        ),
    ];

    let text: Vec<String> = screens.iter().map(|(_, _, buf)| text_of(buf)).collect();
    assert!(
        text[0].contains("Hugin") && text[0].contains("thought, sent out to work"),
        "{}",
        text[0]
    );
    assert!(text[0].contains(MODEL) && text[0].contains("powershell"), "{}", text[0]);
    assert!(text[1].contains(&format!("› {TASK}")), "{}", text[1]);
    assert!(
        text[1].contains("● search") && text[1].contains("● read"),
        "{}",
        text[1]
    );
    assert!(
        text[1].contains("? edit crates/bifrost-core/src/check.rs +1 −1"),
        "{}",
        text[1]
    );
    assert!(text[1].contains("approve? [y/n/a]"), "{}", text[1]);
    assert!(
        text[2].contains("hugin: done in 48 s on qwen3.6:35b-a3b"),
        "{}",
        text[2]
    );
    assert!(text[2].contains("to apply: git -C"), "{}", text[2]);
    assert!(
        text[3].contains("hugin: answered in 14 s on qwen3.6:35b-a3b"),
        "{}",
        text[3]
    );
    assert!(
        text[3].contains("looked at:") && text[3].contains("check.rs (lines 50-60)"),
        "{}",
        text[3]
    );
    // The model's answer is quoted as it is, so a change to the formatter cannot reword it.
    assert!(text[3].contains(ANSWER), "{}", text[3]);

    let html: Vec<String> = screens.iter().map(|(_, title, buf)| page(title, buf)).collect();
    for h in &html {
        assert!(h.contains("class=\"r\"") && h.len() > 2000);
    }

    if let Some(dir) = std::env::var_os("HUGIN_SHOTS_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for ((name, _, _), html) in screens.iter().zip(&html) {
            std::fs::write(dir.join(format!("{name}.html")), html).unwrap();
        }
    }
}
