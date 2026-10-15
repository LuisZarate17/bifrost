//! Hugin: a terminal agent that works in a folder, running the turn loop Mimir runs.
//!
//! - [`start`] — what it learns on start: the folder, git, the model, the warm-up
//! - [`session`] — the conversation, turn by turn, and its saved record
//! - [`ui`] — the seam: events out, approvals in; [`line`] is the plain mode behind it
//! - [`inline`], [`input`], [`render`] — the interface for a terminal: its loop, its input box,
//!   and how it looks
//! - [`frontend`], [`view`] — the loop's frontend, and cards put into words
//! - [`stop`] — Stop, for a thread that is not the one running the turn

mod frontend;
mod inline;
#[cfg(test)]
mod inline_tests;
mod input;
mod line;
mod render;
mod session;
mod start;
mod stop;
#[cfg(test)]
mod tests;
mod ui;
mod view;

use std::io::{self, BufReader, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use session::Session;
use ui::Event;

const HELP: &str = "\
hugin - a terminal agent that works in a folder

usage: hugin [--folder <path>] [--line]

  --folder <path>   the folder to work in (default: the current directory)
  --line            plain line mode, as when input or output is not a terminal
  -p                reserved for a later step
  --help            show this help

Type a request at the prompt. Enter sends; Shift+Enter, Alt+Enter, Ctrl+J or a \\ at
the end of the line starts a new line. Esc stops the turn that is running; Ctrl+D or /exit
leaves. In line mode, /stop stops the turn.
";

/// What the command line asked for.
#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    /// `-p`, which a later step builds.
    Print,
    Run { folder: PathBuf, line: bool },
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Cmd, String> {
    let mut folder = None;
    let mut line = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "-p" => return Ok(Cmd::Print),
            "--line" => line = true,
            "--folder" => folder = Some(args.next().ok_or("--folder needs a path")?),
            other => match other.strip_prefix("--folder=") {
                Some(path) => folder = Some(path.to_string()),
                None => return Err(format!("unknown argument: {other}")),
            },
        }
    }
    Ok(Cmd::Run { folder: folder.map_or_else(|| PathBuf::from("."), PathBuf::from), line })
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Ok(Cmd::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Cmd::Print) => {
            eprintln!("hugin -p is not built yet");
            ExitCode::from(2)
        }
        Ok(Cmd::Run { folder, line }) => run(folder, line),
        Err(e) => {
            eprintln!("hugin: {e}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

fn run(folder: PathBuf, line: bool) -> ExitCode {
    let start = match start::learn(&folder) {
        Ok(start) => start,
        Err(e) => {
            eprintln!("hugin: {e}");
            return ExitCode::from(1);
        }
    };
    let notices = start::warm(start.model.clone(), start.num_ctx);
    let (model, root) = (start.model.clone(), start.folder.clone());
    let header = Event::Header {
        folder: start.folder,
        branch: start.branch,
        changed: start.changed,
        model: start.model,
    };

    // The interface needs a terminal to read keys from and to draw on; a pipe gets the
    // plain line mode, so a script can drive Hugin, and so does `--line`.
    if !line && io::stdin().is_terminal() && io::stdout().is_terminal() {
        return match inline::start(move || Session::real(model, root), header, notices) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("hugin: {e}");
                ExitCode::from(1)
            }
        };
    }
    let mut session = Session::real(model, root);
    // What is read is shown by the terminal only when stdin is one.
    let echo = !io::stdin().is_terminal();
    line::run(BufReader::new(io::stdin()), io::stdout(), &mut session, header, notices, echo);
    ExitCode::SUCCESS
}
