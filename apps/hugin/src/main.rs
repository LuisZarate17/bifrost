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
//! - [`ask`] — `hugin --ask`: one question about the code, answered by reading it, read-only
//! - [`bent`] — the existing tests a `-p` patch changes, which it warns about
//! - [`trace`] — `--trace`: a line for every request a `-p` or `--ask` run sends the model

mod ask;
#[cfg(test)]
mod ask_tests;
mod bent;
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
mod trace;
#[cfg(test)]
mod trace_tests;
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
                       [--model <tag>] [--trace <file.jsonl>]
       hugin --ask <question> [--json] [--folder <path>] [--time <seconds>]
                       [--model <tag>] [--trace <file.jsonl>]

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
  --ask <question>  answer one question about the code, with nobody to ask: Hugin reads the folder
                    in place, on this machine, and writes nothing (it can only read, list and
                    search), then prints its answer and the files it looked at. `--ask -` reads
                    the question from standard input. Any folder; git is not needed. It can be
                    wrong: open the files it names
  --json            with -p or --ask: the result as one JSON object
  --time <seconds>  with -p or --ask: how long the whole run may take (default 540; 300 for --ask)
  --model <tag>     with -p or --ask: the Ollama model to use for this run only, instead of the
                    coding model in the config (which is never changed)
  --trace <file>    with -p or --ask: append one JSON line to this file for every request sent to
                    the model: time, model, prompt_eval_count, eval_count, total_duration,
                    prompt_eval_duration and eval_duration (Ollama's, in nanoseconds, null where
                    it did not say), recovered, fed_back (Ollama could not read the call and the
                    model was told), tool_calls (in the reply), thinking_len and error, and note
                    when a setting was left out. A trace that cannot be written is said once and
                    never stops the run
  --claude-note     print the section for Claude Code's CLAUDE.md that says when to use -p
  --help            show this help

With -p the exit code says how it came out: 0 done; 3 not done (the check failed, the patch
changes existing tests, the run was cut short, or the check gave no verdict: don't apply the
patch as it is); 1 it could not run; 2 the command line could not be read. A model that says it
is done after its check failed is not believed: the report says FAILED, and gives a draft patch
to read, not a command to apply. A passing check can still be wrong: a patch that changes
existing tests, which may have been bent to fit the code, is REVIEW, not done, until someone
confirms the change was asked for. A cargo test run in which a test program stops early (a
process::exit, an abort) counts as failed, whatever its exit code.
With --ask: 0 answered; 3 partial (stopped at the time limit); 1 it could not run; 2 the command
line could not be read.

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
    /// `--ask`: one question about the code, answered by reading it in place.
    Ask(ask::Args),
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
    let (mut task, mut check, mut time, mut question) = (None, None, None, None);
    let (mut model, mut trace) = (None, None);
    let (mut json, mut note) = (false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Cmd::Help),
            "-p" => task = Some(args.next().ok_or("-p needs a task, or - to read it from standard input")?),
            "--ask" => question = Some(args.next().ok_or("--ask needs a question, or - to read it from standard input")?),
            "--launch" => return Ok(Cmd::Launch),
            "--line" => line = true,
            "--json" => json = true,
            "--claude-note" => note = true,
            "--folder" => folder = Some(args.next().ok_or("--folder needs a path")?),
            "--shell" => shell = Some(parse_shell(&args.next().ok_or("--shell needs a name")?)?),
            "--check" => check = Some(args.next().ok_or("--check needs a command")?),
            "--time" => time = Some(args.next().ok_or("--time needs a number of seconds")?),
            "--model" => model = Some(args.next().ok_or("--model needs a model tag")?),
            "--trace" => trace = Some(args.next().ok_or("--trace needs a file")?),
            other => match (other.strip_prefix("--folder="), other.strip_prefix("--shell=")) {
                (Some(path), _) => folder = Some(path.to_string()),
                (_, Some(name)) => shell = Some(parse_shell(name)?),
                _ => match (other.strip_prefix("--check="), other.strip_prefix("--time=")) {
                    (Some(command), _) => check = Some(command.to_string()),
                    (_, Some(secs)) => time = Some(secs.to_string()),
                    _ => match (other.strip_prefix("--model="), other.strip_prefix("--trace=")) {
                        (Some(tag), _) => model = Some(tag.to_string()),
                        (_, Some(file)) => trace = Some(file.to_string()),
                        _ => return Err(format!("unknown argument: {other}")),
                    },
                },
            },
        }
    }
    let folder = folder.map_or_else(|| PathBuf::from("."), PathBuf::from);
    let model = model.map(|tag| parse_model(&tag)).transpose()?;
    let trace = trace.map(|file| parse_trace(&file)).transpose()?;
    if note {
        return Ok(Cmd::ClaudeNote);
    }
    if let Some(question) = question {
        if task.is_some() {
            return Err("-p and --ask are two ways to run Hugin; give one".to_string());
        }
        if check.is_some() {
            return Err("--check goes with -p, not --ask: a question has nothing to check".to_string());
        }
        if line || shell.is_some() {
            return Err("--line and --shell are for the interactive Hugin, not --ask".to_string());
        }
        let question = if question == "-" { ask::Question::Stdin } else { ask::Question::Text(question) };
        let time = ask::parse_time(time.as_deref())?;
        return Ok(Cmd::Ask(ask::Args { question, json, folder, time, model, trace }));
    }
    let Some(task) = task else {
        if check.is_some() || json || time.is_some() || model.is_some() || trace.is_some() {
            return Err("--check goes with -p; --json, --time, --model and --trace go with -p or --ask".to_string());
        }
        return Ok(Cmd::Run { folder, line, shell });
    };
    if line || shell.is_some() {
        return Err("--line and --shell are for the interactive Hugin, not -p".to_string());
    }
    let task = if task == "-" { print::Task::Stdin } else { print::Task::Text(task) };
    let check = check.map(|command| print::parse_check(&command)).transpose()?;
    let time = print::parse_time(time.as_deref())?;
    Ok(Cmd::Print(print::Args { task, check, json, folder, time, model, trace }))
}

