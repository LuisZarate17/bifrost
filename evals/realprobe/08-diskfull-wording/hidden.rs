#[cfg(test)]
mod realprobe_hidden {
    use super::*;

    const REASON: &str = "stopped because the disk was nearly full";

    fn outcome(ended: Ended) -> check::Outcome {
        check::Outcome {
            ended,
            elapsed: Duration::from_secs(30),
            output: "error: no space left on device".into(),
            bytes: 31,
            complete: true,
            blocked: None,
            unfinished_tests: 0,
        }
    }

    fn cmd() -> Vec<String> {
        vec!["cargo".to_string(), "test".to_string()]
    }

    fn report_with(check: CheckReport) -> Report {
        Report { answer: "Done.".into(), check: Some(check), ..Default::default() }
    }

    fn disk_full() -> Report {
        report_with(report_of(&cmd(), &outcome(Ended::DiskFull), false))
    }

    #[test]
    fn realprobe_hidden_a_disk_full_stop_is_a_stopped_check_that_ran_and_gave_no_verdict() {
        let c = report_of(&cmd(), &outcome(Ended::DiskFull), false);
        assert_eq!(c.stopped.as_deref(), Some(REASON), "{c:?}");
        assert!(c.not_run.is_none(), "{c:?}");
        assert!(c.passed.is_none() && !c.timed_out && c.exit_code.is_none() && c.blocked.is_none(), "{c:?}");
        assert!(c.output_tail.is_empty(), "{c:?}");
    }

    #[test]
    fn realprobe_hidden_it_is_still_partial_with_exit_3() {
        let r = disk_full();
        assert!(r.partial());
        assert_eq!(r.status(), Status::Partial);
        assert_eq!(r.exit_code(), 3);
        let j = r.to_json();
        assert_eq!(j["status"], "partial");
        assert_eq!(j["partial"], true);
    }

    #[test]
    fn realprobe_hidden_the_text_says_stopped_and_never_not_run() {
        let text = disk_full().to_text();
        assert!(text.contains("The check was stopped because the disk was nearly full"), "{text}");
        assert!(text.contains("check: `cargo test` was stopped because the disk was nearly full"), "{text}");
        assert!(!text.contains("not run"), "{text}");
    }

    #[test]
    fn realprobe_hidden_the_json_says_stopped_and_leaves_not_run_null() {
        let r = disk_full();
        let j = r.to_json();
        assert!(j["check"]["not_run"].is_null(), "{j}");
        assert!(j["check"]["passed"].is_null(), "{j}");
        assert_eq!(j["check"]["stopped"], REASON, "{j}");
        let text = r.to_json_string();
        assert!(text.contains(&format!("\"stopped\":\"{REASON}\"")), "{text}");
        assert!(text.contains("\"not_run\":null"), "{text}");
    }

    #[test]
    fn realprobe_hidden_a_check_that_did_not_run_a_timeout_and_a_failure_are_unchanged() {
        let argv = cmd();
        let skipped = report_with(not_run(&argv, "no time was left for it"));
        assert!(skipped.to_text().contains("check: `cargo test` was not run: no time was left for it"));
        assert!(skipped.to_text().contains("The check was not run: no time was left for it"));
        assert_eq!(skipped.to_json()["check"]["not_run"], "no time was left for it");
        assert!(skipped.to_json()["check"]["stopped"].is_null());
        assert_eq!((skipped.status(), skipped.exit_code()), (Status::Partial, 3));

        let slow = report_of(&argv, &outcome(Ended::TimedOut), false);
        assert!(slow.timed_out && slow.not_run.is_none() && slow.stopped.is_none(), "{slow:?}");
        assert!(report_with(slow).to_text().contains("timed out after"));

        let failed = report_of(&argv, &outcome(Ended::Exited(Some(101))), false);
        assert!(failed.passed == Some(false) && failed.stopped.is_none() && failed.not_run.is_none(), "{failed:?}");
        let r = report_with(failed);
        assert_eq!((r.status(), r.exit_code()), (Status::Failed, 3));
        assert!(r.to_json()["check"]["stopped"].is_null());
    }
}
