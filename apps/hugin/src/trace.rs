//! `--trace <file.jsonl>`: one JSON line for every request a `-p` or `--ask` run sends the model.
//!
//! Made for comparing models: what each request cost (tokens and Ollama's own timings), how
//! the reply was handled (read, recovered, or sent back unreadable) and what went wrong. The
//! lines are appended, so several runs can share a file.
//!
//! **Writing the trace never fails the run.** A file that cannot be opened or written is said
//! once on standard error and the run goes on without it.
//!
//! - [`Trace`] — the file; [`wired`] puts it round the model a run is wired with
//! - [`Line`] — what one request is written as

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bifrost_core::agent::round::Round;
use bifrost_core::agent::send::Calibration;
use bifrost_core::chat::Message;
use serde::Serialize;

use crate::print::Wired;

/// One request, as a line of the trace. The fields come in this order.
///
/// Durations are Ollama's own, in nanoseconds, and `null` where it did not say (or the request
/// failed). `thinking_len` is the length of what the model thought, in characters; 0 for a model
/// that does not think or was not asked to (`model_settings` in the config).
#[derive(Debug, Serialize, PartialEq)]
pub struct Line {
    /// When the reply came back (or the request failed): UTC, to the millisecond.
    pub time: String,
    pub model: String,
    pub prompt_eval_count: Option<u32>,
    pub eval_count: Option<u32>,
    pub total_duration: Option<u64>,
    pub prompt_eval_duration: Option<u64>,
    pub eval_duration: Option<u64>,
    /// The call was read after a retry through the raw endpoint.
    pub recovered: bool,
    /// Ollama could not read the call and the model was told so.
    pub fed_back: bool,
    /// Calls in the reply, all of them.
    pub tool_calls: usize,
    pub thinking_len: usize,
    /// Why the request failed, if it did; for a call fed back, what Ollama said of it.
    pub error: Option<String>,
    /// Something the round did not do as configured (a `think` setting left out because the
    /// model does not report `thinking`). **Left out of the line when there is none**, so a line
    /// that has nothing to say reads as it always did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Line {
    /// The line for what a request came to, at `time`.
    pub fn of(model: &str, time: SystemTime, result: &Result<Round, String>) -> Self {
        let ns = |d: Duration| (!d.is_zero()).then(|| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        let base = Line {
            time: iso(time),
            model: model.to_string(),
            prompt_eval_count: None,
            eval_count: None,
            total_duration: None,
            prompt_eval_duration: None,
            eval_duration: None,
            recovered: false,
            fed_back: false,
            tool_calls: 0,
            thinking_len: 0,
            error: None,
            note: None,
        };
        match result {
            Ok(round) => Line {
                prompt_eval_count: Some(round.prompt_eval_count),
                eval_count: Some(round.metrics.eval_count),
                total_duration: ns(round.metrics.total),
                prompt_eval_duration: ns(round.metrics.prompt_eval),
                eval_duration: ns(round.metrics.eval),
                recovered: round.recovered,
                fed_back: round.fed_back,
                tool_calls: round.metrics.tool_calls,
                thinking_len: round.metrics.thinking_len,
                error: round.metrics.error.clone(),
                note: round.metrics.note.clone(),
                ..base
            },
            Err(e) => Line { error: Some(e.clone()), ..base },
        }
    }
}

/// `time` as `2026-10-25T14:03:07.123Z`, by the civil-from-days rule, so no date crate is needed.
fn iso(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (secs, millis) = (since.as_secs(), since.subsec_millis());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm), March-based years.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// The trace file, shared by the requests of a run.
pub struct Trace {
    /// `None` once it could not be opened or written.
    file: Mutex<Option<File>>,
    warned: AtomicBool,
}

impl Trace {
    /// Open `path` for appending, creating it. **Never fails**: a file that cannot be opened is
    /// said once, and the trace is then a trace of nothing.
    pub fn open(path: &Path) -> Arc<Self> {
        let trace = Arc::new(Trace { file: Mutex::new(None), warned: AtomicBool::new(false) });
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => *trace.file.lock().unwrap_or_else(|e| e.into_inner()) = Some(file),
            Err(e) => trace.warn(&format!("could not open {}: {e}", path.display())),
        }
        trace
    }

    /// Write one line. A failure turns the trace off and is said once.
    pub fn write(&self, line: &Line) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let Some(open) = file.as_mut() else { return };
        let text = match serde_json::to_string(line) {
            Ok(text) => text,
            Err(e) => return self.warn(&format!("could not encode a line: {e}")),
        };
        if let Err(e) = writeln!(open, "{text}").and_then(|()| open.flush()) {
            *file = None;
            self.warn(&format!("could not write: {e}"));
        }
    }

    fn warn(&self, why: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            eprintln!("hugin: --trace: {why}; the run goes on without it");
        }
    }
}

/// `inner` with every request written to the trace, if there is one. The request is answered
/// exactly as it was: the trace only watches.
pub fn wired(trace: Option<Arc<Trace>>, model: &str, inner: Wired) -> Wired {
    let Some(trace) = trace else { return inner };
    let Wired { mut ask, run } = inner;
    let model = model.to_string();
    let ask = move |messages: &[Message], calibration: Option<Calibration>, closing: bool| {
        let result = ask(messages, calibration, closing);
        trace.write(&Line::of(&model, SystemTime::now(), &result));
        result
    };
    Wired { ask: Box::new(ask), run }
}
