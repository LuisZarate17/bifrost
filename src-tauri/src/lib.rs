use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::prelude::*;
use futures_util::StreamExt;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri::{Emitter, Manager};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
// In scope at module level, unlike the rest of the Win32 imports, because these
// two appear in the geometry helpers' signatures rather than only inside them.
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, RECT};

const OLLAMA_URL: &str = "http://127.0.0.1:11434/api/generate";
const DEFAULT_MODEL: &str = "qwen3-vl:8b-instruct-q4_K_M";

/// A 1080p screen costs ~2,042 image tokens (measured at A2). Ollama derives its
/// default from detected VRAM and lands at 4k on this card, which would silently
/// truncate the screenshot — so it is always set explicitly.
const NUM_CTX: u32 = 8192;

/// One captured frame as owned data. No borrows, so it can leave the callback
/// and cross a thread boundary.
struct Shot {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// One message pushed up to the UI. Sent through a `Channel`, never returned from
/// the command: a command's `Result` is delivered as a single IPC reply, so
/// returning the whole answer would buffer it by construction.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "event", content = "data")]
enum StreamEvent {
    /// The screenshot is taken; the model hasn't answered yet.
    Captured { width: u32, height: u32, png_bytes: usize },
    /// Ollama wasn't running, so this request is paying to start it.
    ///
    /// The last step between "open Warden" and "it works". A1 recorded that
    /// Ollama has no autostart and does not survive a reboot, which made the
    /// first press after a restart fail with an error rather than an answer.
    StartingOllama,
    /// The model wasn't resident, so this request is paying to load it.
    ///
    /// Ollama loads on demand, so a cold press never *fails* — it just sits
    /// there for an extra few seconds with no explanation. Saying so is the
    /// whole point: an unexplained stall reads as broken.
    Warming,
    /// One token of the answer.
    Token { text: String },
    /// The final object, carrying Ollama's own metrics.
    Done {
        prompt_eval_count: u32,
        eval_count: u32,
        total_ms: f64,
        /// Client-measured, because Ollama has no time-to-first-token field.
        ttft_ms: f64,
    },
}

struct CaptureHandler {
    sender: Sender<Shot>,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = Sender<Shot>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { sender: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let width = frame.width();
        let height = frame.height();

        let buffer = frame.buffer()?;
        let mut scratch = Vec::new();

        // `as_nopadding_buffer` returns a *view* into GPU memory that is unmapped
        // when this callback returns, and `Frame`/`FrameBuffer` are `!Send`/`!Sync`.
        // `.to_vec()` copies into memory we own, which is what lets the pixels
        // outlive the frame and cross to another thread. (The A3 lesson.)
        let pixels = buffer.as_nopadding_buffer(&mut scratch).to_vec();

        let _ = self.sender.send(Shot { pixels, width, height });

        control.stop();
        Ok(())
    }
}

/// Fully blocking. MUST only run via `spawn_blocking` — never directly in an
/// `async fn` body and never on Tauri's main thread.
///
/// Two separate reasons:
/// 1. `Capture::start` runs a real `GetMessageW`/`DispatchMessageW` loop on the
///    calling thread, which would park a tokio *worker* — the same small pool the
///    streaming command uses.
/// 2. `windows-capture` calls `RoInitialize(RO_INIT_MULTITHREADED)`, but WebView2
///    has already put Tauri's main thread into COM **STA**. A thread's apartment
///    cannot be changed, so that returns `RPC_E_CHANGED_MODE` → `FailedToInitWinRT`.
///    `spawn_blocking` hands us a fresh OS thread nobody has initialised COM on.
fn capture_screen_blocking() -> Result<Shot, String> {
    let (sender, receiver) = mpsc::channel::<Shot>();

    let monitor = Monitor::primary().map_err(|e| format!("no primary monitor: {e}"))?;
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        sender,
    );

    CaptureHandler::start(settings).map_err(|e| format!("screen capture failed: {e}"))?;

    receiver
        .recv()
        .map_err(|_| "capture stopped without producing a frame".to_string())
}

/// Encode raw RGBA bytes straight into PNG bytes, without touching the disk.
fn rgba_to_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(pixels, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(png)
}

/// The last screenshot, kept so a follow-up can ask about it again.
///
/// The whole point of a follow-up is that it does **not** take a new screenshot:
/// by the time you have read an answer and thought of a second question, the
/// screen has usually moved on — you scrolled, or the popup itself is sitting
/// over what you were asking about. Re-capturing would answer a question about a
/// different screen than the one you meant.
///
/// A `Mutex` rather than a channel because there is exactly one slot and both
/// commands are short-lived. Holds PNG bytes, already encoded, so a follow-up
/// costs no image work at all.
#[derive(Default)]
struct LastCapture(std::sync::Mutex<Option<Vec<u8>>>);

/// Capture the screen, send it to Ollama, and stream the answer back token by token.
mod tray;

#[tauri::command]
async fn ask_about_screen(
    prompt: String,
    model: Option<String>,
    on_event: Channel<StreamEvent>,
    last: tauri::State<'_, LastCapture>,
) -> Result<(), String> {
    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());

    // Before the capture, not after. Ollama takes the longest of the three
    // waits - 12.6s measured on a busy card - and reporting it second meant the
    // popup said "Reading your screen..." while it was actually waiting on a
    // server that had not started yet.
    if !tauri::async_runtime::spawn_blocking(warden_core::ollama::is_running)
        .await
        .unwrap_or(false)
    {
        let _ = on_event.send(StreamEvent::StartingOllama);
        let started = tauri::async_runtime::spawn_blocking(warden_core::ollama::ensure_running)
            .await
            .map_err(|e| format!("start thread panicked: {e}"))?;
        match started {
            Ok(Some(took)) => println!("[warden] started Ollama in {:.1}s", took.as_secs_f64()),
            Ok(None) => {}
            Err(e) => return Err(e),
        }
    }

    // See `capture_screen_blocking` — this must not run on an async worker.
    let shot = tauri::async_runtime::spawn_blocking(capture_screen_blocking)
        .await
        .map_err(|e| format!("capture thread panicked: {e}"))??;

    let png = rgba_to_png(&shot.pixels, shot.width, shot.height)?;

    on_event
        .send(StreamEvent::Captured {
            width: shot.width,
            height: shot.height,
            png_bytes: png.len(),
        })
        .map_err(|e| format!("channel send failed: {e}"))?;

    // Keep it for the follow-ups. Replaces whatever the previous question used —
    // a follow-up always refers to the answer currently on screen.
    *last.0.lock().map_err(|_| "capture store poisoned")? = Some(png.clone());

    ask_with_png(png, prompt, model, on_event).await
}

/// Let the popup hold a caret so a follow-up can be typed.
///
/// Summoning Warden never steals focus — that is the point of `focusable: false`,
/// and B4 established that a window which never takes focus never receives key
/// events either. Typing is the one thing that cannot work under that rule, so
/// the rule bends for exactly as long as the box is open.
#[tauri::command]
fn begin_followup(window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(windows)]
    {
        take_focus(&window)?;
    }
    Ok(())
}

/// Close the box and give the caret back to wherever it came from.
#[tauri::command]
fn end_followup(window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(windows)]
    {
        return_focus(&window)?;
    }
    Ok(())
}

