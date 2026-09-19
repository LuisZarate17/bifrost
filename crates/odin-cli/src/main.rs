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

mod power;

use odin_core::{config, ollama};

// DEFAULT_MODEL and NUM_CTX moved to odin-core::config at Before-G0 step 3.
// They were duplicated here and in Warden, and the num_ctx copies had already
// drifted apart - this one said 8192 while Warden said 32768, so `odin warm`
// loaded a runner the hotkey then threw away. Measured 2026-09-19: switching
// context cost a 3.09s reload every time.

// The reason it must be set at all still holds, and now lives with the constant
// in odin-core::config: a 1080p screen costs ~2,042 image tokens (A2), and
// Ollama derives its own default from VRAM, landing at 4k on this card — which
// would silently truncate the screenshot.

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
    /// Nanoseconds Ollama spent loading the model for *this* request.
    ///
    /// Worth printing because it is the only proof a `capture warm` actually
    /// helped. Ollama keys a loaded runner by the model *and* the options that
    /// change its memory layout — `num_ctx` among them — so a warm at the wrong
    /// context silently reloads here and buys nothing. Warm, then capture: this
    /// should read 0.00s.
    #[serde(default)]
    load_duration: Option<u64>,
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

const USAGE: &str = "\
odin — ask a local vision model about your screen

  odin [SECONDS] [--model TAG]   capture the screen and ask about it
  odin warm      [--model TAG]   load the model and pin it in VRAM
  odin off       [--model TAG]   release it and give the card its memory back
  odin status    [--model TAG]   what is loaded, per Ollama and per the card
  odin help                      this text

The model, the prompt and num_ctx come from %APPDATA%\\odin\\config.json.
Delete it and the built-in defaults apply — a missing config is a normal state.
";

enum Command {
    Capture { delay_secs: u64, model: String },
    Warm { model: String },
    Off { model: String },
    Status { model: String },
    Help,
}

/// Parse the flags shared by every subcommand.
///
/// The old loop silently ignored anything it did not recognise, so `capture warm`
/// took a screenshot and a typo cost nine seconds of GPU. Unknown tokens are now
/// an error — which is also what makes a mistyped subcommand fail loudly.
fn parse_flags(args: &[String], cfg: &config::Config) -> Result<(u64, String), String> {
    let mut delay_secs: u64 = 0;
    let mut model = cfg.model.clone();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                model = args
                    .get(i)
                    .ok_or_else(|| "--model needs a value".to_string())?
                    .clone();
            }
            other => match other.parse::<u64>() {
                Ok(secs) => delay_secs = secs,
                Err(_) => return Err(format!("unknown argument: {other}")),
            },
        }
        i += 1;
    }

    Ok((delay_secs, model))
}

fn parse_args(args: &[String], cfg: &config::Config) -> Result<Command, String> {
    // The subcommand, if any, is the first token — matched before the flag loop
    // so it can never be mistaken for a delay or swallowed as an unknown.
    let (word, rest) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        None => ("", &[] as &[String]),
    };

    match word {
        "warm" | "off" | "status" => {
            let (_, model) = parse_flags(rest, cfg)?;
            Ok(match word {
                "warm" => Command::Warm { model },
                "off" => Command::Off { model },
                _ => Command::Status { model },
            })
        }
        "help" | "--help" | "-h" => Ok(Command::Help),
        // No subcommand: the whole slice is capture's own arguments.
        _ => {
            let (delay_secs, model) = parse_flags(args, cfg)?;
            Ok(Command::Capture { delay_secs, model })
        }
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Read once, here, rather than in each function that wants a field. Three
    // scattered load() calls would be three chances to disagree inside one run.
    let cfg = config::load();

    let command = match parse_args(&args, &cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };

    let result = match command {
        Command::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Command::Warm { model } => power::warm(&model, cfg.num_ctx),
        Command::Off { model } => power::off(&model),
        Command::Status { model } => power::status(&model),
        Command::Capture { delay_secs, model } => {
            capture_and_ask(delay_secs, &model, &cfg);
            Ok(())
        }
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn capture_and_ask(delay_secs: u64, model: &str, cfg: &config::Config) {
    // Was `read_to_string("prompt.txt")` with an inline copy of the prompt as the
    // fallback. Two problems, both fixed by reading the shared config instead:
    // the path was relative, so running the binary from anywhere but its own
    // directory silently fell back without saying so; and the fallback was itself
    // a fourth copy of the same words that nothing kept in step.
    let prompt = cfg.prompt.trim();

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
        "options": { "num_ctx": cfg.num_ctx },
    });

    let started = Instant::now();
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(ollama::url("/api/generate"))
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
                "[{} tokens in, {} out | first token {:.2}s | total {:.1}s | load {:.2}s]",
                chunk.prompt_eval_count.unwrap_or(0),
                chunk.eval_count.unwrap_or(0),
                first_token_at.unwrap_or_default().as_secs_f64(),
                chunk.total_duration.unwrap_or(0) as f64 / 1e9,
                chunk.load_duration.unwrap_or(0) as f64 / 1e9,
            );
        }
    }
}
