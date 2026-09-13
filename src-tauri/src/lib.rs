use std::time::Instant;

use base64::prelude::*;
use futures_util::StreamExt;
use serde::Serialize;
use tauri::ipc::Channel;

const OLLAMA_URL: &str = "http://127.0.0.1:11434/api/generate";
const DEFAULT_MODEL: &str = "qwen3-vl:8b-instruct-q4_K_M";

/// A 1080p screen costs ~2,042 image tokens (measured at A2). Ollama derives its
/// default from detected VRAM and lands at 4k on this card, which would silently
/// truncate the screenshot — so it is always set explicitly.
const NUM_CTX: u32 = 8192;

/// One message pushed up to the UI. Sent through a `Channel`, never returned from
/// the command: a command's `Result` is delivered as a single IPC reply, so
/// returning the whole answer would buffer it by construction.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "event", content = "data")]
enum StreamEvent {
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

/// Send an image plus the default prompt to Ollama and stream the answer back
/// token by token.
#[tauri::command]
async fn ask_about_image(
    image_path: String,
    prompt: String,
    model: Option<String>,
    on_event: Channel<StreamEvent>,
) -> Result<(), String> {
    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let png = std::fs::read(&image_path)
        .map_err(|e| format!("could not read {image_path}: {e}"))?;
    let b64 = BASE64_STANDARD.encode(&png);

    // `images` is top-level; `num_ctx` lives inside `options`. Either wrong fails silently.
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "images": [b64],
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


#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![ask_about_image])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