/// `--model`'s tag: anything Ollama could name, but not nothing.
fn parse_model(tag: &str) -> Result<String, String> {
    let tag = tag.trim();
    if tag.is_empty() { Err("--model needs a model tag".to_string()) } else { Ok(tag.to_string()) }
}

/// `--trace`'s file.
fn parse_trace(file: &str) -> Result<PathBuf, String> {
    if file.trim().is_empty() { Err("--trace needs a file".to_string()) } else { Ok(PathBuf::from(file)) }
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
        Ok(Cmd::Ask(args)) => ask::run(args),
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

    fn asked(list: &[&str]) -> ask::Args {
        match args(list) {
            Ok(Cmd::Ask(a)) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ask_takes_a_question_and_the_flags_that_go_with_it() {
        let a = asked(&["--ask", "where is the parser?"]);
        assert_eq!(a.question, ask::Question::Text("where is the parser?".into()));
        assert_eq!((a.json, a.folder, a.time), (false, PathBuf::from("."), Duration::from_secs(300)));

        let a = asked(&["--folder", "C:\\work", "--ask", "how does it work?", "--json", "--time", "60"]);
        assert_eq!(a.question, ask::Question::Text("how does it work?".into()));
        assert!(a.json);
        assert_eq!((a.folder, a.time), (PathBuf::from("C:\\work"), Duration::from_secs(60)));
        // The `=` forms, and the question from standard input.
        let a = asked(&["--ask", "-", "--time=5", "--folder=sub"]);
        assert_eq!(a.question, ask::Question::Stdin);
        assert_eq!((a.time, a.folder), (Duration::from_secs(5), PathBuf::from("sub")));
    }

    #[test]
    fn ask_refuses_what_it_cannot_do_and_says_so() {
        assert!(args(&["--ask"]).unwrap_err().contains("needs a question"));
        assert!(args(&["-p", "x", "--ask", "y"]).unwrap_err().contains("two ways to run Hugin"));
        assert!(args(&["--ask", "y", "-p", "x"]).unwrap_err().contains("two ways to run Hugin"));
        assert!(args(&["--ask", "q", "--check", "cargo test"]).unwrap_err().contains("--check goes with -p"));
        assert!(args(&["--ask", "q", "--line"]).is_err());
        assert!(args(&["--ask", "q", "--shell", "cmd"]).is_err());
        assert!(args(&["--ask", "q", "--time", "0"]).is_err());
        assert!(args(&["--ask", "q", "--time", "soon"]).is_err());
        // Without a question or a task, the flags have nothing to go with.
        for alone in [&["--json"][..], &["--time", "9"]] {
            assert!(args(alone).unwrap_err().contains("go with -p or --ask"), "{alone:?}");
        }
        // And -p is still -p: five minutes is --ask's own.
        assert_eq!(printed(&["-p", "x"]).time, Duration::from_secs(540));
    }

    #[test]
    fn model_and_trace_go_with_p_and_with_ask_and_with_nothing_else() {
        // Neither is given: the config's model, and no trace.
        let a = printed(&["-p", "x"]);
        assert_eq!((a.model, a.trace), (None, None));
        let a = asked(&["--ask", "q"]);
        assert_eq!((a.model, a.trace), (None, None));

        let a = printed(&["-p", "x", "--model", "devstral:24b", "--trace", "C:\\runs\\a.jsonl"]);
        assert_eq!((a.model.as_deref(), a.trace), (Some("devstral:24b"), Some(PathBuf::from("C:\\runs\\a.jsonl"))));
        let a = asked(&["--ask", "q", "--model=gpt-oss:20b", "--trace=a.jsonl"]);
        assert_eq!((a.model.as_deref(), a.trace), (Some("gpt-oss:20b"), Some(PathBuf::from("a.jsonl"))));

        assert!(args(&["-p", "x", "--model"]).unwrap_err().contains("--model needs a model tag"));
        assert!(args(&["--ask", "q", "--trace"]).unwrap_err().contains("--trace needs a file"));
        assert!(args(&["--ask", "q", "--model", " "]).is_err());
        assert!(args(&["-p", "x", "--trace="]).is_err());
        // Without a task or a question there is nothing for them to go with.
        for alone in [&["--model", "x"][..], &["--trace", "t.jsonl"]] {
            assert!(args(alone).unwrap_err().contains("go with -p or --ask"), "{alone:?}");
        }
    }

    #[test]
    fn the_help_names_model_and_trace() {
        let text: String = HELP.split_whitespace().collect::<Vec<_>>().join(" ");
        for must in [
            "[--model <tag>] [--trace <file.jsonl>]",
            "--model <tag> with -p or --ask: the Ollama model to use for this run only, instead of the coding model in the config (which is never changed)",
            "--trace <file> with -p or --ask: append one JSON line to this file for every request sent to the model",
            "fed_back (Ollama could not read the call and the model was told)",
            "A trace that cannot be written is said once and never stops the run",
        ] {
            assert!(text.contains(must), "{must}");
        }
    }

    #[test]
    fn the_help_names_what_each_exit_code_of_dash_p_means() {
        let text: String = HELP.split_whitespace().collect::<Vec<_>>().join(" ");
        for must in [
            "0 done; 3 not done (the check failed, the patch changes existing tests,",
            "don't apply the patch as it is); 1 it could not run; 2 the command line could not be read",
            "the report says FAILED",
            "A passing check can still be wrong: a patch that changes existing tests, which may have been bent to fit the code, is REVIEW, not done, until someone confirms the change was asked for",
            "A cargo test run in which a test program stops early",
            "hugin --ask <question> [--json] [--folder <path>] [--time <seconds>]",
            "--ask <question> answer one question about the code, with nobody to ask: Hugin reads the folder in place, on this machine, and writes nothing",
            "With --ask: 0 answered; 3 partial (stopped at the time limit); 1 it could not run; 2 the command line could not be read.",
        ] {
            assert!(text.contains(must), "{must}");
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
