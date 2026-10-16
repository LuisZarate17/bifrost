//! The tray: Bifrost's, the suite's always-there control panel. It lives in Vidar's process
//! because that is the one process that runs in the background.
//!
//! The model's power state: three states you set, and can see.
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

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::path::{Path, PathBuf};

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu};
use tauri::Manager;
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Runtime};

use bifrost_core::{config, ollama, vram};

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
/// The power item is **one switch, not a verb**. "Warm up" and "Turn off" read as
/// buttons, and a button that has already been pressed looks exactly like one that
/// has not — which is how the menu came to look broken while working. A tick says
/// the model is loaded, and clicking it moves to the other state.
pub struct TrayItems<R: Runtime> {
    pub status: MenuItem<R>,
    pub loaded: CheckMenuItem<R>,
    pub models: Submenu<R>,
    pub login: CheckMenuItem<R>,
    /// What the model submenu shows now, so it is rebuilt only when it changes.
    shown: Mutex<Vec<PickerEntry>>,
}

/// Shared power state. `AtomicU8` because the poller thread, the menu handlers
/// and the hotkey path all read it, and none of them should be able to block
/// the others on a lock just to ask what the state is.
pub struct Power {
    state: AtomicU8,
    num_ctx: u32,
    /// Set while a load we issued is in flight — this is what makes Warming
    /// observable at all.
    warming_since: Mutex<Option<std::time::Instant>>,
    /// The card's reading the last time nothing of ours was on it.
    ///
    /// Vidar's own share is this subtracted from the current reading. Ollama
    /// reports a `size_vram` of its own, but C4a measured that against the card
    /// and found it **~2,500 MiB low** — it counts the weights, not the context
    /// and compute buffers the runner also allocates. Measuring the difference
    /// ourselves is the same self-baselining the CLI uses, and for the same
    /// reason: the idle figure moves all day, so it has to be sampled, never
    /// assumed.
    idle_mib: Mutex<Option<u64>>,
    /// The config's model tag and the file stamp it was read at. See [`Power::model`].
    #[allow(clippy::type_complexity)]
    model_cache: Mutex<Option<(Option<std::time::SystemTime>, String)>>,
}

impl Power {
    pub fn new(num_ctx: u32) -> Self {
        Self {
            state: AtomicU8::new(State::Off.code()),
            num_ctx,
            warming_since: Mutex::new(None),
            idle_mib: Mutex::new(None),
            model_cache: Mutex::new(None),
        }
    }

    pub fn state(&self) -> State {
        State::from_code(self.state.load(Ordering::Relaxed))
    }

    fn set(&self, s: State) {
        self.state.store(s.code(), Ordering::Relaxed);
    }

    /// The model the shared config names, re-read when the file changes.
    ///
    /// **Not stored at construction.** Mimir writes the current model to the
    /// config, and a tray holding the tag it saw at startup would warm, release
    /// and report on a model nobody had chosen since - silently, because every
    /// one of those operations succeeds against the wrong tag.
    ///
    /// **Cached on the file's modified time**, because `refresh` *is* a loop:
    /// it runs every `POLL` for as long as Vidar is open, and calls this twice
    /// a tick. An earlier version of this comment claimed the tray read the
    /// config "on a menu action or a refresh, not in a loop", which was simply
    /// wrong - it was two file reads every five seconds, forever, against the
    /// same argument `POLL` itself is justified by.
    ///
    /// A `stat` is far cheaper than a read-and-parse, and the config changes
    /// when someone picks a model - rarely, and never behind your back.
    pub fn model(&self) -> String {
        let stamp = bifrost_core::config::path()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());

        if let Ok(cache) = self.model_cache.lock() {
            if let Some((cached_stamp, tag)) = cache.as_ref() {
                if *cached_stamp == stamp {
                    return tag.clone();
                }
            }
        }

        let tag = bifrost_core::config::load().model;
        if let Ok(mut cache) = self.model_cache.lock() {
            *cache = Some((stamp, tag.clone()));
        }
        tag
    }

    /// Drop the cached tag, so the next [`Power::model`] re-reads the config even if the
    /// file's stamp has not moved yet (a pick and the read can land in one tick).
    fn forget_model(&self) {
        if let Ok(mut cache) = self.model_cache.lock() {
            *cache = None;
        }
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
    std::thread::spawn(move || load_now(&app, &power));
}

