use std::sync::mpsc::{self, Sender};
use std::time::Instant;

use base64::prelude::*;
use futures_util::StreamExt;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use tauri::{Emitter, Manager};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

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

/// Capture the screen, send it to Ollama, and stream the answer back token by token.
#[tauri::command]
async fn ask_about_screen(
    prompt: String,
    model: Option<String>,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());

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

/// Park the popup in the bottom-right of the **work area** — the screen minus the
/// taskbar. Using raw screen bounds would tuck it behind the taskbar.
fn position_bottom_right(window: &tauri::WebviewWindow) -> Result<(), String> {
    let monitor = window
        .current_monitor()
        .map_err(|e| format!("no monitor: {e}"))?
        .ok_or_else(|| "no monitor detected".to_string())?;

    let work = monitor.work_area();
    let size = window.outer_size().map_err(|e| format!("no size: {e}"))?;

    const MARGIN: i32 = 16;

    // An auto-hiding taskbar reserves nothing, so `work_area` comes back as the
    // full screen and the popup lands exactly where the taskbar slides over it.
    // Detect that by comparing work area to the monitor, and leave room.
    let auto_hidden_taskbar = work.size.height >= monitor.size().height;
    let bottom_margin = if auto_hidden_taskbar { 64 } else { MARGIN };

    let x = work.position.x + work.size.width as i32 - size.width as i32 - MARGIN;
    let y = work.position.y + work.size.height as i32 - size.height as i32 - bottom_margin;

    window
        .set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x, y)))
        .map_err(|e| format!("set_position failed: {e}"))
}

/// Show the popup **without taking focus**, so the cursor stays in whatever you
/// were typing in.
///
/// Tauri has no API for this — `show()` + `set_focus()` activates the window, and
/// `show()` alone still activates on Windows. tauri#11814 tracks the gap and is
/// open with no fix. `SW_SHOWNOACTIVATE` is the Win32 primitive that does it, and
/// we already link the `windows` crate.
#[cfg(windows)]
fn show_without_focus(window: &tauri::WebviewWindow) -> Result<(), String> {
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

    // Click-through: let the mouse reach whatever is behind the popup. This is
    // the ONE Tauri feature that sets WS_EX_LAYERED, and SetWindowDisplayAffinity
    // is reported to fail on layered windows. Whether order matters was flagged
    // unverified by research — so set it, then read the affinity back from
    // Windows and say plainly whether it survived.
    // Opt-in, so click-through and the backdrop can be tested independently:
    // WS_EX_LAYERED is the remaining suspect for the backdrop not rendering.
    if std::env::var("WARDEN_CLICKTHROUGH").is_ok() {
        window
            .set_ignore_cursor_events(true)
            .map_err(|e| format!("click-through failed: {e}"))?;
        println!("[warden] click-through enabled (WS_EX_LAYERED set)");
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

    report_affinity(window);
    follow_taskbar(window.clone());
    Ok(())
}

/// The taskbar's current top edge, in physical pixels.
///
/// An auto-hiding taskbar does **not** change the work area when it slides in and
/// out — the work area stays full-screen either way, which is why a static margin
/// can't track it. The taskbar window itself does move, so read that instead.
#[cfg(windows)]
fn taskbar_top() -> Option<i32> {
    use windows::core::w;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};

    // SAFETY: FindWindowW returns Err if absent; GetWindowRect only reads.
    unsafe {
        let hwnd = FindWindowW(w!("Shell_TrayWnd"), None).ok()?;
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some(rect.top)
    }
}

/// Keep the popup sitting just above the taskbar while it's on screen, so it
/// slides with it instead of being covered.
///
/// Polls rather than listening: tracking the shell's appbar notifications means
/// registering as an appbar ourselves, which is a lot of machinery for a window
/// that's visible for a few seconds. Repositioning only happens when the edge
/// actually moves, so an idle popup does no work.
#[cfg(windows)]
fn follow_taskbar(window: tauri::WebviewWindow) {
    tauri::async_runtime::spawn(async move {
        let mut last_y: Option<i32> = None;

        while is_really_visible(&window) {
            if let (Some(tray_top), Ok(size)) = (taskbar_top(), window.outer_size()) {
                const GAP: i32 = 12;
                let y = tray_top - size.height as i32 - GAP;

                if last_y != Some(y) {
                    if let Ok(pos) = window.outer_position() {
                        let _ = window.set_position(tauri::Position::Physical(
                            tauri::PhysicalPosition::new(pos.x, y),
                        ));
                    }
                    last_y = Some(y);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

    let primary = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyW);
    let fallback = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    // Fires on BOTH press and release — without this guard every
                    // action would run twice per keypress.
                    if event.state() != ShortcutState::Pressed {
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

                        println!("[warden] hotkey: visible={visible}");
                        if visible {
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

                        if let Err(e) = position_bottom_right(&window) {
                            eprintln!("[warden] WARNING: positioning failed: {e}");
                        }

                        #[cfg(windows)]
                        if let Err(e) = show_without_focus(&window) {
                            eprintln!("[warden] WARNING: show failed: {e}");
                        }

                        // The frontend owns the Channel, so it starts the capture.
                        let _ = app.emit("warden://summoned", ());
                    });
                })
                .build(),
        )
        .setup(move |app| {
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
                if let Err(e) = position_bottom_right(&window) {
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![ask_about_screen, hotkey_label, fit_window])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Grow the popup to fit its answer, within bounds, and keep it anchored.
///
/// Answers run 3,600–7,950 characters (measured at A2), so a fixed height means
/// either wasted space on short answers or heavy scrolling on long ones. The
/// window is repositioned after every resize because it grows from the top edge
/// — without that it would creep down over the taskbar.
#[tauri::command]
fn fit_window(window: tauri::WebviewWindow, content_height: f64) -> Result<(), String> {
    const MIN_H: u32 = 220;
    const MAX_H: u32 = 760;
    const WIDTH: u32 = 480;

    let target = (content_height.ceil() as u32).clamp(MIN_H, MAX_H);

    let current = window.outer_size().map_err(|e| format!("no size: {e}"))?;
    // Resizing on every token would thrash the compositor; ignore small deltas.
    if current.height.abs_diff(target) < 8 {
        return Ok(());
    }

    window
        .set_size(tauri::Size::Physical(tauri::PhysicalSize::new(WIDTH, target)))
        .map_err(|e| format!("resize failed: {e}"))?;

    position_bottom_right(&window)
}

/// Which hotkey actually bound, if any — so the UI can say so rather than leaving
/// you to guess why nothing happens.
struct HotkeyLabel(Option<String>);

#[tauri::command]
fn hotkey_label(state: tauri::State<'_, HotkeyLabel>) -> Option<String> {
    state.0.clone()
}