/// Ask a second question about the screenshot already taken.
///
/// **Takes no new screenshot** — that is the entire feature. Fails rather than
/// silently capturing if there is nothing stored, because quietly answering about
/// a freshly-grabbed screen would be the wrong answer to the right question.
#[tauri::command]
async fn ask_followup(
    prompt: String,
    model: Option<String>,
    on_event: Channel<StreamEvent>,
    last: tauri::State<'_, LastCapture>,
) -> Result<(), String> {
    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let png = last
        .0
        .lock()
        .map_err(|_| "capture store poisoned")?
        .clone()
        .ok_or("no capture to follow up on — summon Warden first")?;

    ask_with_png(png, prompt, model, on_event).await
}

/// Ask about a PNG that is already in hand.
///
/// Shared by the first question and every follow-up, so the two can never drift
/// apart in how they talk to Ollama — same body, same `num_ctx`, same streaming.
async fn ask_with_png(
    png: Vec<u8>,
    prompt: String,
    model: String,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    // Cold? Say so before the wait, not after. Checked rather than assumed: the
    // model may have been warmed from the tray, from the CLI, or by Ollama's own
    // timeout not having expired yet.
    // warden_core is blocking by design - both its other callers drive it from a
    // thread of their own. Calling it straight from this async command spins up a
    // second tokio runtime inside reqwest and then drops it in an async context,
    // which panics the worker and kills the request: "Cannot drop a runtime in a
    // context where blocking is not allowed". So it goes off-thread, like the tray
    // already does.
    // Off the async worker, like every other warden_core call — it is blocking by
    // design, and dropping its runtime in an async context panics.
    let model_for_check = model.clone();
    let resident = tauri::async_runtime::spawn_blocking(move || {
        warden_core::ollama::ps()
            .map(|m| warden_core::ollama::find(&m, &model_for_check).is_some())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if !resident {
        let _ = on_event.send(StreamEvent::Warming);
    }

    // `images` is top-level; `num_ctx` lives inside `options`. Either wrong fails silently.
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "images": [BASE64_STANDARD.encode(&png)],
        "stream": true,
        "options": { "num_ctx": NUM_CTX },
    });

    let started = Instant::now();
    let response = reqwest::Client::new()
        .post(OLLAMA_URL)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request to Ollama failed — is `ollama serve` running? {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {}", response.status()));
    }

    // Read the body as it arrives. A network chunk can end mid-line, so keep a
    // remainder buffer and only parse complete lines.
    let mut stream = response.bytes_stream();
    let mut remainder = String::new();
    let mut ttft_ms: Option<f64> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        remainder.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline) = remainder.find('\n') {
            let line: String = remainder.drain(..=newline).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let v: serde_json::Value =
                serde_json::from_str(line).map_err(|e| format!("bad JSON from Ollama: {e}"))?;

            let text = v["response"].as_str().unwrap_or("");
            if !text.is_empty() {
                if ttft_ms.is_none() {
                    ttft_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                }
                on_event
                    .send(StreamEvent::Token { text: text.to_string() })
                    .map_err(|e| format!("channel send failed: {e}"))?;
            }

            if v["done"].as_bool().unwrap_or(false) {
                on_event
                    .send(StreamEvent::Done {
                        prompt_eval_count: v["prompt_eval_count"].as_u64().unwrap_or(0) as u32,
                        eval_count: v["eval_count"].as_u64().unwrap_or(0) as u32,
                        total_ms: v["total_duration"].as_f64().unwrap_or(0.0) / 1e6,
                        ttft_ms: ttft_ms.unwrap_or(0.0),
                    })
                    .map_err(|e| format!("channel send failed: {e}"))?;
                return Ok(());
            }
        }
    }

    Ok(())
}

/// Hide the window from screen capture — the project's headline feature, and also
/// what keeps Warden out of its own screenshot.
///
/// Called directly rather than via Tauri's `set_content_protected`, because `tao`'s
/// implementation discards the result (`let _ = SetWindowDisplayAffinity(..)`), so a
/// silent failure would be invisible. This reports it.
///
/// Known upstream issue (tauri#14189): the flag can reset after `hide()`/`show()`,
/// degrading to a black rectangle. Warden is exactly that shape once the hotkey
/// lands at B4, so this will need reapplying — see docs/B3-results.md.
#[cfg(windows)]
fn exclude_from_capture(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };

    // Diagnostic escape hatch. The popup being invisible to screen capture also
    // means it cannot be screenshotted for inspection — so debugging how it
    // *looks* requires temporarily giving that up. `WARDEN_NO_EXCLUDE=1` does.
    if std::env::var("WARDEN_NO_EXCLUDE").is_ok() {
        eprintln!("[warden] WARDEN_NO_EXCLUDE set — capture exclusion SKIPPED");
        return Ok(());
    }

    let hwnd = window.hwnd().map_err(|e| format!("no HWND: {e}"))?;

    // SAFETY: `hwnd` is a valid top-level window owned by this process, handed to
    // us by Tauri, and this runs on the main thread inside `setup()`.
    unsafe {
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
            .map_err(|e| format!("SetWindowDisplayAffinity failed: {e}"))
    }
}

// ── Where the popup sits, and how it gets there ──────────────────────────────
//
// One anchor rule, one writer, one Win32 call per frame.
//
// Three mechanisms used to own this geometry and disagree about it. The old
// `position_bottom_right` hung the window off the **work area**, 16px up;
// `follow_taskbar` hung it off the **taskbar window**, 12px up; and `fit_window`
// called `set_size` and *then* `set_position`. So a single growth step while an
// answer streamed was visibly three: grow downwards over the taskbar, snap back
// up to the work-area anchor, then — up to 120ms later, when the poller next
// looked — twitch the 4px the two anchors differed by.

/// Popup width, logical px. Matches the `width` in `tauri.conf.json`.
const POPUP_W: f64 = 480.0;
/// Height bounds, logical px. Answers run 3,600–7,950 characters (measured at
/// A2), so a fixed height means dead space on short ones or heavy scrolling on
/// long ones.
const MIN_H: f64 = 220.0;
const MAX_H: f64 = 760.0;
/// Gap left above the taskbar's top edge, logical px.
const GAP: f64 = 12.0;
/// Inset from the right edge of the work area, logical px.
const SIDE: f64 = 16.0;
/// How close the pointer has to get, in logical px, before the popup moves out
/// of its way. A little slack either side of the window itself, so a pointer
/// resting on the very edge doesn't sit half-over it.
const DODGE_MARGIN: f64 = 8.0;

/// The geometry the popup is steering toward. `fit_window` writes the height the
/// webview measured; the animator thread reads it at frame rate.
#[derive(Default)]
struct Anim {
    /// Target outer height in **physical** px. Zero means "not measured yet",
    /// which resolves to `MIN_H`.
    target_h: AtomicI32,
    /// Bumped on every show and every hide. An animator whose generation is
    /// stale exits instead of steering: `follow_taskbar` spawned one poller per
    /// show and never stopped the previous one, so a quick hide/show left two
    /// threads setting the position from separate ideas of where it should be.
    generation: AtomicU64,
    /// Which corner the popup is using. The right is preferred, and it only ever
    /// leaves because the pointer arrived — so this is sticky: once pushed to the
    /// left it stays there until the pointer comes to the left too. Reset on
    /// hide, so every summon starts from the preference again.
    on_left: std::sync::atomic::AtomicBool,
}