/// The body of a load; the caller has already set Warming.
fn load_now<R: Runtime>(app: &AppHandle<R>, power: &Power) {
    // Same `num_ctx` the capture path sends. Ollama keys a loaded runner by
    // the model *and* the options that change its memory layout, so warming
    // at a different context silently reloads on the next capture and buys
    // nothing at all. C4a proved the match by watching `load_duration`.
    // Already resident? Say so. The CLI prints "already resident - nothing to
    // do"; the tray previously just called load, which returned in 0.0s and
    // looked from the outside like the click had done nothing at all.
    // Resident *and pinned*: a model a chat loaded is resident too, but due
    // to unload in minutes, and ticking Model loaded over that would be a lie.
    let already = ollama::ps()
        .map(|m| ollama::find(&m, &power.model()).is_some_and(|m| m.pinned()))
        .unwrap_or(false);
    if already {
        power.end_warming();
        power.set(State::Ready);
        println!("[vidar] already resident — nothing to warm");
        refresh(app, power);
        return;
    }

    let result = ollama::load(&power.model(), power.num_ctx);
    power.end_warming();
    match result {
        Ok(elapsed) => {
            power.set(State::Ready);
            println!("[vidar] warmed in {:.1}s", elapsed.as_secs_f64());
        }
        Err(e) => {
            power.set(State::Off);
            eprintln!("[vidar] warm failed: {e}");
        }
    }
    refresh(app, power);
}

pub fn off<R: Runtime>(app: &AppHandle<R>, power: Arc<Power>) {
    let app = app.clone();
    std::thread::spawn(move || {
        release(&power.model());
        power.set(State::Off);
        refresh(&app, &power);
    });
}

/// Whether the model is gone from Ollama's list afterwards.
fn release(tag: &str) -> bool {
    match ollama::unload(tag) {
        Ok(true) => {
            println!("[vidar] released");
            true
        }
        Ok(false) => {
            eprintln!("[vidar] {tag} is still resident after 30s");
            false
        }
        Err(e) => {
            eprintln!("[vidar] off failed: {e}");
            false
        }
    }
}

/// `refresh` off the calling thread. The menu handler runs on the main thread, and a refresh
/// makes HTTP calls (`ps`, `tags`, `show` per model), so the handler must never call it directly.
fn refresh_async<R: Runtime>(app: &AppHandle<R>, power: Arc<Power>) {
    let app = app.clone();
    std::thread::spawn(move || refresh(&app, &power));
}

/// Whether a picker item with this id is ticked: it is the configured model's.
fn picker_tick(id: &str, configured: &str) -> bool {
    id.strip_prefix(MODEL_PREFIX).is_some_and(|tag| tag == configured)
}

/// Put every picker item's tick on the configured model. A click flips an item's own tick
/// whatever the handler then decides, so every outcome that does not rebuild the list must
/// call this. It is local (no network, no rebuild), so it is safe on the main thread.
fn sync_ticks<R: Runtime>(app: &AppHandle<R>, configured: &str) {
    let Some(items) = app.try_state::<TrayItems<R>>() else { return };
    let Ok(all) = items.models.items() else { return };
    for kind in all {
        if let MenuItemKind::Check(c) = kind {
            let _ = c.set_checked(picker_tick(c.id().as_ref(), configured));
        }
    }
}

