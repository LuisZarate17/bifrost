//! Hugin: a terminal agent that works in a folder, running the turn loop Mimir runs.
//!
//! - [`start`] — what it learns on start: the folder, git, the model, the warm-up
//! - [`session`] — the conversation, turn by turn, and its saved record
//! - [`ui`] — the seam: events out, approvals in; [`line`] is the plain mode behind it
//! - [`frontend`], [`view`] — the loop's frontend, and cards put into words
//! - [`stop`] — Stop, for a thread that is not the one running the turn

mod frontend;
mod line;
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

usage: hugin [--folder <path>]

  --folder <path>   the folder to work in (default: the current directory)
  -p                reserved for a later step
  --help            show this help

Type a request at the prompt. End a line with \\ to go on to the next.
/stop stops the turn that is running; /exit leaves.
";

/// What the command line asked for.
#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    /// `-p`, which a later step builds.
    Print,
    Run { folder: PathBuf },
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Cmd, String> {
    let mut folder = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "-p" => return Ok(Cmd::Print),
            "--folder" => folder = Some(args.next().ok_or("--folder needs a path")?),
            other => match other.strip_prefix("--folder=") {
                Some(path) => folder = Some(path.to_string()),
                None => return Err(format!("unknown argument: {other}")),
            },
        }
    }
    Ok(Cmd::Run { folder: folder.map_or_else(|| PathBuf::from("."), PathBuf::from) })
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
        Ok(Cmd::Run { folder }) => run(folder),
        Err(e) => {
            eprintln!("hugin: {e}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

fn run(folder: PathBuf) -> ExitCode {
    let start = match start::learn(&folder) {
        Ok(start) => start,
        Err(e) => {
            eprintln!("hugin: {e}");
            return ExitCode::from(1);
        }
    };
    let notices = start::warm(start.model.clone(), start.num_ctx);
    let mut session = Session::real(start.model.clone(), start.folder.clone());
    let header = Event::Header {
        folder: start.folder,
        branch: start.branch,
        changed: start.changed,
        model: start.model,
    };

    // The inline interface arrives in a later step; until then a terminal gets the line
    // mode too. What is read is shown by the terminal only when stdin is one.
    let echo = !io::stdin().is_terminal();
    line::run(BufReader::new(io::stdin()), io::stdout(), &mut session, header, notices, echo);
    ExitCode::SUCCESS
}