impl Anim {
    /// Open a new era. Any running animator sees the bump and exits; the
    /// returned generation is the one the next animator runs under.
    fn bump(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Forget the last answer's height, so the popup reopens at its minimum and
    /// grows into the new answer — rather than appearing at the last answer's
    /// full height and immediately collapsing.
    fn forget_height(&self) {
        self.target_h.store(0, Ordering::Relaxed);
    }

    /// Go back to preferring the bottom-right. The stickiness is meant to last as
    /// long as the answer on screen, not for the life of the process.
    fn prefer_right(&self) {
        self.on_left.store(false, Ordering::Relaxed);
    }

    /// Which corner to use now, given where the popup currently sits.
    ///
    /// The pointer is tested against the corner the popup is **actually in** —
    /// not against the right-hand one. That is what makes it stick: once it has
    /// moved left, the bottom-right is empty, so a pointer there means nothing
    /// and it stays put. It only comes back when the pointer follows it left.
    fn corner(&self, occupied_by_pointer: bool) -> bool {
        if occupied_by_pointer {
            let moved = !self.on_left.load(Ordering::Relaxed);
            self.on_left.store(moved, Ordering::Relaxed);
            moved
        } else {
            self.on_left.load(Ordering::Relaxed)
        }
    }
}

/// The work area and the full bounds of the monitor the popup is on, physical px.
///
/// Read straight from Win32 rather than through `window.current_monitor()`:
/// Tauri's window getters round-trip to the event-loop thread, and this runs on
/// every animation frame.
#[cfg(windows)]
fn monitor_rects(hwnd: HWND) -> Option<(RECT, RECT)> {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: a valid HWND owned by this process; `cbSize` is set as the API
    // requires and `info` is only written by the call.
    let ok = unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(monitor, &mut info).as_bool()
    };

    ok.then_some((info.rcWork, info.rcMonitor))
}

/// Where the popup belongs right now: `(x, y, height)` in physical px.
///
/// The single anchor rule — used by the initial placement and by every animation
/// frame, so the two cannot disagree.
///
/// `tray_top` is passed in rather than read here so the animator can cache the
/// taskbar's window handle instead of calling `FindWindowW` sixty times a
/// second. The taskbar *window* is preferred over the work area because an
/// auto-hiding taskbar never changes the work area: it stays full-screen whether
/// the bar is in or out, which is why no fixed margin can track it.
#[cfg(windows)]
/// Both corners the popup can occupy, plus the shared vertical geometry.
struct Corners {
    right_x: i32,
    left_x: i32,
    top: i32,
    height: i32,
    work: RECT,
}

#[cfg(windows)]
fn target_rect(
    hwnd: HWND,
    tray_top: Option<i32>,
    want_h: i32,
    scale: f64,
) -> Option<Corners> {
    let (work, monitor) = monitor_rects(hwnd)?;

    let gap = (GAP * scale).round() as i32;
    let side = (SIDE * scale).round() as i32;
    let width = (POPUP_W * scale).round() as i32;
    let floor = (MIN_H * scale).round() as i32;

    // Only trust the taskbar's edge if the taskbar is on *this* monitor. Only
    // the primary monitor has a `Shell_TrayWnd`; secondary ones carry their own
    // `Shell_SecondaryTrayWnd`, which this deliberately does not read.
    let bottom = match tray_top {
        Some(top) if top > monitor.top && top <= monitor.bottom => top,
        _ => work.bottom,
    } - gap;

    // Shrink rather than slide off the top: a tall answer on a short display
    // should scroll, not sit with its header above the screen.
    let height = want_h.max(floor).min((bottom - work.top).max(floor));
    let top = bottom - height;

    let right = work.right - width - side;

    // Both corners are returned and the caller picks. Which one is in use is
    // sticky state that outlives a single frame, so it cannot be decided here.
    Some(Corners {
        right_x: right,
        left_x: work.left + side,
        top,
        height,
        work,
    })
}

/// Is the pointer inside this rectangle, with a little slack around it?
///
/// Windows draws the mouse cursor above every window — there is no z-order above
/// the cursor layer, and blanking it needs mouse messages that a click-through
/// window never receives. Moving the window is the only way it stops sitting
/// under the pointer.
#[cfg(windows)]
fn pointer_in(x: i32, top: i32, width: i32, height: i32, scale: f64) -> bool {
    let Some((cx, cy)) = cursor_pos() else {
        return false;
    };
    let margin = (DODGE_MARGIN * scale).round() as i32;

    cx >= x - margin && cx <= x + width + margin && cy >= top - margin && cy <= top + height + margin
}

/// The pointer, in physical screen px. `None` if Windows won't say.
///
/// Read per frame from the animator rather than tracked in the mouse hook: it is
/// one cheap call, and it keeps the dodge working even when the hook failed to
/// install.
#[cfg(windows)]
fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    // SAFETY: writes only into `point`, and reports failure rather than trapping.
    unsafe { GetCursorPos(&mut point).ok()? };
    Some((point.x, point.y))
}

/// Move and resize in **one** Win32 call.
///
/// Position and size have to land in the same frame. Done as `set_size` and then
/// `set_position`, the window grew downwards over the taskbar and snapped back
/// up a frame later — most of what read as jitter while an answer streamed.
///
/// Synchronous on purpose: no `SWP_ASYNCWINDOWPOS`. This blocks until the
/// window's own thread has processed the resize, which paces the animation to
/// what WebView2 can actually relayout instead of queueing frames it will throw
/// away. Nothing is lost by being paced, because the ease is computed from
/// measured elapsed time — a slower frame simply moves further.
#[cfg(windows)]
fn set_bounds(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
    };

    // SAFETY: a valid top-level HWND owned by this process.
    //
    // `SWP_NOACTIVATE` matters as much here as in `show_without_focus` —
    // activating the popup would pull the caret out of whatever you were typing
    // in. `SWP_NOZORDER` preserves `alwaysOnTop`, which is set at creation.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
        );
    }
}

/// Put the popup where it belongs immediately, with no animation.
///
/// Used before the window is shown: it should appear already in place, not glide
/// in from wherever the last answer left it.
#[cfg(windows)]
fn place_now(window: &tauri::WebviewWindow, anim: &Anim) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| format!("no HWND: {e}"))?;
    let scale = window
        .scale_factor()
        .map_err(|e| format!("no scale factor: {e}"))?;

    let c = target_rect(hwnd, taskbar_top(), anim.target_h.load(Ordering::Relaxed), scale)
        .ok_or_else(|| "no monitor info".to_string())?;

    let width = (POPUP_W * scale).round() as i32;
    // The preference applies at birth too: if the pointer is already parked in
    // the bottom-right, the popup is born on the left rather than appearing
    // under the arrow and then sliding out from under it.
    let here = if anim.on_left.load(Ordering::Relaxed) {
        c.left_x
    } else {
        c.right_x
    };
    let on_left = anim.corner(pointer_in(here, c.top, width, c.height, scale));

    let x = if on_left { c.left_x } else { c.right_x };
    set_bounds(hwnd, x, c.top, width, c.height);
    Ok(())
}