/// Pick `tag` as the model. The switch decides what else happens: on, the previous
/// model is released and the new one loaded (Warming meanwhile); off, nothing is loaded.
fn pick_model<R: Runtime>(app: &AppHandle<R>, power: Arc<Power>, tag: String) {
    let previous = power.model();
    if tag == previous {
        sync_ticks(app, &previous); // the click un-ticked it; put the tick back
        return;
    }
    if power.state() == State::Warming {
        eprintln!("[vidar] model pick ignored while a load is in flight");
        sync_ticks(app, &previous);
        return;
    }
    if let Err(e) = config::update(|c| c.model = tag.clone()) {
        eprintln!("[vidar] could not save the model choice: {e}");
        sync_ticks(app, &previous);
        return;
    }
    power.forget_model();
    // The click ticked the new item and left the old one ticked too.
    sync_ticks(app, &tag);
    println!("[vidar] model: {previous} -> {tag}");
    if switch_on(power.state()) {
        power.begin_warming();
        let app = app.clone();
        std::thread::spawn(move || {
            // A model busy for over 30 s may still be resident. Load the new one anyway:
            // Ollama makes room or fails, and the switch shows which.
            if !release(&previous) {
                eprintln!("[vidar] loading {tag} while {previous} may still be resident");
            }
            load_now(&app, &power);
        });
    } else {
        refresh_async(app, power);
    }
}

/// What one look at the world found.
struct Observed {
    state: State,
    /// The menu's first line: Off, Warming…, Ready · 18.2 GB.
    status: String,
    /// The tooltip.
    tooltip: String,
    /// The installed models, or None when Ollama did not answer.
    installed: Option<Vec<ollama::InstalledModel>>,
}

/// The status line: the state, and what the model holds when it is resident.
fn status_text(state: State, ours_mib: Option<u64>) -> String {
    match (state, ours_mib) {
        (State::Ready, Some(mib)) if mib > 0 => format!("Ready · {:.1} GB", mib as f64 / 1024.0),
        _ => state.label().to_string(),
    }
}

/// The tooltip: the status, then the card's figure when there is one.
fn tooltip_text(status: &str, card: Option<(u64, u64)>) -> String {
    match card {
        Some((used, total)) => format!(
            "Bifrost — {status}\n{:.1} of {:.1} GB in use",
            used as f64 / 1024.0,
            total as f64 / 1024.0
        ),
        None => format!("Bifrost — {status}"),
    }
}

/// Whether the "Model loaded" switch reads on. Warming counts: the click asked for it,
/// and a switch that flipped back while the load ran would look like it was refused.
fn switch_on(state: State) -> bool {
    state != State::Off
}

/// One line of the model picker.
#[derive(Clone, PartialEq, Eq, Debug)]
struct PickerEntry {
    /// The model's tag, which is also what picking it sets.
    name: String,
    label: String,
    checked: bool,
    enabled: bool,
}

const PICKER_HINT: &str = "Start the model to list the others";

/// The picker from the installed models (name, can it see images), the configured one,
/// and whether Ollama answered. A model with no vision is labelled, because Vidar needs
/// one that can. With no list, only the configured model and a disabled hint.
fn picker_entries(installed: Option<&[(String, bool)]>, configured: &str) -> Vec<PickerEntry> {
    let entry = |name: &str, vision: Option<bool>| PickerEntry {
        name: name.to_string(),
        label: if vision == Some(false) {
            format!("{name} — text only")
        } else {
            name.to_string()
        },
        checked: name == configured,
        enabled: true,
    };
    match installed {
        Some(models) => {
            let mut out: Vec<_> = models.iter().map(|(n, v)| entry(n, Some(*v))).collect();
            // The configured tag may not be installed (yet); it is still the choice.
            if !models.iter().any(|(n, _)| n == configured) {
                out.insert(0, entry(configured, None));
            }
            out
        }
        None => vec![
            entry(configured, None),
            PickerEntry {
                name: String::new(),
                label: PICKER_HINT.to_string(),
                checked: false,
                enabled: false,
            },
        ],
    }
}

/// Whether this look may update the idle baseline: nothing of ours is loaded and no other
/// model is resident either, so the card reads what the rest of the machine is using.
fn takes_idle_baseline(state: State, any_resident: bool) -> bool {
    state == State::Off && !any_resident
}

