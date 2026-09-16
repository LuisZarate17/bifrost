//! The tray: three states you set, and can see.
//!
//! C4a built and proved the mechanism from the CLI. This is the half you can
//! reach without a terminal — and it is also the only place **Warming** can
//! exist. `/api/ps` has no "loading" entry, so a model appears only once it is
//! resident; a one-shot command can never observe the gap. A long-lived process
//! can, because it knows it issued a load that has not come back yet. That is
//! in-process knowledge, not observable state.
//!
//! **The tooltip shows the card's figure, never Ollama's.** C4a measured them
//! disagreeing by ~2,500 MiB on the same model — `ollama ps` reports what Ollama
//! believes it is holding, and PLAN.md §6b is explicit that this is not proof.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Runtime};

use warden_core::{ollama, vram};

/// How often the poller asks the card and Ollama what is going on.
///
/// Five seconds, not one. The tooltip is read on hover, and the icon only has to
/// notice a state change a person did not initiate — Ollama's own 5-minute
/// timeout expiring, or the CLI being used in another window. Polling harder
/// would burn a thread all day to refresh something nobody is looking at.
const POLL: Duration = Duration::from_secs(5);

/// A load the *user* started is worth showing as Warming even before Ollama
/// admits the model exists. Anything longer than this and something is wrong.
const WARMING_CEILING: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Off,
    Warming,
    Ready,
}

impl State {
    fn code(self) -> u8 {
        match self {
            State::Off => 0,
            State::Warming => 1,
            State::Ready => 2,
        }
    }
    fn from_code(c: u8) -> Self {
        match c {
            1 => State::Warming,
            2 => State::Ready,
            _ => State::Off,
        }
    }
    fn label(self) -> &'static str {
        match self {
            State::Off => "Off",
            State::Warming => "Warming…",
            State::Ready => "Ready",
        }
    }
}

/// Shared power state. `AtomicU8` because the poller thread, the menu handlers
/// and the hotkey path all read it, and none of them should be able to block
/// the others on a lock just to ask what the state is.
pub struct Power {
    state: AtomicU8,
    model: String,
    num_ctx: u32,
    /// Set while a load we issued is in flight — this is what makes Warming
    /// observable at all.
    warming_since: Mutex<Option<std::time::Instant>>,
}

impl Power {
    pub fn new(model: String, num_ctx: u32) -> Self {
        Self {
            state: AtomicU8::new(State::Off.code()),
            model,
            num_ctx,
            warming_since: Mutex::new(None),
        }
    }

    pub fn state(&self) -> State {
        State::from_code(self.state.load(Ordering::Relaxed))
    }

    fn set(&self, s: State) {
        self.state.store(s.code(), Ordering::Relaxed);
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn begin_warming(&self) {
        *self.warming_since.lock().unwrap() = Some(std::time::Instant::now());
        self.set(State::Warming);
    }

    fn end_warming(&self) {
        *self.warming_since.lock().unwrap() = None;
    }

    fn warming_for(&self) -> Option<Duration> {
        self.warming_since.lock().unwrap().map(|t| t.elapsed())
    }
}

/// Load the model, off the UI thread.
///
/// Warming is set *before* the request goes out and cleared when it returns, so
/// the tray says what is happening for the whole 2–4 s rather than only once it
/// has already finished.
pub fn warm<R: Runtime>(app: &AppHandle<R>, power: Arc<Power>) {
    if power.state() == State::Warming {
        return; // already in flight — a second click should not queue another
    }
    power.begin_warming();
    let app = app.clone();
    std::thread::spawn(move || {
        // Same `num_ctx` the capture path sends. Ollama keys a loaded runner by
        // the model *and* the options that change its memory layout, so warming
        // at a different context silently reloads on the next capture and buys
        // nothing at all. C4a proved the match by watching `load_duration`.
        let result = ollama::load(power.model(), power.num_ctx);
        power.end_warming();
        match result {
            Ok(elapsed) => {
                power.set(State::Ready);
                println!("[warden] warmed in {:.1}s", elapsed.as_secs_f64());
            }
            Err(e) => {
                power.set(State::Off);
                eprintln!("[warden] warm failed: {e}");
            }
        }
        refresh(&app, &power);
    });
}

pub fn off<R: Runtime>(app: &AppHandle<R>, power: Arc<Power>) {
    let app = app.clone();
    std::thread::spawn(move || {
        match ollama::unload(power.model()) {
            Ok(true) => println!("[warden] released"),
            Ok(false) => eprintln!("[warden] Ollama still lists the model after 30s"),
            Err(e) => eprintln!("[warden] off failed: {e}"),
        }
        power.set(State::Off);
        refresh(&app, &power);
    });
}

/// Ask both sources what is true, and reconcile.
///
/// Ollama is authoritative for *whether* the model is loaded. The card is
/// authoritative for *how much memory is actually gone*. Neither substitutes for
/// the other.
fn observe(power: &Power) -> (State, String) {
    // A load we started outranks what Ollama can see, because it cannot see one
    // in progress at all.
    if let Some(elapsed) = power.warming_for() {
        if elapsed < WARMING_CEILING {
            return (State::Warming, format!("Warden — warming… ({:.0}s)", elapsed.as_secs_f64()));
        }
    }

    let loaded = match ollama::ps() {
        Ok(models) => ollama::find(&models, power.model()).is_some(),
        // Ollama being unreachable is not the same as the model being off, and
        // the tooltip should say which it is.
        Err(_) => {
            return (State::Off, "Warden — Ollama not running".to_string());
        }
    };

    let state = if loaded { State::Ready } else { State::Off };

    let card = vram::Card::open()
        .ok()
        .and_then(|c| c.sample().ok())
        .map(|s| format!("{} MiB of {} on the card", s.used_mib, s.total_mib))
        .unwrap_or_else(|| "card readout unavailable".to_string());

    (state, format!("Warden — {}\n{}", state.label(), card))
}

/// Re-read the world and push it to the tray.
pub fn refresh<R: Runtime>(app: &AppHandle<R>, power: &Power) {
    let (state, tip) = observe(power);
    // Transitions only. The poller runs all day; logging every tick would bury
    // everything else, and a change is the only part worth a line.
    let previous = power.state();
    if previous != state {
        println!("[warden] power: {} -> {}", previous.label(), state.label());
    }
    power.set(state);
    if let Some(tray) = app.tray_by_id("warden") {
        let _ = tray.set_tooltip(Some(&tip));
    }
}

/// Build the tray and start the poller.
pub fn install<R: Runtime>(app: &AppHandle<R>, power: Arc<Power>) -> tauri::Result<()> {
    let warm_item = MenuItem::with_id(app, "warm", "Warm up", true, None::<&str>)?;
    let off_item = MenuItem::with_id(app, "off", "Turn off", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit Warden", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&warm_item, &off_item, &quit_item])?;

    let handler_power = power.clone();
    TrayIconBuilder::with_id("warden")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Warden — Off")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "warm" => warm(app, handler_power.clone()),
            "off" => off(app, handler_power.clone()),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    // The poller notices what the menu did not: Ollama's own timeout expiring,
    // or `capture off` being run in another window.
    let app_handle = app.clone();
    std::thread::spawn(move || {
        loop {
            refresh(&app_handle, &power);
            std::thread::sleep(POLL);
        }
    });

    Ok(())
}