/// Show the popup **without taking focus**, so the cursor stays in whatever you
/// were typing in.
///
/// Tauri has no API for this — `show()` + `set_focus()` activates the window, and
/// `show()` alone still activates on Windows. tauri#11814 tracks the gap and is
/// open with no fix. `SW_SHOWNOACTIVATE` is the Win32 primitive that does it, and
/// we already link the `windows` crate.
#[cfg(windows)]
fn show_without_focus(
    window: &tauri::WebviewWindow,
    anim: &Arc<Anim>,
    generation: u64,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};

    let hwnd = window.hwnd().map_err(|e| format!("no HWND: {e}"))?;
    // SAFETY: a valid top-level HWND owned by this process.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }

    // Deliberately NOT calling window.set_focus().

    // tauri#14189: the capture-exclusion flag can revert after a hide/show cycle,
    // silently degrading to a black rectangle in screen shares. Reapply on every
    // show. One reporter says reapplying didn't fix it for them, so this is
    // verified at the end of the chunk rather than assumed.
    exclude_from_capture(window)?;

    // Click-through: the mouse reaches whatever is behind the popup instead of
    // stopping on it. `WS_EX_TRANSPARENT` takes the window out of hit-testing
    // entirely, so the pointer, clicks and the wheel all pass straight through.
    //
    // **On by default as of this change.** B4 measured it as working and left it
    // off for one reason: it costs the mouse wheel, "and answers are long". That
    // reason is now gone — Ctrl+Alt+Q and Ctrl+Alt+A scroll the answer without
    // the mouse, so losing the wheel no longer costs the ability to read a long
    // answer. `WARDEN_NO_CLICKTHROUGH` puts it back.
    //
    // This is also the ONE Tauri feature that sets WS_EX_LAYERED, and
    // SetWindowDisplayAffinity is reported to fail on layered windows. B4
    // measured that it doesn't — merely *carrying* the layered bit is not the
    // same as painting with `UpdateLayeredWindow` — but the affinity is read
    // back from Windows below on every show rather than assumed.
    if std::env::var("WARDEN_NO_CLICKTHROUGH").is_err() {
        window
            .set_ignore_cursor_events(true)
            .map_err(|e| format!("click-through failed: {e}"))?;
        println!("[warden] click-through on — the mouse passes through (WS_EX_LAYERED set)");
    } else {
        // Explicit, so the log always says which mode the popup is in.
        let _ = window.set_ignore_cursor_events(false);
        println!("[warden] click-through OFF (WARDEN_NO_CLICKTHROUGH) — the mouse lands on the popup");
    }

    // Reapply the backdrop after showing. The window is created with
    // `visible: false`, and effects applied at build time to a window that was
    // never shown do not reliably stick.
    // Must run BEFORE set_effects. On Windows 11, `apply_acrylic` only calls
    // DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW)
    // and — unlike apply_mica/apply_tabbed — never touches
    // DWMWA_USE_IMMERSIVE_DARK_MODE. So acrylic cannot request dark on its own.
    if let Err(e) = force_dark_mode(window) {
        eprintln!("[warden] WARNING: dark mode attribute failed: {e}");
    }

    // No DWM system backdrop. Four attempts established that the backdrop path
    // cannot do what this popup needs on Windows 11:
    //
    //   Acrylic  — samples what is behind the window (right behaviour), but
    //              `apply_acrylic` has no `dark` parameter and never sets
    //              DWMWA_USE_IMMERSIVE_DARK_MODE, so it only renders light.
    //   MicaDark — genuinely dark, but Mica samples the desktop *wallpaper*, never
    //              the window behind, so it is never see-through over an app.
    //
    // Translucency now comes from the webview itself: `transparent: true` makes
    // WebView2 composite with alpha, and the CSS background supplies it. No blur,
    // but real see-through, fully under our control, and immune to DWM's
    // active/inactive rendering — which a never-focused window always loses.

    // Assert visibility one more time, *after* every style change above.
    //
    // `set_ignore_cursor_events` drops WS_VISIBLE. It re-applies the window state
    // Tauri believes in, and Tauri believes the window is hidden — we showed it
    // with raw `ShowWindow` to avoid stealing focus, which bypasses Tauri's
    // dispatcher and leaves its bookkeeping stale (the same disagreement
    // `is_really_visible` exists to work around).
    //
    // Measured: `visible=true` after ShowWindow, `visible=false` immediately
    // after enabling click-through. The window does come back on its own a moment
    // later, so the popup still appeared and the bug looked like nothing at all —
    // but every thread started here samples visibility straight away and exited,
    // taking the geometry animation and the wheel hook with it.
    // SAFETY: a valid top-level HWND owned by this process.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }

    report_affinity(window);
    animate(window.clone(), anim.clone(), generation);
    watch_wheel(
        window.clone(),
        window.app_handle().clone(),
        anim.clone(),
        generation,
    );
    Ok(())
}

/// The taskbar's current top edge, in physical pixels.
///
/// An auto-hiding taskbar does **not** change the work area when it slides in and
/// out — the work area stays full-screen either way, which is why a static margin
/// can't track it. The taskbar window itself does move, so read that instead.
#[cfg(windows)]
fn taskbar_top() -> Option<i32> {
    find_tray().and_then(window_top)
}

/// The shell's taskbar window, if it has one on the primary monitor.
///
/// Split out from `taskbar_top` because `FindWindowW` walks every top-level
/// window in the session, which is not a thing to do at frame rate — the
/// animator finds it once and re-reads its rect instead.
#[cfg(windows)]
fn find_tray() -> Option<HWND> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

    // SAFETY: returns Err when there is no such window, which `.ok()` makes None.
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None).ok() }
}

/// The top edge of a window in physical px, or `None` if the handle is dead.
#[cfg(windows)]
fn window_top(hwnd: HWND) -> Option<i32> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut rect = RECT::default();
    // SAFETY: GetWindowRect only reads, and fails cleanly on a stale handle.
    unsafe { GetWindowRect(hwnd, &mut rect).ok()? };
    Some(rect.top)
}

