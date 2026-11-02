#[cfg(test)]
mod realprobe_hidden {
    use super::*;
    use std::time::Duration;

    fn parsed(list: &[&str]) -> Result<Cmd, String> {
        parse(list.iter().map(|s| s.to_string()))
    }

    fn asked(list: &[&str]) -> ask::Args {
        match parsed(list) {
            Ok(Cmd::Ask(a)) => a,
            other => panic!("{list:?} gave {other:?}"),
        }
    }

    #[test]
    fn realprobe_hidden_a_flag_right_after_ask_is_not_taken_as_the_question() {
        for list in [
            &["--ask", "--json"][..],
            &["--ask", "--json", "where is the parser?"],
            &["--ask", "--folder", "C:/work"],
            &["--ask", "--time", "60", "q"],
            &["--ask", "--model", "gpt-oss:20b"],
            &["--ask", "--help"],
            &["--folder", "C:/work", "--ask", "--trace", "t.jsonl"],
        ] {
            let e = parsed(list).unwrap_err();
            assert!(e.contains("--ask needs a question"), "{list:?}: {e}");
        }
    }

    #[test]
    fn realprobe_hidden_ask_with_nothing_after_it_still_says_what_it_needs() {
        assert!(parsed(&["--ask"]).unwrap_err().contains("--ask needs a question"));
    }

    #[test]
    fn realprobe_hidden_a_dash_is_standard_input_and_one_dash_then_words_is_a_question() {
        assert_eq!(asked(&["--ask", "-"]).question, ask::Question::Stdin);
        assert_eq!(asked(&["--ask", "-5 degrees: why?"]).question, ask::Question::Text("-5 degrees: why?".into()));
        assert_eq!(asked(&["--ask", "-x"]).question, ask::Question::Text("-x".into()));
        assert_eq!(asked(&["--ask", "what does --json do?"]).question, ask::Question::Text("what does --json do?".into()));
        // An empty question is still dealt with where it is today, not by the parser.
        assert_eq!(asked(&["--ask", ""]).question, ask::Question::Text(String::new()));
    }

    #[test]
    fn realprobe_hidden_an_ordinary_ask_parses_as_before() {
        let a = asked(&["--ask", "where is the parser?"]);
        assert_eq!(a.question, ask::Question::Text("where is the parser?".into()));
        assert_eq!((a.json, a.folder, a.time), (false, PathBuf::from("."), Duration::from_secs(300)));
        let a = asked(&["--folder", "C:/work", "--ask", "how does it work?", "--json", "--time", "60"]);
        assert_eq!(a.question, ask::Question::Text("how does it work?".into()));
        assert!(a.json);
        assert_eq!((a.folder, a.time), (PathBuf::from("C:/work"), Duration::from_secs(60)));
        let a = asked(&["--ask", "q", "--model=gpt-oss:20b", "--trace=a.jsonl"]);
        assert_eq!((a.model.as_deref(), a.trace), (Some("gpt-oss:20b"), Some(PathBuf::from("a.jsonl"))));
    }

    #[test]
    fn realprobe_hidden_dash_p_is_left_as_it_was() {
        match parsed(&["-p", "--json"]) {
            Ok(Cmd::Print(a)) => assert_eq!(a.task, print::Task::Text("--json".into())),
            other => panic!("{other:?}"),
        }
        match parsed(&["-p", "-"]) {
            Ok(Cmd::Print(a)) => assert_eq!(a.task, print::Task::Stdin),
            other => panic!("{other:?}"),
        }
        assert!(parsed(&["-p"]).unwrap_err().contains("-p needs a task"));
        // And --ask still refuses what it refused.
        assert!(parsed(&["--ask", "q", "--check", "cargo test"]).unwrap_err().contains("--check goes with -p"));
        assert!(parsed(&["-p", "x", "--ask", "y"]).unwrap_err().contains("two ways to run Hugin"));
    }
}
