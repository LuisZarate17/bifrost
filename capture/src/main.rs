use std::io::{stdout, BufRead, BufReader, Write};
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, Instant};

use base64::prelude::*;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde::Deserialize;
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

/// A 1080p screen costs ~2,042 image tokens (measured at A2). The Ollama default
/// is derived from VRAM and lands at 4k on this card, which would silently
/// truncate the screenshot — so it is always set explicitly.
const NUM_CTX: u32 = 8192;

/// One captured frame, as owned data. No borrows, so it can leave the callback.
struct Shot {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// One line of Ollama's streamed response.
#[derive(Deserialize)]
struct Chunk {
    response: String,
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
    #[serde(default)]
    total_duration: Option<u64>,
}

/// The capture handler. It owns the sending half of a channel and posts one
/// `Shot` back to `main` before stopping the capture.
struct Capture {
    sender: Sender<Shot>,
}

impl GraphicsCaptureApiHandler for Capture {
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
        // when this callback returns, so `.to_vec()` copies it into memory we own.
        let pixels = buffer.as_nopadding_buffer(&mut scratch).to_vec();

        self.sender.send(Shot {
            pixels,
            width,
            height,
        })?;

        control.stop();
        Ok(())
    }
}

/// Encode raw RGBA bytes straight into PNG bytes, without touching the disk.
/// `PngEncoder` only needs a `Write`, and `Vec<u8>` is one — so no file and no
/// `Cursor` are involved.
fn rgba_to_png(pixels: &[u8], width: u32, height: u32) -> image::ImageResult<Vec<u8>> {
    let mut png = Vec::new();
    PngEncoder::new(&mut png).write_image(pixels, width, height, ExtendedColorType::Rgba8)?;
    Ok(png)
}

/// Grab one frame from the primary monitor.
fn capture_screen() -> Shot {
    let monitor = Monitor::primary().expect("no primary monitor found");
    let (sender, receiver) = channel::<Shot>();

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

    Capture::start(settings).expect("screen capture failed");
    receiver.recv().expect("capture produced no frame")
}

fn main() {
    // Args: an optional delay in seconds, and an optional `--model <tag>`.
    let mut delay_secs: u64 = 0;
    let mut model = DEFAULT_MODEL.to_string();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                model = args.get(i).expect("--model needs a value").clone();
            }
            other => {
                if let Ok(secs) = other.parse::<u64>() {
                    delay_secs = secs;
                }
            }
        }
        i += 1;
    }

    let prompt = std::fs::read_to_string("prompt.txt")
        .unwrap_or_else(|_| {
            eprintln!("warning: prompt.txt not found, using built-in default");
            "What am I looking at? If there's an error or something broken, explain it \
             and the fix. Otherwise explain what matters here."
                .to_string()
        })
        .trim()
        .to_string();

    if delay_secs > 0 {
        println!("capturing in {delay_secs}s...");
        std::thread::sleep(std::time::Duration::from_secs(delay_secs));
    }

    let shot = capture_screen();
    let png = rgba_to_png(&shot.pixels, shot.width, shot.height).expect("PNG encode failed");

    // Keep a copy on disk — when an answer looks wrong it matters whether the
    // model misread the screen or we sent it the wrong screen.
    std::fs::write("out.png", &png).expect("failed to write out.png");

    println!(
        "captured {}x{} -> {} KB PNG, asking {}...\n",
        shot.width,
        shot.height,
        png.len() / 1024,
        model
    );

    // Ollama wants raw base64 with no `data:` prefix, and `images` sits at the
    // top level while `num_ctx` goes inside `options`.
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "images": [BASE64_STANDARD.encode(&png)],
        "stream": true,
        "options": { "num_ctx": NUM_CTX },
    });

    let started = Instant::now();
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(OLLAMA_URL)
        .json(&body)
        .send()
        .expect("request to Ollama failed — is `ollama serve` running?");

    // Read the body incrementally. `Response` implements `std::io::Read`, so
    // `BufReader::lines()` hands us each NDJSON line as it arrives rather than
    // waiting for the whole answer. It also handles the two splitting problems
    // for us: a network chunk can end mid-line, and it can end mid-character.
    let reader = BufReader::new(response);
    let mut first_token_at: Option<Duration> = None;

    for line in reader.lines() {
        let line = line.expect("failed to read a line from Ollama");
        if line.trim().is_empty() {
            continue;
        }

        let chunk: Chunk = serde_json::from_str(&line).expect("bad JSON line from Ollama");

        if !chunk.response.is_empty() && first_token_at.is_none() {
            first_token_at = Some(started.elapsed());
        }

        print!("{}", chunk.response);
        // stdout is line-buffered, so without this the tokens would pile up
        // until a newline and we would have rebuilt the bug we just fixed.
        stdout().flush().expect("failed to flush stdout");

        if chunk.done {
            println!("\n");
            println!(
                "[{} tokens in, {} out | first token {:.2}s | total {:.1}s]",
                chunk.prompt_eval_count.unwrap_or(0),
                chunk.eval_count.unwrap_or(0),
                first_token_at.unwrap_or_default().as_secs_f64(),
                chunk.total_duration.unwrap_or(0) as f64 / 1e9,
            );
        }
    }
}