/// Glide the popup toward wherever it currently belongs, for as long as it is on
/// screen.
///
/// Two things move it: the answer growing (`fit_window` writes a new target
/// height) and the taskbar sliding (an auto-hiding bar takes roughly 200ms to
/// come in). Both used to be steps. Easing them costs one `SetWindowPos` per
/// frame and turns both into motion the eye can follow.
///
/// Polls rather than listening: tracking the shell's appbar notifications means
/// registering as an appbar ourselves, which is a lot of machinery for a window
/// that's visible for a few seconds. It runs only while the popup is up, and
/// once settled it issues no Win32 call at all, so an idle popup does no work.
///
/// A plain OS thread, not a `tauri::async_runtime` task: every call in here is a
/// blocking Win32 call, and `SetWindowPos` on a window owned by another thread
/// blocks until that thread pumps it. Parking a tokio worker on that for the
/// life of the popup is exactly what the runtime is not for.
#[cfg(windows)]
fn animate(window: tauri::WebviewWindow, anim: Arc<Anim>, generation: u64) {
    /// Time constant of the ease, in seconds: how long to close ~63% of the
    /// remaining distance. At 45ms a 300px jump is visually done in ~150ms —
    /// fast enough not to read as lag, slow enough to read as motion.
    const TAU: f64 = 0.045;
    /// Horizontal motion gets its own, slower constant. Growing and following the
    /// taskbar want to keep up with what caused them; the dodge is the only move
    /// the eye actually follows across the screen, and at the vertical speed it
    /// reads as a flinch rather than as the window getting out of the way.
    const TAU_X: f64 = 0.13;
    /// Requested frame interval. Windows' default timer granularity is ~15ms, so
    /// this usually lands near 60Hz — and higher when WebView2 raises the
    /// resolution to animate. Nothing depends on hitting it.
    const FRAME: Duration = Duration::from_millis(8);
    /// Distance at which the motion is over. Half a pixel cannot be drawn, and
    /// stopping cleanly is what makes a settled popup free.
    const DONE: f64 = 0.5;

    std::thread::spawn(move || {
        let Ok(hwnd) = window.hwnd() else { return };
        // Read once per show. A monitor change mid-answer would want this again,
        // but the popup lives for seconds and appears on one monitor.
        let Ok(scale) = window.scale_factor() else { return };
        let width = (POPUP_W * scale).round() as i32;

        let mut tray = find_tray();
        let want = || anim.target_h.load(Ordering::Relaxed);

        // Seeded *at* the target, so the first frame is a snap rather than a
        // glide: `place_now` has already put the window here, and re-animating
        // to the same place would only add a flinch on every show.
        let Some(c0) = target_rect(hwnd, taskbar_top(), want(), scale) else {
            return;
        };
        let left0 = anim.on_left.load(Ordering::Relaxed);
        let x0 = if left0 { c0.left_x } else { c0.right_x };
        let (mut x, mut y, mut h) = (x0 as f64, c0.top as f64, c0.height as f64);
        let mut sent: Option<(i32, i32, i32)> = None;
        let mut prev = Instant::now();

        // How the horizontal position is being driven.
        #[derive(Clone, Copy)]
        enum Swap {
            /// Following the anchor directly — a taskbar move, a work-area change.
            Settled,
            /// On the way out through the near edge.
            Leaving { since: Instant, from: f64 },
            /// On the way in from the far edge.
            Arriving { since: Instant, from: f64 },
        }

        /// Out through the near edge, then in from the far one. The exit is short
        /// — nothing is being read once the window has left — and the entry is
        /// where the time goes, because that is the part actually watched.
        const OUT: Duration = Duration::from_millis(150);
        const IN: Duration = Duration::from_millis(340);

        // Which corner the window is heading for, and where it is in a swap.
        let mut on_left = left0;
        let mut swap = Swap::Settled;

        loop {
            // A newer show, or a hide, has taken over. Stop touching the window.
            if anim.generation.load(Ordering::Relaxed) != generation {
                return;
            }
            if !is_really_visible(&window) {
                return;
            }

            let mut tray_top = tray.and_then(window_top);
            if tray_top.is_none() {
                // The taskbar window is replaced when Explorer restarts.
                tray = find_tray();
                tray_top = tray.and_then(window_top);
            }

            let Some(c) = target_rect(hwnd, tray_top, want(), scale) else {
                return;
            };
            let work = c.work;

            // Only the settled window is a target: mid-swap it is off screen, and
            // testing it there would retrigger against a rectangle nobody can see.
            let here = if on_left { c.left_x } else { c.right_x };
            let pushed = matches!(swap, Swap::Settled)
                && pointer_in(here, c.top, width, c.height, scale);

            let want_left = anim.corner(pushed);
            let (tx, ty, th) = (
                if want_left { c.left_x } else { c.right_x },
                c.top,
                c.height,
            );

            let now = Instant::now();
            // Frame-rate independent: `k` is the fraction of the remaining
            // distance to close over however long this frame *actually* took, so
            // a late frame moves proportionally further instead of stuttering.
            let dt = (now - prev).as_secs_f64();
            let k = 1.0 - (-dt / TAU).exp();
            let kx = 1.0 - (-dt / TAU_X).exp();
            prev = now;

            for (current, target) in [(&mut y, ty as f64), (&mut h, th as f64)] {
                *current += (target - *current) * k;
                if (target - *current).abs() < DONE {
                    *current = target;
                }
            }

            // Swapping corners goes *around* rather than across: out through the
            // near edge, then in from the far one. Sliding straight across would
            // drag the window through the middle of the screen — over whatever
            // you were reading — which is the thing the dodge exists to avoid.
            //
            // Timed, not the exponential chase the rest of this uses. An
            // exponential is right for following something that is still moving;
            // this is a move between two fixed points, and measured as an
            // exponential it spent its last 580ms crawling the final 27 pixels.
            if want_left != on_left {
                if !matches!(swap, Swap::Leaving { .. }) {
                    swap = Swap::Leaving { since: now, from: x };
                }
            } else if matches!(swap, Swap::Leaving { .. }) {
                // The pointer moved away again mid-flight. Come back, rather than
                // completing a lap to the corner already occupied.
                swap = Swap::Settled;
            }

            // Just past the near edge, and just past the far one: the window is
            // fully off screen at both, so the jump between them is invisible.
            let past_left = (work.left - width) as f64;
            let past_right = work.right as f64;

            match swap {
                Swap::Settled => {
                    x += (tx as f64 - x) * kx;
                    if (tx as f64 - x).abs() < DONE {
                        x = tx as f64;
                    }
                }
                Swap::Leaving { since, from } => {
                    let edge = if on_left { past_left } else { past_right };
                    let p = ((now - since).as_secs_f64() / OUT.as_secs_f64()).clamp(0.0, 1.0);
                    // Accelerating away, so leaving reads as a departure.
                    x = from + (edge - from) * (p * p);
                    if p >= 1.0 {
                        on_left = want_left;
                        let entry = if on_left { past_left } else { past_right };
                        x = entry;
                        swap = Swap::Arriving { since: now, from: entry };
                    }
                }
                Swap::Arriving { since, from } => {
                    let p = ((now - since).as_secs_f64() / IN.as_secs_f64()).clamp(0.0, 1.0);
                    // Decelerating in, and actually finishing rather than
                    // asymptotically approaching.
                    let eased = 1.0 - (1.0 - p).powi(3);
                    x = from + (tx as f64 - from) * eased;
                    if p >= 1.0 {
                        x = tx as f64;
                        swap = Swap::Settled;
                    }
                }
            }

            let frame = (x.round() as i32, y.round() as i32, h.round() as i32);
            if sent != Some(frame) {
                set_bounds(hwnd, frame.0, frame.1, width, frame.2);
                sent = Some(frame);
            }

            std::thread::sleep(FRAME);
        }
    });
}

/// Ask DWM to render this window's chrome and system backdrop in dark mode.
///
/// `apply_acrylic` never sets this — only `apply_mica`/`apply_tabbed` do — so an
/// acrylic window has no way to request dark through Tauri's API at all.
#[cfg(windows)]
fn force_dark_mode(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};

    let hwnd = window.hwnd().map_err(|e| format!("no HWND: {e}"))?;
    let dark: u32 = 1;
    // SAFETY: valid top-level HWND owned by this process; the attribute takes a
    // 4-byte BOOL.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&dark) as u32,
        )
        .map_err(|e| format!("{e}"))
    }
}

/// Hide via the same Win32 path we show with, and report what Windows says
/// afterwards rather than trusting the call.
#[cfg(windows)]
fn hide_now(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    let Ok(hwnd) = window.hwnd() else {
        eprintln!("[warden] hide: no HWND");
        return;
    };
    // SAFETY: valid HWND owned by this process.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    println!("[warden] hidden (now visible={})", is_really_visible(window));
}

