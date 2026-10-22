//! Hugin: a terminal agent that works in a folder, running the turn loop Mimir runs.
//!
//! - [`start`] — what it learns on start: the folder, git, the model, the warm-up
//! - [`session`] — the conversation, turn by turn, and its saved record
//! - [`ui`] — the seam: events out, approvals in; [`line`] is the plain mode behind it
//! - [`inline`], [`input`], [`render`], [`transcript`] — the full-screen interface for a terminal: its
//!   loop, its input box, how it looks, and the scrollback it keeps
//! - [`frontend`], [`view`] — the loop's frontend, and cards put into words
//! - [`stop`] — Stop, for a thread that is not the one running the turn
//! - [`print`] — `hugin -p`: one task handed over, a proposal handed back

mod frontend;
mod inline;
#[cfg(test)]
mod inline_tests;
mod input;
mod launch;
#[cfg(test)]
mod launch_tests;
mod line;
mod print;
#[cfg(test)]
mod print_tests;
mod render;
mod session;
mod start;
mod stop;
#[cfg(test)]
mod tests;
mod transcript;
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
       hugin -p <task> [--check <command>] [--json] [--folder <path>] [--time <seconds>]

  --folder <path>   the folder to work in (default: the current directory)
  --shell <name>    the shell the model's commands run in (default: hugin_shell in the config,
                    else Git Bash, or PowerShell where Git Bash is not installed); every
                    command is shown and asked about first
  --line            plain line mode, as when input or output is not a terminal
  --launch          open Hugin in Windows Terminal, in the folder it was last used in
  -p <task>         do one task and print what came of it, with nobody to ask: Hugin works in a
                    copy of the folder (the last commit plus your uncommitted files), so nothing
                    in your folder is written, and hands back its answer, a patch and the result
                    of the check. `-p -` reads the task from standard input. Git folders only.
                    The check runs code Hugin wrote before you have read it: name a check only
                    in a repository you trust
  --check <command> with -p: one program and its arguments to check the work with, for this run
                    only (no pipes, && or redirects); without it, the folder's check in Mimir's
                    settings, else none. Not run at the end if Hugin changed a file that makes a
                    check run code (Cargo.toml, package.json, a makefile, conftest.py...)
  --json            with -p: the result as one JSON object
  --time <seconds>  with -p: how long the whole run may take (default 540)
  --claude-note     print the section for Claude Code's CLAUDE.md that says when to use -p
  --help            show this help

Type a request at the prompt. Enter sends; Shift+Enter, Alt+Enter, Ctrl+J or a \\ at
the end of the line starts a new line. Esc stops the turn that is running; Ctrl+D or /exit
leaves. In line mode, /stop stops the turn.
";