/// Ask both sources what is true, and reconcile.
///
/// Ollama is authoritative for *whether* the model is loaded. The card is
/// authoritative for *how much memory is actually gone*. Neither substitutes for
/// the other.
///
/// **This only asks.** Starting at login runs this within seconds, and it must load
/// nothing and start nothing: `ps` and `tags` are reads, and `ensure_running` is
/// never called from the tray's poll.
fn observe(power: &Power) -> Observed {
    // A load we started outranks what Ollama can see, because it cannot see one
    // in progress at all.
    if let Some(elapsed) = power.warming_for() {
        if elapsed < WARMING_CEILING {
            let status = State::Warming.label().to_string();
            return Observed {
                state: State::Warming,
                tooltip: tooltip_text(&status, None),
                status,
                installed: None,
            };
        }
    }

    let (loaded, any_resident) = match ollama::ps() {
        Ok(models) => (ollama::find(&models, &power.model()).is_some(), !models.is_empty()),
        // Ollama being unreachable is not the same as the model being off, but
        // for the switch it is the same answer: nothing is loaded.
        Err(_) => {
            let status = State::Off.label().to_string();
            return Observed {
                state: State::Off,
                tooltip: format!("Bifrost — {status} (Ollama not running)"),
                status,
                installed: None,
            };
        }
    };

    let state = if loaded { State::Ready } else { State::Off };
    let sample = vram::Card::open().ok().and_then(|c| c.sample().ok());

    // Every moment the model is off is a fresh reading of the card without us on
    // it. Kept, so our own share is a measured difference rather than a
    // guess - and re-taken constantly, because the idle figure drifts with
    // whatever else is open.
    // Not while any model is resident, though: ours may be off while another still holds
    // memory, and that would be baked into the idle figure.
    if takes_idle_baseline(state, any_resident) {
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
            .and_then(|m| ollama::find(&m, &power.model()).and_then(|e| e.size_vram_mib())),
    };

    let status = status_text(state, ours_mib);
    Observed {
        state,
        tooltip: tooltip_text(&status, sample.map(|s| (s.used_mib, s.total_mib))),
        status,
        installed: ollama::tags().ok(),
    }
}

/// Re-read the world and push it to the tray.
pub fn refresh<R: Runtime>(app: &AppHandle<R>, power: &Power) {
    let seen = observe(power);
    let state = seen.state;
    // Transitions only. The poller runs all day; logging every tick would bury
    // everything else, and a change is the only part worth a line.
    let previous = power.state();
    if previous != state {
        println!("[vidar] power: {} -> {}", previous.label(), state.label());
    }
    power.set(state);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(&seen.tooltip));
    }
    // And the menu's own line, so opening it answers the question a click was
    // about to ask.
    if let Some(items) = app.try_state::<TrayItems<R>>() {
        let _ = items.status.set_text(&seen.status);
        // The switch follows the real state, whatever the click last left it at.
        let _ = items.loaded.set_checked(switch_on(state));
        let _ = items.login.set_checked(startup_shortcut().is_some_and(|p| p.exists()));
        // Every refresh, rebuilt or not: a click may have left the ticks wrong.
        sync_ticks(app, &power.model());

        // Warming shows no list (a load is in flight); keep what is there.
        if state != State::Warming {
            let probed: Option<Vec<(String, bool)>> = seen.installed.map(|models| {
                models
                    .into_iter()
                    .map(|m| {
                        let vision = ollama::show_cached(&m.name).map(|d| d.has("vision")).unwrap_or(true);
                        (m.name, vision)
                    })
                    .collect()
            });
            let entries = picker_entries(probed.as_deref(), &power.model());
            let mut shown = items.shown.lock().unwrap();
            if *shown != entries && rebuild_picker(app, &items.models, &entries).is_ok() {
                *shown = entries;
            }
        }
    }
}

/// Replace the submenu's contents with `entries`.
fn rebuild_picker<R: Runtime>(app: &AppHandle<R>, menu: &Submenu<R>, entries: &[PickerEntry]) -> tauri::Result<()> {
    for old in menu.items()? {
        menu.remove(&old)?;
    }
    for e in entries {
        let id = format!("{MODEL_PREFIX}{}", e.name);
        let item = CheckMenuItem::with_id(app, id, &e.label, e.enabled, e.checked, None::<&str>)?;
        menu.append(&item)?;
    }
    Ok(())
}

const TRAY_ID: &str = "bifrost";
const MODEL_PREFIX: &str = "model:";