/// Ask **Windows** whether the popup is on screen, not Tauri.
///
/// We show it with raw `ShowWindow(SW_SHOWNOACTIVATE)` to avoid stealing focus,
/// which bypasses Tauri's dispatcher — so Tauri's own `is_visible()` never learns
/// the window came up and keeps reporting `false`. That made the toggle a one-way
/// trip: every press took the "show" branch and nothing ever hid it.
#[cfg(windows)]
fn is_really_visible(window: &tauri::WebviewWindow) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    let Ok(hwnd) = window.hwnd() else { return false };
    // SAFETY: valid HWND owned by this process.
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// Where the caret was before the follow-up box took it.
///
/// Remembered so it can be handed back. Summoning Warden deliberately never
/// steals focus (that is why the window is `focusable: false`), and typing a
/// follow-up is the one moment that rule has to bend — so it bends for exactly
/// as long as the box is open, and puts things back afterwards.
static PREVIOUS_FOREGROUND: std::sync::Mutex<Option<isize>> = std::sync::Mutex::new(None);

/// Let the popup receive keystrokes, remembering who had focus.
///
/// `WS_EX_NOACTIVATE` is what keeps the window from ever taking focus. It has to
/// come off before the window can hold a caret, and go back on the moment the box
/// closes — leaving it off would mean the next summon steals focus from whatever
/// you were typing in, which is the behaviour the whole design avoids.
///
/// `SetForegroundWindow` refuses when the calling process does not already own
/// the foreground, so this attaches to the foreground thread's input queue first.
/// Without that it silently does nothing and the box looks broken.
#[cfg(windows)]
fn take_focus(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, SetForegroundWindow,
        SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    };

    let hwnd = window.hwnd().map_err(|e| format!("no hwnd: {e}"))?;

    unsafe {
        let previous = GetForegroundWindow();
        *PREVIOUS_FOREGROUND.lock().map_err(|_| "focus store poisoned")? =
            (!previous.is_invalid()).then(|| previous.0 as isize);

        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !(WS_EX_NOACTIVATE.0 as isize));

        // Attach to whoever owns the foreground so the call is permitted.
        let ours = GetCurrentThreadId();
        let theirs = GetWindowThreadProcessId(previous, None);
        let attached = theirs != 0 && theirs != ours && AttachThreadInput(theirs, ours, true).as_bool();

        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));

        if attached {
            let _ = AttachThreadInput(theirs, ours, false);
        }
    }
    Ok(())
}

/// Put `WS_EX_NOACTIVATE` back and return the caret to where it was.
#[cfg(windows)]
fn return_focus(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    };

    let hwnd = window.hwnd().map_err(|e| format!("no hwnd: {e}"))?;

    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | (WS_EX_NOACTIVATE.0 as isize));

        if let Some(previous) = PREVIOUS_FOREGROUND.lock().map_err(|_| "focus store poisoned")?.take() {
            let _ = SetForegroundWindow(HWND(previous as *mut _));
        }
    }
    Ok(())
}

/// Read the display affinity back from Windows. `17` is `WDA_EXCLUDEFROMCAPTURE`;
/// anything else means the exclusion is not actually in force, whatever we asked for.
#[cfg(windows)]
fn report_affinity(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowDisplayAffinity;

    let Ok(hwnd) = window.hwnd() else { return };
    let mut affinity: u32 = 0;
    // SAFETY: valid HWND owned by this process.
    let ok = unsafe { GetWindowDisplayAffinity(hwnd, &mut affinity) };

    match (ok.is_ok(), affinity) {
        (true, 17) => println!("[warden] affinity verified: 17 (excluded) — click-through OK"),
        (true, other) => eprintln!(
            "[warden] WARNING: affinity is {other}, not 17 — the window is NOT excluded from capture"
        ),
        (false, _) => eprintln!("[warden] WARNING: could not read display affinity"),
    }
}

// ── Reading an answer without the mouse ──────────────────────────────────────
//
// These are what make click-through affordable. With the popup out of
// hit-testing there is no wheel, no text selection and no pointer — the overlay
// is something you look at, not something you touch — so the keyboard has to be
// able to do the one thing reading a long answer requires.
//
// The order mattered: these were built first, deliberately, because turning on
// click-through before there was another way to scroll would have made long
// answers unreadable.

fn chord() -> Option<Modifiers> {
    Some(Modifiers::CONTROL | Modifiers::ALT)
}

/// Pick a block of the answer, then copy it.
///
/// The same key does both halves: the first press enters pick mode, the second
/// copies what is picked. While picking, the scroll keys move the pick instead
/// of scrolling — they already mean up and down, so there is nothing new to
/// learn and no third pair of keys to claim.
///
/// The lesson that shaped this: **a global hotkey outranks anything an
/// application binds internally.** Claiming `Ctrl+Alt+<letter>` takes that chord
/// away from every app for as long as the popup is up, and nothing detects it
/// beforehand — `RegisterHotKey` reports all of these as free, because it only
/// knows about other *global* registrations.
///
/// `Ctrl+Alt+C` went that way first. `Ctrl+Alt+Q`/`A` followed: A is the one the
/// hand reaches for when it means select-all. Scrolling moved to Ctrl+Alt+wheel
/// instead, which claims no key at all. One letter remains, and it is the only
/// one, so it is worth knowing it carries the same risk.
fn key_copy() -> Shortcut {
    Shortcut::new(chord(), Code::KeyZ)
}

/// Ask a second question about the screenshot already taken.
///
/// This is the second letter claimed while the popup is up, and it carries
/// exactly the risk `key_copy` describes — for as long as an answer is on
/// screen, no other application can see `Ctrl+Alt+F`. It is released on dismiss,
/// like the rest.
///
/// A letter and not the wheel, because this one has to *start* something rather
/// than nudge it, and there is no wheel gesture that means "ask".
fn key_followup() -> Shortcut {
    Shortcut::new(chord(), Code::KeyF)
}

