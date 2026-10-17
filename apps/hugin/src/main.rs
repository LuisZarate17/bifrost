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
mod launch;
#[cfg(test)]
mod launch_tests;
mod line;
mod raven;
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

use bifrost_core::check::Shell;
use bifrost_core::config;
use session::Session;
use ui::Event;

const HELP: &str = "\
hugin - a terminal agent that works in a folder

usage: hugin [--folder <path>] [--shell <powershell|cmd|bash>] [--line] [--launch]

  --folder <path>   the folder to work in (default: the current directory)
  --shell <name>    the shell the model's commands run in (default: hugin_shell in the config,
                    else Git Bash, or PowerShell where Git Bash is not installed); every
                    command is shown and asked about first
  --line            plain line mode, as when input or output is not a terminal
  --launch          open Hugin in Windows Terminal, in the folder it was last used in
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
    /// `--launch`: open Hugin in a terminal window, as the shortcut key does.
    Launch,
    Run { folder: PathBuf, line: bool, shell: Option<Shell> },
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Cmd, String> {
    let mut folder = None;
    let mut line = false;
    let mut shell = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "-p" => return Ok(Cmd::Print),
            "--launch" => return Ok(Cmd::Launch),
            "--line" => line = true,
            "--folder" => folder = Some(args.next().ok_or("--folder needs a path")?),
            "--shell" => shell = Some(parse_shell(&args.next().ok_or("--shell needs a name")?)?),
            other => match (other.strip_prefix("--folder="), other.strip_prefix("--shell=")) {
                (Some(path), _) => folder = Some(path.to_string()),
                (_, Some(name)) => shell = Some(parse_shell(name)?),
                _ => return Err(format!("unknown argument: {other}")),
            },
        }
    }
    Ok(Cmd::Run { folder: folder.map_or_else(|| PathBuf::from("."), PathBuf::from), line, shell })
}

fn parse_shell(name: &str) -> Result<Shell, String> {
    Shell::parse(name).ok_or_else(|| format!("unknown shell: {name} (powershell, cmd or bash)"))
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
        Ok(Cmd::Launch) => match launch::target().and_then(|(program, folder)| launch::start(&program, &folder)) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("hugin: {e}");
                ExitCode::from(1)
            }
        },
        Ok(Cmd::Run { folder, line, shell }) => run(folder, line, shell),
        Err(e) => {
            eprintln!("hugin: {e}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

/// Whether the interactive interface runs: not with `--line`, and only with a terminal to
/// read keys from and draw on.
fn interactive(line: bool, stdin_tty: bool, stdout_tty: bool) -> bool {
    !line && stdin_tty && stdout_tty
}

fn run(folder: PathBuf, line: bool, shell: Option<Shell>) -> ExitCode {
    // The flag, else the config's (an unknown value there is already the default).
    let asked = shell;
    let shell = asked.or_else(|| Shell::parse(&config::load().hugin_shell)).unwrap_or(Shell::Bash);
    let shell = if shell.available() {
        shell
    } else if asked.is_some() {
        // Named on the command line: say so rather than run commands somewhere else.
        eprintln!("hugin: {} is not installed, so commands cannot run in it (try --shell powershell)", shell.label());
        return ExitCode::from(1);
    } else {
        // The configured one is missing, Git Bash on a machine without Git most likely:
        // PowerShell is always there.
        eprintln!("hugin: {} is not installed; commands run in Windows PowerShell", shell.label());
        Shell::PowerShell
    };
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
        shell: shell.name().to_string(),
    };

    // The interface needs a terminal to read keys from and to draw on; a pipe gets the
    // plain line mode, so a script can drive Hugin, and so does `--line`.
    if interactive(line, io::stdin().is_terminal(), io::stdout().is_terminal()) {
        // Only here: line mode is what scripts use, and must not move a person's last folder.
        // Failing to record it is not worth a word.
        let last = root.clone();
        let _ = config::update(move |c| c.hugin_last_folder = Some(last));
        return match inline::start(move || Session::real(model, root, shell), header, notices) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("hugin: {e}");
                ExitCode::from(1)
            }
        };
    }
    let mut session = Session::real(model, root, shell);
    // What is read is shown by the terminal only when stdin is one.
    let echo = !io::stdin().is_terminal();
    line::run(BufReader::new(io::stdin()), io::stdout(), &mut session, header, notices, echo);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests_main {
    use super::*;

    fn args(list: &[&str]) -> Result<Cmd, String> {
        parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn line_mode_and_pipes_are_not_interactive_so_they_record_no_folder() {
        assert!(interactive(false, true, true));
        assert!(!interactive(true, true, true));
        assert!(!interactive(false, false, true));
        assert!(!interactive(false, true, false));
        assert_eq!(args(&["--launch"]), Ok(Cmd::Launch));
    }

    #[test]
    fn the_shell_flag_takes_a_name_and_nothing_else() {
        let shell = |list: &[&str]| match args(list) {
            Ok(Cmd::Run { shell, .. }) => shell,
            other => panic!("{other:?}"),
        };
        assert_eq!(shell(&[]), None);
        assert_eq!(shell(&["--shell", "cmd"]), Some(Shell::Cmd));
        assert_eq!(shell(&["--shell=bash", "--line"]), Some(Shell::Bash));
        assert!(args(&["--shell", "zsh"]).is_err());
        assert!(args(&["--shell"]).is_err());
    }
}