/// The mark at each size the tray can use.
const TRAY_ICONS: [(u32, &[u8]); 4] = [
    (16, include_bytes!("../icons/tray-16.png")),
    (20, include_bytes!("../icons/tray-20.png")),
    (24, include_bytes!("../icons/tray-24.png")),
    (32, include_bytes!("../icons/tray-32.png")),
];

/// The icon size for a display scale: 16 px at 100%, the next size up from there, 32 at most.
fn tray_icon_size(scale: f64) -> u32 {
    let want = (16.0 * scale).ceil() as u32;
    TRAY_ICONS.iter().map(|(s, _)| *s).find(|s| *s >= want).unwrap_or(32)
}

fn tray_icon<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Image<'static>> {
    let scale = app.primary_monitor().ok().flatten().map_or(1.0, |m| m.scale_factor());
    let size = tray_icon_size(scale);
    let bytes = TRAY_ICONS.iter().find(|(s, _)| *s == size).map_or(TRAY_ICONS[3].1, |(_, b)| *b);
    Image::from_bytes(bytes)
}

/// `Bifrost.lnk` in the user's Startup folder, under the roaming app data folder.
fn startup_shortcut_in(appdata: &Path) -> PathBuf {
    appdata
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("Bifrost.lnk")
}

fn startup_shortcut() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| startup_shortcut_in(Path::new(&a)))
}

/// The PowerShell that writes the shortcut. The paths are never part of the text: they come in
/// through the environment (`BIFROST_LNK`, `BIFROST_TARGET`, `BIFROST_DIR`), because no quoting
/// rule covers every character PowerShell treats as a quote (the curly ones included).
const WRITE_SHORTCUT: &str = "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:BIFROST_LNK);$s.TargetPath=$env:BIFROST_TARGET;$s.WorkingDirectory=$env:BIFROST_DIR;$s.Description='Bifrost';$s.Save()";

/// The PowerShell that prints the shortcut's target, with the path in `BIFROST_LNK`.
const READ_SHORTCUT: &str = "(New-Object -ComObject WScript.Shell).CreateShortcut($env:BIFROST_LNK).TargetPath";

/// `CREATE_NO_WINDOW`, so the PowerShell that writes the shortcut never flashes a console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`: a program we start must outlive us and
/// not share our console or our Ctrl+C.
#[cfg(windows)]
const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;