/// Every key that only means something while an answer is on screen, with the
/// label to print if it can't be claimed.
fn popup_keys() -> [(Shortcut, &'static str); 2] {
    [(key_copy(), "Ctrl+Alt+Z"), (key_followup(), "Ctrl+Alt+F")]
}

/// Claim the scroll keys, and say whether both were actually claimed.
///
/// Bound on show and dropped on hide rather than held for the life of the
/// process: Warden sits in the background all day, and a popup with nothing on
/// screen has no business owning Ctrl+Alt+Q system-wide.
///
/// A failure is survivable and is reported rather than fatal — the wheel still
/// scrolls, so the answer is still readable. The return value is what the UI
/// uses to decide whether to *offer* the keys, so it never advertises a shortcut
/// that isn't listening.
fn bind_popup_keys(app: &tauri::AppHandle) -> bool {
    let mut all = true;

    for (key, label) in popup_keys() {
        if app.global_shortcut().is_registered(key) {
            continue;
        }
        if let Err(e) = app.global_shortcut().register(key) {
            eprintln!("[warden] WARNING: {label} unavailable ({e})");
            all = false;
        }
    }
    all
}

/// Give the popup's keys back to the rest of the system.
fn release_popup_keys(app: &tauri::AppHandle) {
    for (key, _) in popup_keys() {
        if app.global_shortcut().is_registered(key) {
            let _ = app.global_shortcut().unregister(key);
        }
    }
}

// ── Ctrl+Alt+wheel ───────────────────────────────────────────────────────────
//
// The popup is click-through, so it receives no mouse input at all — not even
// the wheel. The only way to hear a wheel aimed at it is to watch the whole
// session's mouse input and pick out the events carrying Ctrl+Alt.
//
// Requiring the modifiers is what keeps click-through intact: a bare wheel still
// passes straight through to whatever is behind the popup, exactly as before.
// Only Ctrl+Alt+wheel is taken, and only while an answer is on screen.
//
// This also claims no *key*, which is the point. Every key tried for scrolling
// had to be taken from the applications underneath — see `key_copy`.

/// Wheel delta seen by the hook and not yet handed to the webview. 120 per
/// notch, in the sign Windows reports: positive is a forward roll.
///
/// The hook does nothing but add to this. It runs in front of every mouse event
/// in the session, and Windows silently unhooks a low-level hook whose callback
/// overruns `LowLevelHooksTimeout` (300ms by default) — so the callback stays a
/// handful of instructions and the thread that owns it does the talking.
static WHEEL_DELTA: AtomicI32 = AtomicI32::new(0);

/// Whether the hook should be taking Ctrl+Alt+wheel at all. Read on every mouse
/// event, so it is the cheapest possible gate.
static WHEEL_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The installed hook as a raw handle, so the main thread can take it back down
/// later. Zero when nothing is hooked.
static WHEEL_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[cfg(windows)]
unsafe extern "system" fn wheel_hook(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, MSLLHOOKSTRUCT, WM_MOUSEWHEEL,
    };

    if code == HC_ACTION as i32
        && wparam.0 as u32 == WM_MOUSEWHEEL
        && WHEEL_ACTIVE.load(Ordering::Relaxed)
    {
        // SAFETY: reads only. The high bit of GetAsyncKeyState is "currently down".
        let held = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) < 0 };

        if held(VK_CONTROL.0) && held(VK_MENU.0) {
            // SAFETY: for HC_ACTION the system guarantees lparam points at a
            // MSLLHOOKSTRUCT valid for the duration of this call.
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            // The signed wheel delta is the high word of mouseData.
            let delta = (info.mouseData >> 16) as i16;
            WHEEL_DELTA.fetch_add(delta as i32, Ordering::Relaxed);

            // Swallow it, so the window underneath does not scroll as well.
            return LRESULT(1);
        }
    }

    // SAFETY: passing the event on unchanged is the documented default path.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Take Ctrl+Alt+wheel for as long as the popup is on screen.
///
/// Its own thread, and one that never blocks: a low-level hook is only called
/// while its installing thread is retrieving messages, so anything that parked
/// this loop would stall the callback — and a stalled callback is how Windows
/// decides to remove the hook. That is also why this cannot live on the animator
/// thread, which blocks on `SetWindowPos` by design.
#[cfg(windows)]
fn watch_wheel(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    anim: Arc<Anim>,
    generation: u64,
) {
    // Install on Tauri's **main** thread, not on one of ours.
    //
    // A low-level hook is called on the thread that installed it, and only while
    // that thread is inside a message-retrieval call. Two hand-rolled loops were
    // measured failing at exactly that: a `PeekMessageW` poll never delivered the
    // callback once, and a `GetMessageW` loop woken by a thread timer took one
    // WM_TIMER and then blocked forever. Tauri's event loop already pumps
    // correctly and continuously, so the hook goes there and the callback — an
    // atomic add and two key-state reads — rides along.
    let installed = app.run_on_main_thread(|| {
        use windows::Win32::Foundation::HINSTANCE;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_MOUSE_LL};

        // The module handle matters, even though the documentation says a
        // low-level hook may pass NULL because its procedure is not injected into
        // other processes. Measured: with NULL the call **succeeds and returns a
        // valid handle**, and the callback is then never invoked — not once, for
        // any mouse event at all. A standalone control hook passing
        // `GetModuleHandle(NULL)` saw 34 wheel events over the same three seconds.
        // So it fails silently in the worst way: nothing to check, nothing logged.
        let module = unsafe { GetModuleHandleW(None) }
            .ok()
            .map(|m| HINSTANCE(m.0));

        // SAFETY: WH_MOUSE_LL is one of the two hooks that needs no DLL — it is
        // called on this thread rather than injected into other processes.
        match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(wheel_hook), module, 0) } {
            Ok(hook) => {
                WHEEL_DELTA.store(0, Ordering::Relaxed);
                WHEEL_HOOK.store(hook.0 as isize, Ordering::Relaxed);
                WHEEL_ACTIVE.store(true, Ordering::Relaxed);
            }
            Err(e) => eprintln!(
                "[warden] WARNING: Ctrl+Alt+wheel unavailable ({e}) — Ctrl+Alt+Z still copies"
            ),
        }
    });

    if let Err(e) = installed {
        eprintln!("[warden] WARNING: could not reach the main thread to hook the wheel ({e})");
        return;
    }

    // The hook only counts. This hands what it counted to the webview, and takes
    // the hook back down when the popup goes away. It needs no message loop of
    // its own precisely because the hook does not live here.
    std::thread::spawn(move || {
        while anim.generation.load(Ordering::Relaxed) == generation && is_really_visible(&window) {
            // Whole notches go to the webview; the remainder stays banked, so a
            // high-resolution wheel reporting in fractions of a notch is not
            // quietly rounded away to nothing.
            let delta = WHEEL_DELTA.swap(0, Ordering::Relaxed);
            let notches = delta / 120;
            if notches == 0 {
                WHEEL_DELTA.fetch_add(delta, Ordering::Relaxed);
            } else {
                WHEEL_DELTA.fetch_add(delta % 120, Ordering::Relaxed);
                // Negated: Windows reports a forward roll as positive, and rolling
                // forward should move the answer *up*, which is a negative step.
                let _ = app.emit("warden://scroll", -notches);
            }

            std::thread::sleep(Duration::from_millis(8));
        }

        WHEEL_ACTIVE.store(false, Ordering::Relaxed);

        // Removed from the same thread that installed it, as Windows requires.
        let _ = app.run_on_main_thread(|| {
            use windows::Win32::UI::WindowsAndMessaging::{UnhookWindowsHookEx, HHOOK};

            let raw = WHEEL_HOOK.swap(0, Ordering::Relaxed);
            if raw != 0 {
                // SAFETY: our own handle, taken exactly once by the swap above.
                unsafe {
                    let _ = UnhookWindowsHookEx(HHOOK(raw as *mut std::ffi::c_void));
                }
            }
        });
    });
}

