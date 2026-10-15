//! The Stop counter. One Stop reaches whatever has a number on it: a streamed
//! answer, a check, a search, or a walk that has not begun.

use std::sync::atomic::{AtomicU64, Ordering};

/// How many answers `send` has started, and how many of those have been
/// stopped — every answer numbered up to this one.
///
/// **Counters, not a flag.** A flag would have to be reset when the next answer
/// starts, and an old stream still waiting on its first token would then read
/// the reset and run on. With numbers, stopping marks only the answers that
/// already exist; the next one gets a higher number and cannot be caught by it.
static STREAMS_STARTED: AtomicU64 = AtomicU64::new(0);
// Crate-visible for one test in `run`, which marks a single number as stopped.
pub(crate) static STREAMS_STOPPED: AtomicU64 = AtomicU64::new(0);

pub fn stopped(id: u64) -> bool {
    STREAMS_STOPPED.load(Ordering::SeqCst) >= id
}

/// A number on the Stop counter for something that has not started yet — a
/// check or a search waiting on its card. Taken when the card is shown, so a
/// Stop pressed between Allow and the start still reaches it.
pub fn next_stop() -> u64 {
    STREAMS_STARTED.fetch_add(1, Ordering::SeqCst) + 1
}

/// Stop everything numbered so far: the answer streaming now, and any check or
/// search that has taken a number. Each notices the next time it looks — a stream
/// at its next token. A number taken afterwards is higher and is not caught by it.
pub fn stop_all() {
    STREAMS_STOPPED.fetch_max(STREAMS_STARTED.load(Ordering::SeqCst), Ordering::SeqCst);
}