/// Run `script` in PowerShell with `envs` set on the child; returns what it printed.
fn run_ps(script: &str, envs: &[(&str, &Path)]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn create_startup_shortcut() -> Result<(), String> {
    let lnk = startup_shortcut().ok_or("no APPDATA")?;
    let target = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = target.parent().unwrap_or(Path::new(".")).to_path_buf();
    run_ps(
        WRITE_SHORTCUT,
        &[("BIFROST_LNK", &lnk), ("BIFROST_TARGET", &target), ("BIFROST_DIR", &dir)],
    )?;
    if lnk.exists() {
        Ok(())
    } else {
        Err("the shortcut was not written".to_string())
    }
}

/// Whether a shortcut whose target reads `read` points somewhere other than `exe`. Windows
/// paths differ by case without differing in meaning.
fn shortcut_is_stale(read: &str, exe: &Path) -> bool {
    !read.trim().eq_ignore_ascii_case(&exe.to_string_lossy())
}

/// A Startup shortcut left pointing at an exe that has moved (or been rebuilt elsewhere) would
/// start nothing at login; point it at the running exe. No shortcut means the user turned
/// it off, so none is made.
fn repair_startup_shortcut() {
    let Some(lnk) = startup_shortcut().filter(|p| p.exists()) else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    match run_ps(READ_SHORTCUT, &[("BIFROST_LNK", &lnk)]) {
        Ok(read) if !shortcut_is_stale(&read, &exe) => {}
        Ok(read) => match create_startup_shortcut() {
            Ok(()) => println!("[vidar] start at login: repaired the shortcut ({read} -> {})", exe.display()),
            Err(e) => eprintln!("[vidar] could not repair the startup shortcut: {e}"),
        },
        Err(e) => eprintln!("[vidar] could not read the startup shortcut: {e}"),
    }
}

fn remove_startup_shortcut() -> Result<(), String> {
    let lnk = startup_shortcut().ok_or("no APPDATA")?;
    match std::fs::remove_file(&lnk) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

/// The switch was clicked: the shortcut is made if it is not there, removed if it is.
fn toggle_login() {
    // Two quick clicks would both read "not there" and race to write it (or to remove it).
    // The one that finds a toggle running is dropped; the refresh re-syncs the tick from disk.
    if LOGIN_BUSY.swap(true, Ordering::SeqCst) {
        eprintln!("[vidar] start at login: a change is already running, click ignored");
        return;
    }
    let there = startup_shortcut().is_some_and(|p| p.exists());
    let result = if there { remove_startup_shortcut() } else { create_startup_shortcut() };
    match result {
        Ok(()) => {
            println!("[vidar] start at login: {}", if there { "off" } else { "on" });
            // The user has chosen now, so the first-run default must never override it.
            if let Err(e) = config::update(|c| c.bifrost_login_chosen = true) {
                eprintln!("[vidar] could not record the login choice: {e}");
            }
        }
        Err(e) => eprintln!("[vidar] start at login failed: {e}"),
    }
    LOGIN_BUSY.store(false, Ordering::SeqCst);
}

/// Set while `toggle_login` runs.
static LOGIN_BUSY: AtomicBool = AtomicBool::new(false);

/// The first run of the tray turns "Start at login" on, once. The choice is recorded
/// only when the shortcut is in place, so a failure is tried again at the next start.
fn first_run_login() {
    if config::load().bifrost_login_chosen {
        return;
    }
    let there = startup_shortcut().is_some_and(|p| p.exists());
    let made = there
        || create_startup_shortcut()
            .map_err(|e| eprintln!("[vidar] start at login failed: {e}"))
            .is_ok();
    if made {
        match config::update(|c| c.bifrost_login_chosen = true) {
            Ok(()) => println!("[vidar] start at login: on (first run)"),
            Err(e) => eprintln!("[vidar] could not record the login choice: {e}"),
        }
    }
}

/// A program beside the running exe.
fn beside_exe(name: &str) -> Option<PathBuf> {
    std::env::current_exe().ok().map(|e| e.with_file_name(name))
}

/// Where `hugin-launch.exe` is: the installed copy in the bin folder under the local app
/// data folder, else the one beside the running exe.
fn hugin_launcher(local_appdata: Option<&Path>, beside: Option<PathBuf>, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    local_appdata
        .map(|l| l.join("bifrost").join("bin").join("hugin-launch.exe"))
        .filter(|p| exists(p))
        .or(beside)
}

fn spawn_detached(program: &Path) {
    let mut cmd = std::process::Command::new(program);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED);
    }
    if let Err(e) = cmd.spawn() {
        eprintln!("[vidar] could not start {}: {e}", program.display());
    }
}

/// Start Hugin's launcher, never waiting on it. Used by the menu and by Ctrl+Alt+H.
pub fn open_hugin() {
    let local = std::env::var_os("LOCALAPPDATA");
    match hugin_launcher(local.as_deref().map(Path::new), beside_exe("hugin-launch.exe"), |p| p.exists()) {
        Some(p) => spawn_detached(&p),
        None => eprintln!("[vidar] cannot find hugin-launch.exe"),
    }
}

fn open_mimir() {
    match beside_exe("mimir.exe") {
        Some(p) => spawn_detached(&p),
        None => eprintln!("[vidar] cannot find mimir.exe"),
    }
}