/// Put one block of the answer on the clipboard.
///
/// Written from Rust rather than with `navigator.clipboard` in the webview. The
/// popup is `focusable: false` and is never activated, and the web clipboard API
/// wants a focused, user-activated document — so from the page it would fail
/// exactly when it is needed. The clipboard has no such requirement natively.
#[tauri::command]
fn copy_text(app: tauri::AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    app.clipboard()
        .write_text(text)
        .map_err(|e| format!("clipboard write failed: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Send stdout and stderr to a file, since there is no console to print to.
///
/// `main.rs` suppresses the console so the app survives the terminal that
/// launched it. That would otherwise throw away every diagnostic - and those
/// diagnostics are what found the panic that hung the popup, and proved the tray
/// menu was firing when it looked dead. So the handles get pointed at a file
/// instead of a window.
///
/// Done at the Win32 level rather than by changing every `println!`: setting the
/// process standard handles catches everything, including anything a dependency
/// prints.
#[cfg(windows)]
fn redirect_log_to_file() -> Option<std::path::PathBuf> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    let path = std::env::temp_dir().join("warden.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    let handle = HANDLE(file.as_raw_handle());
    // SAFETY: a valid handle to a file this process just opened.
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    // Deliberately leaked: the handles outlive this scope and are used for the
    // life of the process.
    std::mem::forget(file);
    Some(path)
}

pub fn run() {
    #[cfg(windows)]
    if let Some(log) = redirect_log_to_file() {
        println!("[warden] log: {}", log.display());
    }

    let primary = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyW);
    let fallback = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    // Fires on BOTH press and release — without this guard every
                    // action would run twice per keypress.
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }

                    // The copy key first: it is live only while an answer is on
                    // screen, so reaching here means there is something to copy.
                    // It is a plain message to the frontend, which owns what it
                    // means — enter the pick, or take what is picked.
                    //
                    // Scrolling is not here any more. It arrives as Ctrl+Alt+wheel
                    // through `watch_wheel`, which claims no key from anyone.
                    if *shortcut == key_copy() {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = app.emit("warden://copy", 0);
                        });
                        return;
                    }

                    // Same shape for the follow-up box. The frontend opens it and
                    // calls back to `begin_followup` for the caret, rather than
                    // taking focus from here — this handler holds the message pump,
                    // and `SetForegroundWindow` on it is the freeze described below.
                    if *shortcut == key_followup() {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = app.emit("warden://followup", 0);
                        });
                        return;
                    }

                    // The handler runs on the main thread, synchronously, holding
                    // the Win32 message pump. Touching windows inline here is a
                    // confirmed Windows 11 freeze (plugins-workspace#1923), so
                    // hand off immediately.
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let Some(window) = app.get_webview_window("main") else {
                            return;
                        };

                        // Toggle. Esc can't dismiss a non-focusable window — it
                        // never receives key events — so the hotkey does both jobs
                        // until C3 sorts out the focus transition.
                        #[cfg(windows)]
                        let visible = is_really_visible(&window);
                        #[cfg(not(windows))]
                        let visible = window.is_visible().unwrap_or(false);

                        let anim = app.state::<Arc<Anim>>().inner().clone();

                        println!("[warden] hotkey: visible={visible}");
                        if visible {
                            // Bump before hiding, so the animator stops on its
                            // next frame rather than steering a window that is
                            // on its way out.
                            anim.bump();
                            anim.forget_height();
                            anim.prefer_right();
                            release_popup_keys(&app);

                            // Hide the same way we show: raw Win32. Showing via
                            // ShowWindow and hiding via Tauri's dispatcher leaves
                            // Windows and Tauri disagreeing about the window, and
                            // the next IsWindowVisible still reported `true`.
                            #[cfg(windows)]
                            hide_now(&window);
                            #[cfg(not(windows))]
                            let _ = window.hide();
                            return;
                        }

                        // One generation per show: a fast hide/show cannot leave
                        // the previous animator running alongside the new one.
                        let generation = anim.bump();

                        #[cfg(windows)]
                        {
                            // Place first, then show — the popup appears already
                            // in position, and only animates from there.
                            if let Err(e) = place_now(&window, &anim) {
                                eprintln!("[warden] WARNING: positioning failed: {e}");
                            }
                            if let Err(e) = show_without_focus(&window, &anim, generation) {
                                eprintln!("[warden] WARNING: show failed: {e}");
                            }
                        }

                        // Claim the scroll keys for as long as this answer is up.
                        let keys_bound = bind_popup_keys(&app);

                        // The frontend owns the Channel, so it starts the capture.
                        // The payload says whether the scroll keys are listening,
                        // so the UI can offer them without ever advertising a
                        // shortcut that failed to bind.
                        let _ = app.emit("warden://summoned", keys_bound);
                    });
                })
                .build(),
        )
        .setup(move |app| {
            // The one place the popup's geometry is written from. Managed before
            // anything can ask for it: `fit_window` resolves it as state, and the
            // hotkey handler reads it on every press.
            let anim = Arc::new(Anim::default());
            app.manage(anim.clone());
            app.manage(LastCapture::default());

            #[cfg(windows)]
            {
                let window = app
                    .get_webview_window("main")
                    .expect("no window labeled \"main\" — check tauri.conf.json");
                match exclude_from_capture(&window) {
                    Ok(()) => println!("[warden] window excluded from screen capture"),
                    // Loudly, not silently: this is the headline feature.
                    Err(e) => eprintln!("[warden] WARNING: capture exclusion failed: {e}"),
                }
                if let Err(e) = place_now(&window, &anim) {
                    eprintln!("[warden] WARNING: positioning failed: {e}");
                }
            }

            // Registered by hand rather than through the plugin builder's
            // `.with_shortcut()`: a failed bind there propagates out of plugin
            // setup and aborts the whole app, instead of falling back.
            let shortcut = match app.global_shortcut().register(primary) {
                Ok(()) => {
                    println!("[warden] hotkey registered: Ctrl+Alt+W");
                    Some("Ctrl+Alt+W".to_string())
                }
                Err(e) => {
                    eprintln!("[warden] Ctrl+Alt+W unavailable ({e}) — trying fallback");
                    match app.global_shortcut().register(fallback) {
                        Ok(()) => {
                            println!("[warden] hotkey registered: Ctrl+Alt+Space");
                            Some("Ctrl+Alt+Space".to_string())
                        }
                        Err(e2) => {
                            eprintln!("[warden] ERROR: no hotkey could be registered: {e2}");
                            None
                        }
                    }
                }
            };

            // Note: a successful bind is not proof the hotkey works. Apps that grab
            // keys with a low-level keyboard hook swallow it with no error at all
            // (plugins-workspace#2646). Only a real keypress proves it.
            app.manage(HotkeyLabel(shortcut));

            // Power control. The CLI proved the mechanism at C4a; this is the
            // half you can reach without a terminal, and the only place Warming
            // can be shown — see tray.rs.
            let power = std::sync::Arc::new(tray::Power::new(
                DEFAULT_MODEL.to_string(),
                NUM_CTX,
            ));
            app.manage(power.clone());
            if let Err(e) = tray::install(app.handle(), power) {
                eprintln!("[warden] WARNING: tray unavailable: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ask_about_screen,
            ask_followup,
            begin_followup,
            end_followup,
            hotkey_label,
            fit_window,
            copy_text
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Record how tall the popup wants to be. `animate` does the moving.
///
/// Nothing is resized here, and there is no deadband. The animator is already
/// the one writer, it already skips a frame whose rectangle is unchanged, and it
/// already eases — so a target that shifts by two pixels costs two pixels of
/// glide rather than a resize the compositor has to service.
///
/// `content_height` is in **CSS** pixels, because that is the only unit the
/// webview can measure in. The bounds are therefore applied in logical px and
/// the result scaled once. Treating it as device px — which this did — left the
/// window a third too short and 320 logical px wide on a 150% display, sizing
/// itself to text it could then never fit.
#[tauri::command]
fn fit_window(
    window: tauri::WebviewWindow,
    anim: tauri::State<'_, Arc<Anim>>,
    content_height: f64,
) -> Result<(), String> {
    if !content_height.is_finite() {
        return Err(format!("content height is not a number: {content_height}"));
    }

    let scale = window
        .scale_factor()
        .map_err(|e| format!("no scale factor: {e}"))?;

    let logical = content_height.clamp(MIN_H, MAX_H);
    anim.target_h
        .store((logical * scale).round() as i32, Ordering::Relaxed);
    Ok(())
}

/// Which hotkey actually bound, if any — so the UI can say so rather than leaving
/// you to guess why nothing happens.
struct HotkeyLabel(Option<String>);

#[tauri::command]
fn hotkey_label(state: tauri::State<'_, HotkeyLabel>) -> Option<String> {
    state.0.clone()
}
