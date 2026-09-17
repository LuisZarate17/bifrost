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

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::Manager;
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

/// The menu items the poller keeps up to date.
///
/// The two power items are **checkmarks, not verbs**. "Warm up" and "Turn off"
/// read as buttons, and a button that has already been pressed looks exactly like
/// one that has not — which is how the menu came to look broken while working.
/// A tick says which state you are *in*, and clicking the other one moves there.
pub struct TrayItems<R: Runtime> {
    pub status: MenuItem<R>,
    pub loaded: CheckMenuItem<R>,
    pub released: CheckMenuItem<R>,
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
    /// The card's reading the last time nothing of ours was on it.
    ///
    /// Warden's own share is this subtracted from the current reading. Ollama
    /// reports a `size_vram` of its own, but C4a measured that against the card
    /// and found it **~2,500 MiB low** — it counts the weights, not the context
    /// and compute buffers the runner also allocates. Measuring the difference
    /// ourselves is the same self-baselining the CLI uses, and for the same
    /// reason: the idle figure moves all day, so it has to be sampled, never
    /// assumed.
    idle_mib: Mutex<Option<u64>>,
}

impl Power {
    pub fn new(model: String, num_ctx: u32) -> Self {
        Self {
            state: AtomicU8::new(State::Off.code()),
            model,
            num_ctx,
            warming_since: Mutex::new(None),
            idle_mib: Mutex::new(None),
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
        // Already resident? Say so. The CLI prints "already resident - nothing to
        // do"; the tray previously just called load, which returned in 0.0s and
        // looked from the outside like the click had done nothing at all.
        let already = ollama::ps()
            .map(|m| ollama::find(&m, power.model()).is_some())
            .unwrap_or(false);
        if already {
            power.end_warming();
            power.set(State::Ready);
            println!("[warden] already resident — nothing to warm");
            refresh(&app, &power);
            return;
        }

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
/// Ask both sources what is true, and reconcile.
///
/// Returns the state, the tooltip, and the menu line — three renderings of the
/// same answer for three places that show it.
fn observe(power: &Power) -> (State, String, String, String) {
    // A load we started outranks what Ollama can see, because it cannot see one
    // in progress at all.
    if let Some(elapsed) = power.warming_for() {
        if elapsed < WARMING_CEILING {
            let secs = elapsed.as_secs_f64();
            return (
                State::Warming,
                format!("Warden — warming… ({secs:.0}s)"),
                format!("Loading… ({secs:.0}s)"),
                "Loaded".to_string(),
            );
        }
    }

    let loaded = match ollama::ps() {
        Ok(models) => ollama::find(&models, power.model()).is_some(),
        // Ollama being unreachable is not the same as the model being off, and
        // both places should say which it is.
        Err(_) => {
            return (
                State::Off,
                "Warden — Ollama not running".to_string(),
                "Ollama not running".to_string(),
                "Loaded".to_string(),
            );
        }
    };

    let state = if loaded { State::Ready } else { State::Off };
    let sample = vram::Card::open().ok().and_then(|c| c.sample().ok());

    // Every moment the model is off is a fresh reading of the card without us on
    // it. Kept, so Warden's own share is a measured difference rather than a
    // guess - and re-taken constantly, because the idle figure drifts with
    // whatever else is open.
    if state == State::Off {
        if let Some(s) = sample {
            *power.idle_mib.lock().unwrap() = Some(s.used_mib);
        }
    }

    let ours_mib = match (state, sample, *power.idle_mib.lock().unwrap()) {
        (State::Off, _, _) => Some(0),
        (_, Some(s), Some(idle)) => Some(s.used_mib.saturating_sub(idle)),
        // No idle reading yet this session - Ollama's own figure is the only
        // number available, and C4a measured it running ~2,500 MiB low because it
        // counts the weights but not the runner's context and compute buffers.
        // Better than showing nothing, worth not trusting.
        _ => ollama::ps()
            .ok()
            .and_then(|m| ollama::find(&m, power.model()).and_then(|e| e.size_vram_mib())),
    };

    let gb = |mib: u64| mib as f64 / 1024.0;

    let card = match (sample, ours_mib) {
        (Some(s), Some(ours)) => format!(
            "Warden {:.1} · {:.1} of {:.1} GB in use",
            gb(ours),
            gb(s.used_mib),
            gb(s.total_mib)
        ),
        (Some(s), None) => format!("{:.1} of {:.1} GB in use", gb(s.used_mib), gb(s.total_mib)),
        _ => "VRAM readout unavailable".to_string(),
    };

    // The menu line carries the GPU figure, because that is the number Warm up
    // and Turn off actually move. Without it the two items look identical from
    // the outside: warming pays off on the *next* press, which is invisible at
    // the moment you click. With it, the menu visibly goes from ~1 GB to ~9 GB.
    let line = match sample {
        Some(s) => format!("Warden · {:.1} / {:.1} GB used", gb(s.used_mib), gb(s.total_mib)),
        None => state.label().to_string(),
    };

    // Only worth a figure when there is one: "Loaded 0.0 GB" while released is
    // noise on the item you are not in.
    let loaded_label = match (state, ours_mib) {
        (State::Ready, Some(ours)) if ours > 0 => format!("Loaded · {:.1} GB", gb(ours)),
        _ => "Loaded".to_string(),
    };

    (
        state,
        format!("Warden — {}\n{}", state.label(), card),
        line,
        loaded_label,
    )
}

/// Re-read the world and push it to the tray.
pub fn refresh<R: Runtime>(app: &AppHandle<R>, power: &Power) {
    let (state, tip, menu_line, loaded_label) = observe(power);
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
    // And the menu's own line, so opening it answers the question a click was
    // about to ask.
    if let Some(items) = app.try_state::<TrayItems<R>>() {
        let _ = items.status.set_text(&menu_line);
        // Exactly one tick, always — the pair reads as a choice rather than two
        // buttons whose effect you have to remember.
        let _ = items.loaded.set_text(&loaded_label);
        let _ = items.loaded.set_checked(state == State::Ready);
        let _ = items.released.set_checked(state == State::Off);
    }
}

/// Build the tray and start the poller.
pub fn install<R: Runtime>(app: &AppHandle<R>, power: Arc<Power>) -> tauri::Result<()> {
    // A disabled first line carrying the current state.
    //
    // Without it the menu is three verbs and no feedback: clicking "Warm up" on a
    // model that is already resident returns instantly and looks identical to
    // nothing happening. The tooltip does say, but only if you think to hover,
    // and you have just clicked rather than hovered. Tested and found wanting.
    let status_item = MenuItem::with_id(app, "status", "…", false, None::<&str>)?;
    // Named for what you get, not for what the machine does. "Warm up" describes
    // the mechanism; "answers are instant" describes the reason you would click.
    let warm_item = CheckMenuItem::with_id(
        app, "warm", "Loaded", true, false, None::<&str>,
    )?;
    let off_item = CheckMenuItem::with_id(
        app, "off", "Released", true, false, None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit Warden", true, None::<&str>)?;

    // A separator above Quit, because it sits directly under "Turn off" and the
    // two got confused twice in one testing session. One is reversible in a
    // click; the other ends the process and takes the hotkey with it.
    let gap = PredefinedMenuItem::separator(app)?;
    let gap2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&status_item, &gap2, &warm_item, &off_item, &gap, &quit_item],
    )?;

    // Kept so the poller can rewrite the line as the state moves.
    app.manage(TrayItems {
        status: status_item.clone(),
        loaded: warm_item.clone(),
        released: off_item.clone(),
    });

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