/// Build the tray and start the poller.
///
/// Not generic over the runtime: "Open Vidar" calls `toggle_popup`, which takes the app's own.
pub fn install(app: &AppHandle, power: Arc<Power>) -> tauri::Result<()> {
    // A disabled first line carrying the current state.
    //
    // Without it the menu is verbs and no feedback: clicking on a model that is
    // already resident returns instantly and looks identical to nothing
    // happening. The tooltip does say, but only if you think to hover, and you
    // have just clicked rather than hovered. Tested and found wanting.
    let status_item = MenuItem::with_id(app, "status", "…", false, None::<&str>)?;
    let loaded_item = CheckMenuItem::with_id(app, "loaded", "Model loaded", true, false, None::<&str>)?;
    let models = Submenu::with_id(app, "models", "Model", true)?;
    let vidar_item = MenuItem::with_id(app, "open_vidar", "Open Vidar", true, Some("Ctrl+Alt+W"))?;
    let mimir_item = MenuItem::with_id(app, "open_mimir", "Open Mimir", true, None::<&str>)?;
    let hugin_item = MenuItem::with_id(app, "open_hugin", "Open Hugin", true, Some("Ctrl+Alt+H"))?;
    let login_item = CheckMenuItem::with_id(
        app,
        "login",
        "Start at login",
        true,
        startup_shortcut().is_some_and(|p| p.exists()),
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit Bifrost", true, None::<&str>)?;

    let gap1 = PredefinedMenuItem::separator(app)?;
    let gap2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &status_item,
            &loaded_item,
            &models,
            &gap1,
            &vidar_item,
            &mimir_item,
            &hugin_item,
            &gap2,
            &login_item,
            &quit_item,
        ],
    )?;

    // The configured model, checked, until the first poll lists the rest.
    let first = picker_entries(None, &power.model());
    rebuild_picker(app, &models, &first)?;

    // Kept so the poller can rewrite the line as the state moves.
    app.manage(TrayItems {
        status: status_item.clone(),
        loaded: loaded_item.clone(),
        models: models.clone(),
        login: login_item.clone(),
        shown: Mutex::new(first),
    });

    let handler_power = power.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tray_icon(app)?)
        .tooltip("Bifrost — Off")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref();
            match id {
                // The click has already flipped the tick; the state decides the move.
                "loaded" => match handler_power.state() {
                    State::Off => warm(app, handler_power.clone()),
                    State::Ready => off(app, handler_power.clone()),
                    State::Warming => {
                        sync_ticks(app, &handler_power.model());
                        refresh_async(app, handler_power.clone());
                    }
                },
                "open_vidar" => {
                    // As the hotkey does: off the menu's thread, which holds the message pump.
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move { crate::toggle_popup(&app, "tray") });
                }
                "open_mimir" => open_mimir(),
                "open_hugin" => open_hugin(),
                "login" => {
                    let (app, power) = (app.clone(), handler_power.clone());
                    std::thread::spawn(move || {
                        toggle_login();
                        refresh(&app, &power);
                    });
                }
                "quit" => app.exit(0),
                _ => {
                    if let Some(tag) = id.strip_prefix(MODEL_PREFIX) {
                        if !tag.is_empty() {
                            pick_model(app, handler_power.clone(), tag.to_string());
                        }
                    }
                }
            }
        })
        .build(app)?;

    // The poller notices what the menu did not: Ollama's own timeout expiring,
    // or `capture off` being run in another window. It only asks.
    let app_handle = app.clone();
    std::thread::spawn(move || {
        // Off the setup thread: writing the shortcut runs PowerShell.
        first_run_login();
        repair_startup_shortcut();
        loop {
            refresh(&app_handle, &power);
            std::thread::sleep(POLL);
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Vec<(String, bool)> {
        vec![("vl:8b".into(), true), ("coder:30b".into(), false)]
    }

    #[test]
    fn the_status_line_names_the_state_and_what_is_held() {
        assert_eq!(status_text(State::Off, Some(0)), "Off");
        assert_eq!(status_text(State::Warming, None), "Warming…");
        assert_eq!(status_text(State::Ready, Some(18_637)), "Ready · 18.2 GB");
        assert_eq!(status_text(State::Ready, None), "Ready");
        assert_eq!(status_text(State::Ready, Some(0)), "Ready");
    }

    #[test]
    fn the_tooltip_leads_with_bifrost_and_the_status() {
        assert_eq!(tooltip_text("Off", None), "Bifrost — Off");
        assert_eq!(
            tooltip_text("Ready · 8.0 GB", Some((10240, 16384))),
            "Bifrost — Ready · 8.0 GB\n10.0 of 16.0 GB in use"
        );
    }

    #[test]
    fn the_switch_is_on_unless_the_model_is_off() {
        assert!(!switch_on(State::Off));
        assert!(switch_on(State::Warming));
        assert!(switch_on(State::Ready));
    }

    #[test]
    fn the_picker_checks_the_configured_model_and_labels_text_only_ones() {
        let e = picker_entries(Some(&models()), "vl:8b");
        let shown: Vec<_> = e.iter().map(|x| (x.label.as_str(), x.checked)).collect();
        assert_eq!(shown, [("vl:8b", true), ("coder:30b — text only", false)]);
        // Picking is by tag, whatever the label says.
        assert_eq!(e[1].name, "coder:30b");
        assert!(e.iter().all(|x| x.enabled));
    }

    #[test]
    fn a_configured_model_that_is_not_installed_still_shows_checked() {
        let e = picker_entries(Some(&models()), "gone:1b");
        assert_eq!((e[0].name.as_str(), e[0].checked), ("gone:1b", true));
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn without_ollama_the_picker_holds_the_configured_model_and_a_hint() {
        let e = picker_entries(None, "vl:8b");
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].label.as_str(), e[0].checked, e[0].enabled), ("vl:8b", true, true));
        assert_eq!((e[1].label.as_str(), e[1].checked, e[1].enabled), (PICKER_HINT, false, false));
    }

    #[test]
    fn the_startup_shortcut_is_bifrost_lnk_in_the_startup_folder() {
        let base = Path::new(r"C:\Users\u\AppData\Roaming");
        assert_eq!(
            startup_shortcut_in(base),
            base.join(r"Microsoft\Windows\Start Menu\Programs\Startup\Bifrost.lnk")
        );
    }

    #[test]
    fn the_shortcut_scripts_hold_no_path() {
        for script in [WRITE_SHORTCUT, READ_SHORTCUT] {
            assert!(script.contains("$env:BIFROST_LNK"));
            assert!(!script.contains(":\\"), "a path is embedded: {script}");
        }
        assert!(WRITE_SHORTCUT.contains("$env:BIFROST_TARGET") && WRITE_SHORTCUT.contains("$env:BIFROST_DIR"));
    }

    #[test]
    fn a_shortcut_is_stale_unless_it_targets_the_running_exe() {
        let exe = Path::new(r"C:\app\vidar.exe");
        assert!(!shortcut_is_stale(r"C:\app\vidar.exe", exe));
        assert!(!shortcut_is_stale("c:\\APP\\Vidar.exe\r\n", exe));
        assert!(shortcut_is_stale(r"D:\old\vidar.exe", exe));
        assert!(shortcut_is_stale("", exe));
    }

    #[test]
    fn only_the_configured_models_item_is_ticked() {
        assert!(picker_tick("model:vl:8b", "vl:8b"));
        assert!(!picker_tick("model:coder:30b", "vl:8b"));
        assert!(!picker_tick("loaded", "vl:8b"));
        assert!(!picker_tick("model:", "vl:8b"));
    }

    #[test]
    fn the_idle_baseline_is_taken_only_with_nothing_resident() {
        assert!(takes_idle_baseline(State::Off, false));
        assert!(!takes_idle_baseline(State::Off, true));
        assert!(!takes_idle_baseline(State::Ready, false));
        assert!(!takes_idle_baseline(State::Ready, true));
    }

    #[test]
    fn hugin_launches_from_the_bin_folder_else_beside_the_exe() {
        let local = Path::new(r"C:\L");
        let beside = Some(PathBuf::from(r"C:\app\hugin-launch.exe"));
        let installed = PathBuf::from(r"C:\L\bifrost\bin\hugin-launch.exe");
        assert_eq!(hugin_launcher(Some(local), beside.clone(), |p| p == installed), Some(installed.clone()));
        assert_eq!(hugin_launcher(Some(local), beside.clone(), |_| false), beside);
        assert_eq!(hugin_launcher(None, beside.clone(), |_| true), beside);
    }

    #[test]
    fn the_tray_icon_size_follows_the_display_scale() {
        assert_eq!(tray_icon_size(1.0), 16);
        assert_eq!(tray_icon_size(1.25), 20);
        assert_eq!(tray_icon_size(1.5), 24);
        assert_eq!(tray_icon_size(2.0), 32);
        assert_eq!(tray_icon_size(3.0), 32);
    }
}
