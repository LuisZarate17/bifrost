//! `--trace`: how a request is written as a line, and that a file that cannot be written to is
//! never the run's problem. The run over a scripted model is in `ask_tests`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bifrost_core::agent::round::{Metrics, Reply, Round};
use bifrost_core::agent::send::compact_for_send;

use crate::trace::*;

fn round(prompt_eval_count: u32, recovered: bool, fed_back: bool, metrics: Metrics) -> Round {
    Round {
        reply: Reply::Answer { content: "ok".into(), eval_count: metrics.eval_count },
        prompt_eval_count,
        recovered,
        fed_back,
        metrics,
        sent: compact_for_send(&mut [], &[], 8192, None, 0),
    }
}

fn at(secs: u64, millis: u32) -> SystemTime {
    UNIX_EPOCH + Duration::new(secs, millis * 1_000_000)
}

#[test]
fn the_time_is_utc_to_the_millisecond() {
    for (time, want) in [
        (at(0, 0), "1970-01-01T00:00:00.000Z"),
        (at(951_782_400, 5), "2000-02-29T00:00:00.005Z"),
        (at(1_700_000_000, 123), "2023-11-14T22:13:20.123Z"),
        (at(1_782_000_000 + 86_399, 999), "2026-06-21T23:59:59.999Z"),
    ] {
        assert_eq!(Line::of("m", time, &Err(String::new())).time, want);
    }
}

#[test]
fn a_reply_is_a_line_with_ollamas_numbers_and_how_it_was_handled() {
    let metrics = Metrics {
        eval_count: 42,
        total: Duration::from_millis(1500),
        prompt_eval: Duration::from_millis(300),
        eval: Duration::from_millis(1100),
        tool_calls: 2,
        thinking_len: 0,
        error: None,
    };
    let line = Line::of("devstral:24b", at(0, 0), &Ok(round(900, true, false, metrics)));
    assert_eq!(
        serde_json::to_string(&line).unwrap(),
        r#"{"time":"1970-01-01T00:00:00.000Z","model":"devstral:24b","prompt_eval_count":900,"eval_count":42,"total_duration":1500000000,"prompt_eval_duration":300000000,"eval_duration":1100000000,"recovered":true,"fed_back":false,"tool_calls":2,"thinking_len":0,"error":null}"#
    );
}

#[test]
fn a_call_that_was_fed_back_says_so_and_a_missing_duration_is_null() {
    let metrics = Metrics { tool_calls: 1, error: Some("error parsing tool call: raw='{'".into()), ..Metrics::default() };
    let line = Line::of("m", at(0, 0), &Ok(round(0, false, true, metrics)));
    assert!(line.fed_back && !line.recovered);
    assert_eq!((line.total_duration, line.prompt_eval_duration, line.eval_duration), (None, None, None));
    assert_eq!((line.tool_calls, line.error.as_deref()), (1, Some("error parsing tool call: raw='{'")));
}

#[test]
fn a_failed_request_is_a_line_with_the_error_text() {
    let line = Line::of("m", at(0, 0), &Err("/api/chat returned 500: EOF".to_string()));
    assert_eq!(line.error.as_deref(), Some("/api/chat returned 500: EOF"));
    assert_eq!((line.prompt_eval_count, line.eval_count, line.tool_calls), (None, None, 0));
    assert!(!line.recovered && !line.fed_back);
}

#[test]
fn lines_are_appended_one_to_a_line() {
    let dir = std::env::temp_dir().join(format!("hugin-trace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("t.jsonl");
    for _ in 0..2 {
        // Opened again each time, as two runs sharing a file would.
        let trace = Trace::open(&file);
        trace.write(&Line::of("m", at(0, 0), &Err("x".to_string())));
    }
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.lines().all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A trace that cannot be opened is a trace of nothing, not a panic and not an error.
#[test]
fn a_file_that_cannot_be_opened_or_written_does_not_fail_the_run() {
    // A folder cannot be opened as a file.
    let trace = Trace::open(&std::env::temp_dir());
    trace.write(&Line::of("m", at(0, 0), &Err("x".to_string())));
    trace.write(&Line::of("m", at(0, 0), &Err("y".to_string())));
}
