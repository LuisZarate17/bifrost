use std::sync::mpsc::{self, Sender};
use std::time::Instant;

use base64::prelude::*;
use futures_util::StreamExt;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::Manager;
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

    let hwnd = window.hwnd().map_err(|e| format!("no HWND: {e}"))?;

    // SAFETY: `hwnd` is a valid top-level window owned by this process, handed to
    // us by Tauri, and this runs on the main thread inside `setup()`.
    unsafe {
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
            .map_err(|e| format!("SetWindowDisplayAffinity failed: {e}"))
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
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
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![ask_about_screen])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