/// What the command line asked for.
#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    /// `-p`: one task, done unattended in a copy of the folder.
    Print(print::Args),
    /// `--claude-note`: print the CLAUDE.md section, and write nothing.
    ClaudeNote,
    /// `--launch`: open Hugin in a terminal window, as the shortcut key does.
    Launch,
    Run { folder: PathBuf, line: bool, shell: Option<Shell> },
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Cmd, String> {
    let mut folder = None;
    let mut line = false;
    let mut shell = None;
    let (mut task, mut check, mut time) = (None, None, None);
    let (mut json, mut note) = (false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "-p" => task = Some(args.next().ok_or("-p needs a task, or - to read it from standard input")?),
            "--launch" => return Ok(Cmd::Launch),
            "--line" => line = true,
            "--json" => json = true,
            "--claude-note" => note = true,
            "--folder" => folder = Some(args.next().ok_or("--folder needs a path")?),
            "--shell" => shell = Some(parse_shell(&args.next().ok_or("--shell needs a name")?)?),
            "--check" => check = Some(args.next().ok_or("--check needs a command")?),
            "--time" => time = Some(args.next().ok_or("--time needs a number of seconds")?),
            other => match (other.strip_prefix("--folder="), other.strip_prefix("--shell=")) {
                (Some(path), _) => folder = Some(path.to_string()),
                (_, Some(name)) => shell = Some(parse_shell(name)?),
                _ => match (other.strip_prefix("--check="), other.strip_prefix("--time=")) {
                    (Some(command), _) => check = Some(command.to_string()),
                    (_, Some(secs)) => time = Some(secs.to_string()),
                    _ => return Err(format!("unknown argument: {other}")),
                },
            },
        }
    }
    let folder = folder.map_or_else(|| PathBuf::from("."), PathBuf::from);
    if note {
        return Ok(Cmd::ClaudeNote);
    }
    let Some(task) = task else {
        if check.is_some() || json || time.is_some() {
            return Err("--check, --json and --time go with -p".to_string());
        }
        return Ok(Cmd::Run { folder, line, shell });
    };
    if line || shell.is_some() {
        return Err("--line and --shell are for the interactive Hugin, not -p".to_string());
    }
    let task = if task == "-" { print::Task::Stdin } else { print::Task::Text(task) };
    let check = check.map(|command| print::parse_check(&command)).transpose()?;
    let time = print::parse_time(time.as_deref())?;
    Ok(Cmd::Print(print::Args { task, check, json, folder, time }))
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
        Ok(Cmd::Print(args)) => print::run(args),
        Ok(Cmd::ClaudeNote) => {
            print!("{}", print::CLAUDE_NOTE);
            ExitCode::SUCCESS
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
    use std::time::Duration;

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

    fn printed(list: &[&str]) -> print::Args {
        match args(list) {
            Ok(Cmd::Print(a)) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dash_p_takes_a_task_and_the_flags_that_go_with_it() {
        let a = printed(&["-p", "fix the bug"]);
        assert_eq!(a.task, print::Task::Text("fix the bug".into()));
        assert_eq!((a.check, a.json, a.folder, a.time), (None, false, PathBuf::from("."), Duration::from_secs(540)));

        let a = printed(&["--folder", "C:\\work", "-p", "x", "--json", "--check", "cargo test -p hugin", "--time", "60"]);
        assert_eq!(a.check, Some(vec!["cargo".into(), "test".into(), "-p".into(), "hugin".into()]));
        assert!(a.json);
        assert_eq!((a.folder, a.time), (PathBuf::from("C:\\work"), Duration::from_secs(60)));
        // The `=` forms, and the task from standard input.
        let a = printed(&["-p", "-", "--check=npm test", "--time=5", "--folder=sub"]);
        assert_eq!(a.task, print::Task::Stdin);
        assert_eq!(a.check, Some(vec!["npm.cmd".into(), "test".into()]));
        assert_eq!((a.time, a.folder), (Duration::from_secs(5), PathBuf::from("sub")));
    }

    #[test]
    fn dash_p_refuses_what_it_cannot_do_and_says_so() {
        assert!(args(&["-p"]).unwrap_err().contains("needs a task"));
        for bad in ["cargo test | tee out.txt", "cargo test && cargo clippy", "cargo test > out.txt", "cargo test; ls"] {
            let e = args(&["-p", "x", "--check", bad]).unwrap_err();
            assert!(e.contains("not a shell command line"), "{bad}: {e}");
        }
        assert!(args(&["-p", "x", "--check"]).is_err());
        assert!(args(&["-p", "x", "--time", "0"]).is_err());
        assert!(args(&["-p", "x", "--time", "soon"]).is_err());
        assert!(args(&["-p", "x", "--line"]).is_err());
        assert!(args(&["-p", "x", "--shell", "cmd"]).is_err());
        // The flags that only mean something with -p.
        for alone in [&["--check", "cargo test"][..], &["--json"], &["--time", "9"]] {
            assert!(args(alone).unwrap_err().contains("go with -p"), "{alone:?}");
        }
    }

    #[test]
    fn the_claude_note_is_printed_whatever_else_is_given_and_nothing_is_written() {
        assert_eq!(args(&["--claude-note"]), Ok(Cmd::ClaudeNote));
        assert_eq!(args(&["--folder", "x", "--claude-note"]), Ok(Cmd::ClaudeNote));
        assert!(print::CLAUDE_NOTE.starts_with("## Hugin") && print::CLAUDE_NOTE.ends_with('\n'));
        assert!(print::CLAUDE_NOTE.contains("hugin -p") && print::CLAUDE_NOTE.contains("600000"));
    }
}
