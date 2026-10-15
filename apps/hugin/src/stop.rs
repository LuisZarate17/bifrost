//! Stop, for a terminal: one handle the session, the loop's frontend and the thread
//! reading the keyboard all hold.
//!
//! The turn runs on the main thread and can sit in one request for a minute, so
//! something else has to be able to end it: the thread that reads stdin sets the flag
//! the moment `/stop` arrives, and the loop, which asks [`Stopper::stopped`] at every
//! point it could carry on, ends the turn.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bifrost_core::agent;

#[derive(Default)]
struct Flags {
    /// Stop was pressed during the current turn.
    stopped: AtomicBool,
    /// A turn is running, so a `/stop` line means "stop it" rather than nothing.
    running: AtomicBool,
}

/// Cheap to clone; every clone is the same handle.
#[derive(Clone, Default)]
pub struct Stopper(Arc<Flags>);

impl Stopper {
    /// A turn begins: a Stop from an earlier one does not carry over.
    pub fn begin(&self) {
        self.0.stopped.store(false, Ordering::SeqCst);
        self.0.running.store(true, Ordering::SeqCst);
    }

    /// The turn is over; a `/stop` now has nothing to stop.
    pub fn end(&self) {
        self.0.running.store(false, Ordering::SeqCst);
    }

    pub fn running(&self) -> bool {
        self.0.running.load(Ordering::SeqCst)
    }

    pub fn stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }

    /// Stop the turn: the flag, and the Stop counter, so a check or a folder walk under
    /// way ends too. An approval that is waiting sees the flag and answers Deny.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        agent::stop::stop_all();
    }
}
