#!/usr/bin/env node
// evals/bench.mjs
//
// Zero-dependency benchmark harness for local Ollama vision models (Vidar A2).
// Only Node built-ins are used: global fetch/AbortController/TextDecoder, node:fs,
// node:path, node:crypto, node:child_process, node:perf_hooks, node:url.
//
// Usage:
//   node evals/bench.mjs --models qwen3-vl:4b-instruct-q4_K_M,qwen3-vl:8b-instruct-q4_K_M \
//       [--images path1.png,path2.png] [--reps 3] [--num-ctx 8192] [--num-predict 2048] \
//       [--warmup-image path.png] [--prompt-file prompt.txt] [--out evals/out] \
//       [--gpu-trace] [--mode matrix|gate] [--no-warmup]
//
// See docs/archive/PLAN.md / BUILD.md (F1) for the motivation and docs/archive/stages/STAGE-A.md for the A2 write-up this feeds.
//
// IMPORTANT — prompt-cache correctness (measured empirically on this machine, 2026-09-08):
// Ollama caches prefill across successive /api/generate calls to the SAME loaded model whenever
// the token prefix matches, and it does this independently per image — simply alternating between
// two eval images (A,B,A,B,...) does NOT defeat the cache once both images have been seen once:
// prefill for a "repeat" image collapses from ~450ms to ~5ms with prompt_eval_cached_count equal
// to nearly the whole prompt. The only thing that reliably clears it is an explicit unload
// (POST /api/generate {model, prompt:"", keep_alive:0}, then poll /api/ps until gone). So every
// measured rep-cycle here unloads the model, does a throwaway warm-up generate against a SMALL
// SYNTHETIC image (never one of the eval images — warming up with an eval image would cache it
// and destroy the very measurement we're trying to take), discards that warm-up's timings (but
// keeps its load_duration, which is the real cold-load number), and only then measures the real
// eval images. See CACHE_CONTAMINATION_ABS_THRESHOLD / _RATIO_THRESHOLD below for how "honest
// cold" is distinguished from "still cached" — a truly cold run still reports a small nonzero
// prompt_eval_cached_count (a few tokens of shared prompt-template prefix), so the check is a
// threshold, not prompt_eval_cached_count === 0.

import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import zlib from 'node:zlib'; // still a Node built-in, not a third-party dep — used only to
// deflate-compress the IDAT chunk of the tiny synthetic warm-up PNG we generate in-process.
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const OLLAMA_URL = 'http://127.0.0.1:11434';
const VRAM_TOLERANCE_MIB = 150;
const REQUEST_TIMEOUT_MS = 180_000; // per-generate-request hard timeout
const SHORT_TIMEOUT_MS = 10_000; // version/tags/ps preflight-style calls
const UNLOAD_POLL_TIMEOUT_MS = 30_000;
const UNLOAD_POLL_INTERVAL_MS = 1_000;
const VRAM_SAMPLE_COUNT = 3;
const VRAM_SAMPLE_DELAY_MS = 1_000;

// num_predict used ONLY for the per-rep-cycle throwaway warm-up call. Its own answer is
// discarded entirely — we just need enough decode steps past TTFT to warm the decode kernels
// too, without paying for a full ~2048-token generation on every cycle.
const WARMUP_NUM_PREDICT = 16;

// A run is "cache-contaminated" (its prefill timing is not an honest cold read) if the fraction
// of the prompt that was cache-hit exceeds this ratio, OR the absolute cached-token count is
// large. An honest cold run still reports a small nonzero prompt_eval_cached_count (a shared
// prompt-template prefix of a few tokens), so asserting === 0 is wrong — these thresholds were
// picked to separate that legitimate small overlap from real cache reuse (empirically, a
// genuinely cached repeat read reports cached counts in the thousands, ~100% of the prompt).
const CACHE_CONTAMINATION_ABS_THRESHOLD = 100;
const CACHE_CONTAMINATION_RATIO_THRESHOLD = 0.05;

// The default prompt is treated as a tested variable (see PLAN.md), not a tweak buried in
// argument parsing. It is the ONE exported constant other tooling (e.g. a future prompt-variant
// sweep) should import rather than re-typing.
export const DEFAULT_PROMPT =
  "What am I looking at? If there's an error or something broken, explain it and the fix. " +
  'Otherwise explain what matters here.';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const DEFAULT_OUT_DIR = path.join(__dirname, 'out');
const DEFAULT_IMAGES = [
  path.join(__dirname, 'shots', 'terminal-rust-error.png'),
  path.join(__dirname, 'shots', 'editor-rust-code.png'),
];

const RUNS_CSV_HEADER = [
  'model',
  'size',
  'quant',
  'image',
  'rep',
  'ttft_ms',
  'total_ms',
  'load_ms',
  'prompt_eval_count',
  'prompt_eval_cached_count',
  'prompt_eval_ms',
  'prefill_tok_s',
  'eval_count',
  'eval_ms',
  'decode_tok_s',
  'done_reason',
  'output_chars',
  'output_sha256',
].join(',');

const VRAM_CSV_HEADER = [
  'model',
  'baseline_mib',
  'resident_mib',
  'delta_mib',
  'ollama_size_vram_mib',
  'post_unload_mib',
  'returned_to_baseline',
].join(',');

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

class BenchError extends Error {
  constructor(message) {
    super(message);
    this.name = 'BenchError';
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function median(arr) {
  if (!arr || arr.length === 0) return null;
  const sorted = [...arr].sort((a, b) => a - b);
  const n = sorted.length;
  const mid = Math.floor(n / 2);
  return n % 2 === 0 ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid];
}

function sha256Hex(bufOrStr) {
  const buf = Buffer.isBuffer(bufOrStr) ? bufOrStr : Buffer.from(bufOrStr, 'utf8');
  return crypto.createHash('sha256').update(buf).digest('hex');
}

function slugifyTag(tag) {
  // Windows filenames cannot contain ':' — also guard against '/' for namespaced tags.
  return tag.replace(/[:/\\]/g, '_');
}

function parseTagInfo(tag) {
  // Derive size (e.g. "4b") and quant (e.g. "q4_K_M") by parsing the TAG STRING only,
  // not from /api/tags metadata, per spec. Tags with no colon (no variant) yield both empty.
  const variant = tag.includes(':') ? tag.split(':').slice(1).join(':') : '';
  const sizeMatch = variant.match(/(\d+(?:\.\d+)?)[bB](?![a-zA-Z])/);
  const size = sizeMatch ? `${sizeMatch[1]}b` : '';
  const quantMatch = variant.match(/\b([qQ]\d+(?:_[A-Za-z0-9]+)*)\b/);
  const quant = quantMatch ? quantMatch[1] : '';
  return { size, quant };
}

function readPngDimensions(buf) {
  // PNG: 8-byte signature, then first chunk is IHDR: 4-byte length + 4-byte type "IHDR",
  // then width (bytes 16-19) and height (bytes 20-23), both big-endian uint32.
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) };
}

function isCacheContaminated(cachedCount, promptEvalCount) {
  const c = cachedCount || 0;
  if (c <= 0) return false;
  if (c > CACHE_CONTAMINATION_ABS_THRESHOLD) return true;
  if (promptEvalCount && c / promptEvalCount > CACHE_CONTAMINATION_RATIO_THRESHOLD) return true;
  return false;
}

// ---------------------------------------------------------------------------
// Synthetic warm-up PNG (zero-dependency: hand-rolled PNG encoder, CRC32 table, zlib deflate)
// ---------------------------------------------------------------------------

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const typeBuf = Buffer.from(type, 'ascii');
  const lenBuf = Buffer.alloc(4);
  lenBuf.writeUInt32BE(data.length, 0);
  const crcBuf = Buffer.alloc(4);
  crcBuf.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([lenBuf, typeBuf, data, crcBuf]);
}

/** Builds a minimal valid 8-bit RGB PNG in-process — no image library needed. */
function makeSyntheticPng(width = 64, height = 64, rgb = [128, 128, 128]) {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const ihdrData = Buffer.alloc(13);
  ihdrData.writeUInt32BE(width, 0);
  ihdrData.writeUInt32BE(height, 4);
  ihdrData[8] = 8; // bit depth
  ihdrData[9] = 2; // color type: RGB
  ihdrData[10] = 0; // compression method
  ihdrData[11] = 0; // filter method
  ihdrData[12] = 0; // interlace method
  const ihdr = pngChunk('IHDR', ihdrData);

  const rowBytes = 1 + width * 3;
  const raw = Buffer.alloc(rowBytes * height);
  for (let y = 0; y < height; y++) {
    const rowStart = y * rowBytes;
    raw[rowStart] = 0; // filter type: none
    for (let x = 0; x < width; x++) {
      const px = rowStart + 1 + x * 3;
      raw[px] = rgb[0];
      raw[px + 1] = rgb[1];
      raw[px + 2] = rgb[2];
    }
  }
  const idat = pngChunk('IDAT', zlib.deflateSync(raw));
  const iend = pngChunk('IEND', Buffer.alloc(0));
  return Buffer.concat([signature, ihdr, idat, iend]);
}

function bytesToMiB(bytes) {
  if (bytes === null || bytes === undefined) return null;
  return Math.round(bytes / (1024 * 1024));
}

function fmtNum(v, decimals = 2) {
  if (v === null || v === undefined || Number.isNaN(v)) return '';
  return v.toFixed(decimals);
}

function csvEscape(v) {
  if (v === null || v === undefined) return '';
  const s = String(v);
  if (/[",\n\r]/.test(s)) return '"' + s.replace(/"/g, '""') + '"';
  return s;
}

function isoStampSafe(date) {
  // Filesystem-safe ISO stamp for Windows: no ':' allowed in filenames.
  return date.toISOString().replace(/:/g, '-').replace(/\.\d+Z$/, 'Z');
}

function nsToMs(ns) {
  return ns === null || ns === undefined ? null : ns / 1e6;
}

// ---------------------------------------------------------------------------
// nvidia-smi helpers
// ---------------------------------------------------------------------------

function nvidiaSmiSync(args) {
  const res = spawnSync('nvidia-smi', args, { encoding: 'utf8' });
  if (res.error) {
    throw new BenchError(`Failed to run nvidia-smi ${args.join(' ')}: ${res.error.message}`);
  }
  if (res.status !== 0) {
    throw new BenchError(`nvidia-smi ${args.join(' ')} exited with status ${res.status}: ${res.stderr}`);
  }
  return res.stdout.trim();
}

async function sampleVramMedianMib(n = VRAM_SAMPLE_COUNT, delayMs = VRAM_SAMPLE_DELAY_MS) {
  const samples = [];
  for (let i = 0; i < n; i++) {
    const out = nvidiaSmiSync(['--query-gpu=memory.used', '--format=csv,noheader,nounits']);
    const firstLine = out.split(/\r?\n/)[0].trim();
    const val = Number(firstLine);
    if (!Number.isFinite(val)) {
      throw new BenchError(`Could not parse nvidia-smi memory.used output: "${out}"`);
    }
    samples.push(val);
    if (i < n - 1) await sleep(delayMs);
  }
  return median(samples);
}

function getGpuInfo() {
  const out = nvidiaSmiSync(['--query-gpu=name,driver_version', '--format=csv,noheader']);
  const line = out.split(/\r?\n/)[0];
  const [name, driver_version] = line.split(',').map((s) => s.trim());
  return { name, driver_version };
}

function startGpuTrace() {
  const samples = [];
  let remainder = '';
  let proc;
  try {
    proc = spawn('nvidia-smi', [
      '--query-gpu=timestamp,memory.used,utilization.gpu',
      '--format=csv,noheader,nounits',
      '-lms',
      '250',
    ]);
  } catch (e) {
    console.warn(`WARNING: failed to start gpu-trace nvidia-smi: ${e.message}`);
    return null;
  }
  proc.on('error', (e) => {
    console.warn(`WARNING: gpu-trace nvidia-smi process error: ${e.message}`);
  });
  proc.stdout.on('data', (data) => {
    remainder += data.toString('utf8');
    const lines = remainder.split(/\r?\n/);
    remainder = lines.pop();
    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      const parts = trimmed.split(',').map((s) => s.trim());
      if (parts.length >= 3) {
        samples.push({
          timestamp: parts[0],
          memory_used_mib: Number(parts[1]),
          utilization_gpu_pct: Number(parts[2]),
        });
      }
    }
  });
  return { proc, samples };
}

function stopGpuTrace(trace) {
  if (!trace) return undefined;
  try {
    trace.proc.kill();
  } catch {
    // best effort
  }
  return trace.samples;
}

// ---------------------------------------------------------------------------
// Ollama HTTP helpers
// ---------------------------------------------------------------------------

async function getVersion() {
  const res = await fetch(`${OLLAMA_URL}/api/version`, { signal: AbortSignal.timeout(SHORT_TIMEOUT_MS) });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

async function getTags() {
  const res = await fetch(`${OLLAMA_URL}/api/tags`, { signal: AbortSignal.timeout(SHORT_TIMEOUT_MS) });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

async function getPs() {
  const res = await fetch(`${OLLAMA_URL}/api/ps`, { signal: AbortSignal.timeout(SHORT_TIMEOUT_MS) });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

async function unloadModel(model) {
  const res = await fetch(`${OLLAMA_URL}/api/generate`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model, prompt: '', keep_alive: 0, stream: false }),
    signal: AbortSignal.timeout(SHORT_TIMEOUT_MS),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new BenchError(`Unload request for ${model} failed: HTTP ${res.status} ${text}`);
  }
  await res.json().catch(() => ({}));

  const start = Date.now();
  while (Date.now() - start < UNLOAD_POLL_TIMEOUT_MS) {
    const ps = await getPs();
    const stillLoaded = (ps.models || []).some((m) => m.name === model || m.model === model);
    if (!stillLoaded) return true;
    await sleep(UNLOAD_POLL_INTERVAL_MS);
  }
  return false;
}

/**
 * Issue one /api/generate call and stream the NDJSON response, measuring TTFT exactly on the
 * first chunk whose `response` field is a non-empty string (never on first byte / first chunk).
 */
async function generateOnce({ model, promptText, imageBase64, options, traceGpu = false, timeoutMs = REQUEST_TIMEOUT_MS }) {
  const url = `${OLLAMA_URL}/api/generate`;
  const body = {
    model,
    prompt: promptText,
    stream: true,
    keep_alive: -1,
    options,
  };
  if (imageBase64) body.images = [imageBase64];

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(new Error(`exceeded ${timeoutMs}ms timeout`)), timeoutMs);

  const trace = traceGpu ? startGpuTrace() : null;

  const startWall = new Date().toISOString();
  const t0 = performance.now();

  let res;
  try {
    res = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
  } catch (e) {
    clearTimeout(timer);
    stopGpuTrace(trace);
    if (e.name === 'AbortError' || controller.signal.aborted) {
      throw new BenchError(`Request to /api/generate timed out after ${timeoutMs}ms for model=${model}`);
    }
    throw new BenchError(`fetch to /api/generate failed for model=${model}: ${e.message}`);
  }

  if (res.status !== 200) {
    const text = await res.text().catch(() => '(failed to read body)');
    clearTimeout(timer);
    stopGpuTrace(trace);
    throw new BenchError(`/api/generate returned HTTP ${res.status} for model=${model}. Body:\n${text}`);
  }

  const decoder = new TextDecoder('utf-8');
  let remainder = '';
  let ttftMs = null;
  let answer = '';
  let finalObj = null;

  const consumeLine = (line) => {
    const trimmed = line.trim();
    if (!trimmed) return;
    let obj;
    try {
      obj = JSON.parse(trimmed);
    } catch (e) {
      throw new BenchError(`Failed to parse NDJSON line from model=${model}: ${e.message}\nLine: ${trimmed}`);
    }
    if (obj.error) {
      throw new BenchError(`Ollama returned an error for model=${model}: ${obj.error}`);
    }
    if (ttftMs === null && typeof obj.response === 'string' && obj.response.length > 0) {
      ttftMs = performance.now() - t0;
    }
    if (typeof obj.response === 'string') answer += obj.response;
    if (obj.done) finalObj = obj;
  };

  try {
    for await (const chunk of res.body) {
      // TextDecoder with {stream:true} correctly buffers a codepoint split across chunks.
      remainder += decoder.decode(chunk, { stream: true });
      const lines = remainder.split('\n');
      remainder = lines.pop(); // keep the trailing partial line for next iteration
      for (const line of lines) consumeLine(line);
    }
    remainder += decoder.decode(); // flush
    if (remainder.trim()) consumeLine(remainder);
  } catch (e) {
    clearTimeout(timer);
    stopGpuTrace(trace);
    if (e instanceof BenchError) throw e;
    if (e.name === 'AbortError' || controller.signal.aborted) {
      throw new BenchError(`Request to /api/generate timed out after ${timeoutMs}ms for model=${model}`);
    }
    throw new BenchError(`Error while streaming response for model=${model}: ${e.stack || e.message}`);
  }

  clearTimeout(timer);
  const endWall = new Date().toISOString();
  const gpuTrace = stopGpuTrace(trace);

  if (!finalObj) {
    throw new BenchError(`Stream ended without a final done:true object for model=${model}`);
  }

  return { finalObj, answer, ttftMs, startWall, endWall, gpuTrace };
}

// ---------------------------------------------------------------------------
// Validation (fail-loudly rules)
// ---------------------------------------------------------------------------

function validateRun(result, { label, expectTruncation = false }) {
  const f = result.finalObj;
  const flags = { truncated: false, cache_contaminated: false };

  if (!f.eval_count || f.eval_count === 0 || !result.answer || result.answer.length === 0) {
    throw new BenchError(
      `[${label}] eval_count is 0 or the answer is empty — aborting. eval_count=${f.eval_count} answer_len=${result.answer ? result.answer.length : 0}`,
    );
  }

  if (f.done_reason !== 'stop') {
    flags.truncated = f.done_reason === 'length';
    // expectTruncation is set for the throwaway warm-up call, whose tiny num_predict is
    // *designed* to hit "length" — that's not a real truncation warning, its answer is discarded.
    if (!expectTruncation) {
      console.warn(
        `WARNING: [${label}] done_reason="${f.done_reason}" (expected "stop")` +
          (flags.truncated ? ' — output was TRUNCATED, this cell\'s grade is invalid' : ''),
      );
    }
  }

  flags.cache_contaminated = isCacheContaminated(f.prompt_eval_cached_count, f.prompt_eval_count);
  if (flags.cache_contaminated) {
    console.warn(
      `WARNING: [${label}] prompt_eval_cached_count=${f.prompt_eval_cached_count} of prompt_eval_count=${f.prompt_eval_count} exceeds the cache-contamination threshold (>${CACHE_CONTAMINATION_ABS_THRESHOLD} or >${CACHE_CONTAMINATION_RATIO_THRESHOLD * 100}%) — this run's prefill timing is not an honest cold read`,
    );
  }

  return flags;
}

// ---------------------------------------------------------------------------
// Image-token A/B test
// ---------------------------------------------------------------------------

async function imageTokenAB({ model, promptText, image, options, traceGpu }) {
  // Defensive: guarantee an honestly cold start regardless of what ran before us in this
  // process (or was left loaded by a previous invocation) — unload is the only thing that
  // reliably clears Ollama's prompt cache.
  console.log(`[ab] ensuring ${model} starts unloaded (clean cache state)...`);
  await unloadModel(model);

  console.log(`[ab] no-image call against ${model}...`);
  const noImgResult = await generateOnce({ model, promptText, imageBase64: null, options, traceGpu: false });
  const noImgFlags = validateRun(noImgResult, { label: `${model}/ab-noimage` });

  console.log(`[ab] with-image call against ${model} (image=${image.basename})...`);
  const withImgResult = await generateOnce({ model, promptText, imageBase64: image.base64, options, traceGpu });
  const withImgFlags = validateRun(withImgResult, { label: `${model}/ab-withimage` });

  const noCount = noImgResult.finalObj.prompt_eval_count;
  const withCount = withImgResult.finalObj.prompt_eval_count;
  const noCached = noImgResult.finalObj.prompt_eval_cached_count;
  const withCached = withImgResult.finalObj.prompt_eval_cached_count;

  let cacheWarning = null;
  if (isCacheContaminated(noCached, noCount) || isCacheContaminated(withCached, withCount)) {
    cacheWarning = `prompt_eval_cached_count exceeds the cache-contamination threshold in the A/B test: no-image=${noCached}/${noCount}, with-image=${withCached}/${withCount}`;
    console.warn(`WARNING: ${cacheWarning}`);
  }

  let imageTokens = null;
  let reason = null;
  if (
    typeof withCount !== 'number' ||
    withCount <= 1 ||
    typeof noCount !== 'number' ||
    !(withCount > noCount)
  ) {
    reason = `with-image prompt_eval_count (${withCount}) did not exceed no-image prompt_eval_count (${noCount}) as expected`;
    console.warn(`WARNING: image-token estimation failed: ${reason}`);
  } else {
    imageTokens = withCount - noCount;
  }

  return {
    noImgResult,
    withImgResult,
    noImgFlags,
    withImgFlags,
    no_image_prompt_eval_count: noCount,
    with_image_prompt_eval_count: withCount,
    image_tokens: imageTokens,
    reason,
    cache_warning: cacheWarning,
  };
}

// ---------------------------------------------------------------------------
// Run record + output writers
// ---------------------------------------------------------------------------

function buildRunRecord({ model, image, rep, warmup, tag, result, options, flags }) {
  const f = result.finalObj;
  const totalMs = nsToMs(f.total_duration);
  const loadMs = nsToMs(f.load_duration);
  const promptEvalMs = nsToMs(f.prompt_eval_duration);
  const evalMs = nsToMs(f.eval_duration);
  const prefillTokS = f.prompt_eval_count && promptEvalMs ? f.prompt_eval_count / (promptEvalMs / 1000) : null;
  const decodeTokS = f.eval_count && evalMs ? f.eval_count / (evalMs / 1000) : null;
  const answerSha256 = sha256Hex(result.answer);

  return {
    model,
    image_path: image.path,
    image_basename: image.basename,
    rep,
    warmup: !!warmup,
    tag: tag || null,
    started_at: result.startWall,
    ended_at: result.endWall,
    ttft_ms: result.ttftMs,
    raw: f, // every field Ollama returned, verbatim (nanoseconds as reported)
    derived_ms: {
      total_ms: totalMs,
      load_ms: loadMs,
      prompt_eval_ms: promptEvalMs,
      eval_ms: evalMs,
      prefill_tok_s: prefillTokS,
      decode_tok_s: decodeTokS,
    },
    options,
    answer: result.answer,
    answer_sha256: answerSha256,
    gpu_trace: result.gpuTrace,
    flags: flags || {},
  };
}

function runFileName(rec) {
  const slug = slugifyTag(rec.model);
  // Each rep-cycle now does its own unload+throwaway-warm-up (see the cache-correctness note at
  // the top of this file), so there can be several warm-ups per model — one per cycle, not one
  // per model — hence "rep<N>-warmup" rather than a single fixed "rep0".
  const repPart = rec.warmup ? `rep${rec.rep}-warmup` : rec.tag ? rec.tag : `rep${rec.rep}`;
  return `${slug}__${rec.image_basename}__${repPart}.json`;
}

function writeRunJson(outDir, rec) {
  const fname = runFileName(rec);
  const fpath = path.join(outDir, 'runs', fname);
  fs.writeFileSync(fpath, JSON.stringify(rec, null, 2));
  console.log(`  wrote runs/${fname}`);
  return fname;
}

function appendRunsCsvRow(outDir, rec) {
  const { size, quant } = parseTagInfo(rec.model);
  const f = rec.raw;
  const row = [
    rec.model,
    size,
    quant,
    rec.image_basename,
    rec.rep,
    fmtNum(rec.ttft_ms),
    fmtNum(rec.derived_ms.total_ms),
    fmtNum(rec.derived_ms.load_ms),
    f.prompt_eval_count ?? '',
    f.prompt_eval_cached_count ?? '',
    fmtNum(rec.derived_ms.prompt_eval_ms),
    fmtNum(rec.derived_ms.prefill_tok_s),
    f.eval_count ?? '',
    fmtNum(rec.derived_ms.eval_ms),
    fmtNum(rec.derived_ms.decode_tok_s),
    f.done_reason ?? '',
    [...rec.answer].length,
    rec.answer_sha256,
  ]
    .map(csvEscape)
    .join(',');
  fs.appendFileSync(path.join(outDir, 'runs.csv'), row + '\n');
}

function appendVramCsvRow(outDir, row) {
  const line = [
    row.model,
    row.baseline_mib,
    row.resident_mib,
    row.delta_mib,
    row.ollama_size_vram_mib ?? '',
    row.post_unload_mib,
    row.returned_to_baseline,
  ]
    .map(csvEscape)
    .join(',');
  fs.appendFileSync(path.join(outDir, 'vram.csv'), line + '\n');
}

function writeFailuresMd(outDir, failures) {
  try {
    fs.mkdirSync(outDir, { recursive: true });
    const lines = ['# FAILURES', '', `Aborted at: ${new Date().toISOString()}`, ''];
    for (const f of failures) {
      lines.push('- ' + String(f).split('\n').join('\n  '));
      lines.push('');
    }
    fs.writeFileSync(path.join(outDir, 'FAILURES.md'), lines.join('\n'));
  } catch (e) {
    console.error('Additionally failed to write FAILURES.md: ' + e.message);
  }
}

function writeSummaryMd(outDir, { summaryData, vramRows, images, models }) {
  const lines = [];
  lines.push('# A2 Benchmark Summary', '', `Generated: ${new Date().toISOString()}`, '');
  lines.push('## Per (model, image)', '');
  lines.push(
    '| Model | Image | median ttft_ms (range) | median total_ms (range) | median decode_tok_s | identical output across reps |',
  );
  lines.push('|---|---|---|---|---|---|');
  for (const model of models) {
    for (const image of images) {
      const d = summaryData.find((s) => s.model === model && s.image.path === image.path);
      if (!d) continue;
      const ttftMed = median(d.ttfts);
      const ttftMin = Math.min(...d.ttfts);
      const ttftMax = Math.max(...d.ttfts);
      const totMed = median(d.totals);
      const totMin = Math.min(...d.totals);
      const totMax = Math.max(...d.totals);
      const decMed = d.decodeTokS.length ? median(d.decodeTokS) : null;
      const identicalCell = d.identical ? 'yes' : 'NO';
      const cacheNote = d.anyCacheContaminated ? ' — **CACHE-CONTAMINATED, see runs/*.json** — prefill timing for at least one rep is not an honest cold read' : '';
      lines.push(
        `| ${model} | ${image.basename} | ${ttftMed.toFixed(1)} (${ttftMin.toFixed(1)}-${ttftMax.toFixed(1)}) | ${totMed.toFixed(1)} (${totMin.toFixed(1)}-${totMax.toFixed(1)}) | ${decMed !== null ? decMed.toFixed(2) : 'n/a'} | ${identicalCell}${cacheNote} |`,
      );
    }
  }

  lines.push(
    '',
    '## Cold load_duration (median across rep-cycles; each cycle unloads then warms up on a synthetic, non-eval image)',
    '',
    '| Model | median cold load_ms | min-max across cycles |',
    '|---|---|---|',
  );
  const seenModels = new Set();
  for (const d of summaryData) {
    if (seenModels.has(d.model)) continue;
    seenModels.add(d.model);
    if (d.coldLoadMs === null || d.coldLoadMs === undefined) {
      lines.push(`| ${d.model} | n/a (--no-warmup) | n/a |`);
    } else {
      const lo = Math.min(...d.coldLoadSamples);
      const hi = Math.max(...d.coldLoadSamples);
      lines.push(`| ${d.model} | ${d.coldLoadMs.toFixed(1)} | ${lo.toFixed(1)}-${hi.toFixed(1)} (n=${d.coldLoadSamples.length}) |`);
    }
  }

  lines.push(
    '',
    '## VRAM',
    '',
    '| Model | baseline MiB | resident MiB | delta MiB | ollama size_vram MiB | post-unload MiB | returned to baseline |',
    '|---|---|---|---|---|---|---|',
  );
  for (const r of vramRows) {
    lines.push(
      `| ${r.model} | ${r.baseline_mib} | ${r.resident_mib} | ${r.delta_mib} | ${r.ollama_size_vram_mib ?? 'n/a'} | ${r.post_unload_mib} | ${r.returned_to_baseline ? 'yes' : 'NO'} |`,
    );
  }

  lines.push('', '_No accuracy column — a human grades that separately using answers.md._', '');
  fs.writeFileSync(path.join(outDir, 'summary.md'), lines.join('\n'));
}

function labelName(i) {
  return i < 26 ? `Model ${String.fromCharCode(65 + i)}` : `Model ${i + 1}`;
}

function shuffledIndices(n) {
  const idx = Array.from({ length: n }, (_, i) => i);
  for (let i = idx.length - 1; i > 0; i--) {
    const j = crypto.randomInt(0, i + 1);
    [idx[i], idx[j]] = [idx[j], idx[i]];
  }
  return idx;
}

function writeAnswersMd(outDir, { answersByImage, images }) {
  const lines = [];
  lines.push('# Blind Answer Grading', '');
  lines.push(
    'Grouped by image. Model identity is BLINDED and shuffled independently per image — see `blind-key.json` to unblind after grading. No accuracy column here or in summary.md; grade by hand.',
    '',
  );
  const blindKey = {};
  for (const image of images) {
    const entries = answersByImage.get(image.path) || [];
    lines.push(`## Image: ${image.basename}`, '');
    lines.push(`\`${image.path}\` — ${image.width}x${image.height}, sha256 \`${image.sha256}\``, '');
    const order = shuffledIndices(entries.length);
    blindKey[image.basename] = {};
    order.forEach((origIdx, labelPos) => {
      const e = entries[origIdx];
      const label = labelName(labelPos);
      blindKey[image.basename][label] = e.model;
      lines.push(`### ${label}`, '');
      lines.push(`- rep: 1`);
      lines.push(`- output_chars: ${e.charCount}`);
      lines.push(`- identical across reps: ${e.identical ? 'yes' : 'NO'}`);
      lines.push('');
      lines.push(e.answer);
      lines.push('');
      lines.push('---', '');
    });
  }
  fs.writeFileSync(path.join(outDir, 'answers.md'), lines.join('\n'));
  return blindKey;
}

function buildManifest({ startedAt, version, tags, args, images, warmupImage, promptText, promptSha256, options, gpuInfo, harnessSha256 }) {
  const benchedTagInfo = args.models.map((tag) => {
    const t = tags.models.find((m) => m.name === tag);
    return { name: tag, digest: t?.digest ?? null, size: t?.size ?? null };
  });
  return {
    started_at: startedAt.toISOString(),
    ollama_version: version.version,
    ollama_url: OLLAMA_URL,
    mode: args.mode,
    models: benchedTagInfo,
    reps: args.reps,
    warmup: !args.noWarmup,
    warmup_num_predict: WARMUP_NUM_PREDICT,
    warmup_image: warmupImage
      ? { path: warmupImage.path, sha256: warmupImage.sha256, byte_size: warmupImage.byte_size, width: warmupImage.width, height: warmupImage.height, synthetic: warmupImage.synthetic }
      : null,
    gpu_trace: args.gpuTrace,
    prompt: { text: promptText, sha256: promptSha256, source: args.promptFile || '(DEFAULT_PROMPT constant in bench.mjs)' },
    options,
    cache_contamination_policy: {
      note:
        'A run is flagged cache_contaminated if prompt_eval_cached_count exceeds this absolute ' +
        'threshold or this fraction of prompt_eval_count. An honest cold run still reports a ' +
        'small nonzero cached count (shared prompt-template prefix), so the check is a ' +
        'threshold, not prompt_eval_cached_count === 0.',
      abs_threshold: CACHE_CONTAMINATION_ABS_THRESHOLD,
      ratio_threshold: CACHE_CONTAMINATION_RATIO_THRESHOLD,
    },
    images: images.map((img) => ({ path: img.path, sha256: img.sha256, byte_size: img.byte_size, width: img.width, height: img.height })),
    gpu: gpuInfo,
    harness: { path: __filename, sha256: harnessSha256 },
  };
}

// ---------------------------------------------------------------------------
// Preflight
// ---------------------------------------------------------------------------

async function preflight(args) {
  console.log(`Preflight: checking Ollama at ${OLLAMA_URL}/api/version ...`);
  let version;
  try {
    version = await getVersion();
  } catch (e) {
    throw new BenchError(`Ollama server unreachable at ${OLLAMA_URL} — is it running? (${e.message})`);
  }
  console.log(`Preflight: Ollama version ${version.version}`);

  console.log('Preflight: checking requested model tags against /api/tags ...');
  let tags;
  try {
    tags = await getTags();
  } catch (e) {
    throw new BenchError(`Failed to fetch /api/tags: ${e.message}`);
  }
  const available = new Set((tags.models || []).map((m) => m.name));
  const missing = args.models.filter((m) => !available.has(m));
  if (missing.length > 0) {
    throw new BenchError(
      `Missing model tag(s) not present in /api/tags: ${missing.join(', ')}. ` +
        `This harness never pulls models for you — pull them first (e.g. via the ollama CLI or another process) and re-run.`,
    );
  }
  console.log(`Preflight: all ${args.models.length} requested model tag(s) present.`);

  console.log('Preflight: checking image files ...');
  const images = [];
  for (const rawImgPath of args.images) {
    const imgPath = path.resolve(rawImgPath);
    if (!fs.existsSync(imgPath)) throw new BenchError(`Image not found: ${imgPath}`);
    const stat = fs.statSync(imgPath);
    if (stat.size === 0) throw new BenchError(`Image is zero bytes: ${imgPath}`);
    const buf = fs.readFileSync(imgPath);
    if (!(buf.length >= 4 && buf[0] === 0x89 && buf[1] === 0x50 && buf[2] === 0x4e && buf[3] === 0x47)) {
      throw new BenchError(`Image does not start with PNG magic bytes (89 50 4E 47): ${imgPath}`);
    }
    const dims = readPngDimensions(buf);
    images.push({
      path: imgPath,
      basename: path.basename(imgPath, path.extname(imgPath)),
      buffer: buf,
      base64: buf.toString('base64'),
      sha256: sha256Hex(buf),
      byte_size: buf.length,
      width: dims.width,
      height: dims.height,
    });
    console.log(`Preflight: OK image ${imgPath} (${dims.width}x${dims.height}, ${buf.length} bytes)`);
  }

  return { version, tags, images };
}

/**
 * The per-rep-cycle throwaway warm-up must NOT use one of the eval images (doing so would cache
 * that exact image and destroy the next "cold" measurement of it). Default to a tiny in-process
 * generated PNG; --warmup-image lets the caller supply a real file instead.
 */
function loadWarmupImage(args) {
  if (args.warmupImage) {
    const imgPath = args.warmupImage;
    if (!fs.existsSync(imgPath)) throw new BenchError(`--warmup-image not found: ${imgPath}`);
    const stat = fs.statSync(imgPath);
    if (stat.size === 0) throw new BenchError(`--warmup-image is zero bytes: ${imgPath}`);
    const buf = fs.readFileSync(imgPath);
    if (!(buf.length >= 4 && buf[0] === 0x89 && buf[1] === 0x50 && buf[2] === 0x4e && buf[3] === 0x47)) {
      throw new BenchError(`--warmup-image does not start with PNG magic bytes (89 50 4E 47): ${imgPath}`);
    }
    const dims = readPngDimensions(buf);
    console.log(`Warm-up image: ${imgPath} (${dims.width}x${dims.height}, ${buf.length} bytes)`);
    return {
      path: imgPath,
      basename: `warmup-${path.basename(imgPath, path.extname(imgPath))}`,
      buffer: buf,
      base64: buf.toString('base64'),
      sha256: sha256Hex(buf),
      byte_size: buf.length,
      width: dims.width,
      height: dims.height,
      synthetic: false,
    };
  }
  const buf = makeSyntheticPng(64, 64, [128, 128, 128]);
  const dims = readPngDimensions(buf);
  console.log(`Warm-up image: synthetic in-process ${dims.width}x${dims.height} PNG (${buf.length} bytes)`);
  return {
    path: '(synthetic 64x64 gray PNG, generated in-process — not written to disk)',
    basename: 'warmup-synthetic',
    buffer: buf,
    base64: buf.toString('base64'),
    sha256: sha256Hex(buf),
    byte_size: buf.length,
    width: dims.width,
    height: dims.height,
    synthetic: true,
  };
}

// ---------------------------------------------------------------------------
// Gate mode
// ---------------------------------------------------------------------------

async function runGate(ctx) {
  const { args, outDir, images, promptText, options } = ctx;
  const model = args.models[0];
  const image = images[0];
  console.log(`\n=== GATE mode: model=${model} image=${image.basename} ===`);

  const ab = await imageTokenAB({ model, promptText, image, options, traceGpu: true });

  writeRunJson(outDir, buildRunRecord({ model, image, rep: 0, warmup: false, tag: 'ab-noimage', result: ab.noImgResult, options, flags: ab.noImgFlags }));
  writeRunJson(outDir, buildRunRecord({ model, image, rep: 0, warmup: false, tag: 'ab-withimage', result: ab.withImgResult, options, flags: ab.withImgFlags }));

  fs.writeFileSync(
    path.join(outDir, 'image-token-ab.json'),
    JSON.stringify(
      {
        model,
        image: image.path,
        no_image_prompt_eval_count: ab.no_image_prompt_eval_count,
        with_image_prompt_eval_count: ab.with_image_prompt_eval_count,
        image_tokens: ab.image_tokens,
        reason: ab.reason,
        cache_warning: ab.cache_warning,
      },
      null,
      2,
    ),
  );

  console.log(
    `[gate] no-image prompt_eval_count=${ab.no_image_prompt_eval_count} with-image prompt_eval_count=${ab.with_image_prompt_eval_count} image_tokens=${
      ab.image_tokens === null ? `NULL (${ab.reason})` : ab.image_tokens
    }`,
  );
  console.log(
    `[gate] with-image: ttft_ms=${fmtNum(ab.withImgResult.ttftMs, 1)} total_ms=${fmtNum(nsToMs(ab.withImgResult.finalObj.total_duration), 1)} done_reason=${ab.withImgResult.finalObj.done_reason}`,
  );
  console.log('[gate] --- answer (with image) ---');
  console.log(ab.withImgResult.answer);
  console.log('[gate] --- end answer ---');

  console.log(`[gate] unloading ${model} ...`);
  const unloaded = await unloadModel(model);
  if (!unloaded) {
    console.warn(`WARNING: [gate] ${model} did not report unloaded within ${UNLOAD_POLL_TIMEOUT_MS}ms (harness is exiting anyway)`);
  }
  console.log('[gate] done.');
}

// ---------------------------------------------------------------------------
// Matrix mode
// ---------------------------------------------------------------------------

async function runMatrix(ctx) {
  const { args, outDir, images, warmupImage, promptText, options, warmupOptions } = ctx;

  fs.writeFileSync(path.join(outDir, 'runs.csv'), RUNS_CSV_HEADER + '\n');
  fs.writeFileSync(path.join(outDir, 'vram.csv'), VRAM_CSV_HEADER + '\n');

  const firstModel = args.models[0];
  console.log(`\n=== Image-token A/B test (model=${firstModel}) ===`);
  const ab = await imageTokenAB({ model: firstModel, promptText, image: images[0], options, traceGpu: false });
  writeRunJson(outDir, buildRunRecord({ model: firstModel, image: images[0], rep: 0, warmup: false, tag: 'ab-noimage', result: ab.noImgResult, options, flags: ab.noImgFlags }));
  writeRunJson(outDir, buildRunRecord({ model: firstModel, image: images[0], rep: 0, warmup: false, tag: 'ab-withimage', result: ab.withImgResult, options, flags: ab.withImgFlags }));
  fs.writeFileSync(
    path.join(outDir, 'image-token-ab.json'),
    JSON.stringify(
      {
        model: firstModel,
        image: images[0].path,
        no_image_prompt_eval_count: ab.no_image_prompt_eval_count,
        with_image_prompt_eval_count: ab.with_image_prompt_eval_count,
        image_tokens: ab.image_tokens,
        reason: ab.reason,
        cache_warning: ab.cache_warning,
      },
      null,
      2,
    ),
  );
  console.log(
    `[ab] no-image prompt_eval_count=${ab.no_image_prompt_eval_count} with-image=${ab.with_image_prompt_eval_count} image_tokens=${
      ab.image_tokens === null ? `NULL (${ab.reason})` : ab.image_tokens
    }`,
  );

  console.log(`[ab] unloading ${firstModel} to get a clean baseline before the matrix starts ...`);
  const abUnloaded = await unloadModel(firstModel);
  if (!abUnloaded) {
    throw new BenchError(`${firstModel} did not unload within ${UNLOAD_POLL_TIMEOUT_MS}ms after the image-token A/B test`);
  }

  const vramRows = [];
  const summaryData = [];
  const answersByImage = new Map();

  for (const model of args.models) {
    console.log(`\n=== Model: ${model} ===`);

    const baselineMib = await sampleVramMedianMib();
    console.log(`[${model}] baseline VRAM: ${baselineMib} MiB`);

    const perImageStats = new Map(images.map((img) => [img.path, { ttfts: [], totals: [], decodeTokS: [], hashes: [], cacheContaminated: [] }]));
    const coldLoadSamples = [];
    let residentMib = null;
    let ollamaSizeVramMib = null;

    // Cache-correctness protocol (see the note at the top of this file): Ollama's prompt cache
    // survives across calls to the same loaded model, so plain alternation between images does
    // NOT produce honest cold prefill numbers once both images have been seen once. Each
    // rep-cycle therefore: (a) explicitly unloads the model, (b) does a throwaway warm-up
    // generate against a small SYNTHETIC image (never an eval image) to pay for the load and
    // kernel warm-up once, discarding its answer/timings but keeping load_duration, then
    // (c) measures every real eval image exactly once, cold.
    for (let rep = 1; rep <= args.reps; rep++) {
      const cycleLabel = `rep-cycle ${rep}/${args.reps}`;

      if (args.noWarmup) {
        console.log(`[${model}] ${cycleLabel}: --no-warmup set, skipping unload+warm-up (timings from rep>1 may be cache-contaminated — watch the warnings)`);
      } else {
        console.log(`[${model}] ${cycleLabel}: unloading to clear the prompt cache before warm-up ...`);
        const preUnloaded = await unloadModel(model);
        if (!preUnloaded) {
          throw new BenchError(`[${model}] ${cycleLabel}: model did not report unloaded within ${UNLOAD_POLL_TIMEOUT_MS}ms before warm-up`);
        }

        console.log(`[${model}] ${cycleLabel}: throwaway warm-up against ${warmupImage.basename} (discarding its answer/timings, keeping load_duration) ...`);
        const traceThisWarmup = args.gpuTrace && rep === 1; // trace only once per model, not every cycle
        const warmResult = await generateOnce({ model, promptText, imageBase64: warmupImage.base64, options: warmupOptions, traceGpu: traceThisWarmup });
        const warmFlags = validateRun(warmResult, { label: `${model}/${cycleLabel}/warmup`, expectTruncation: true });
        const cycleLoadMs = nsToMs(warmResult.finalObj.load_duration);
        coldLoadSamples.push(cycleLoadMs);
        writeRunJson(outDir, buildRunRecord({ model, image: warmupImage, rep, warmup: true, result: warmResult, options: warmupOptions, flags: warmFlags }));
        console.log(`[${model}] ${cycleLabel}: warm-up ttft=${fmtNum(warmResult.ttftMs, 1)}ms load(cold)=${fmtNum(cycleLoadMs, 1)}ms`);

        if (rep === 1) {
          residentMib = await sampleVramMedianMib();
          const ps = await getPs();
          const psEntry = (ps.models || []).find((m) => m.name === model || m.model === model);
          ollamaSizeVramMib = psEntry ? bytesToMiB(psEntry.size_vram) : null;
          console.log(
            `[${model}] resident VRAM: ${residentMib} MiB (delta ${residentMib - baselineMib} MiB), ollama-reported size_vram=${ollamaSizeVramMib ?? 'n/a'} MiB`,
          );
        }
      }

      for (const image of images) {
        console.log(`[${model}] [${image.basename}] rep ${rep}/${args.reps} (cold) ...`);
        const result = await generateOnce({ model, promptText, imageBase64: image.base64, options, traceGpu: false });
        const flags = validateRun(result, { label: `${model}/${image.basename}/rep${rep}` });
        const rec = buildRunRecord({ model, image, rep, warmup: false, result, options, flags });
        writeRunJson(outDir, rec);
        appendRunsCsvRow(outDir, rec);

        const stats = perImageStats.get(image.path);
        stats.ttfts.push(result.ttftMs);
        stats.totals.push(nsToMs(result.finalObj.total_duration));
        if (rec.derived_ms.decode_tok_s !== null) stats.decodeTokS.push(rec.derived_ms.decode_tok_s);
        stats.hashes.push(rec.answer_sha256);
        stats.cacheContaminated.push(flags.cache_contaminated);

        if (rep === 1) {
          if (!answersByImage.has(image.path)) answersByImage.set(image.path, []);
          answersByImage.get(image.path).push({ model, answer: result.answer, charCount: [...result.answer].length, identical: true });
        }

        console.log(
          `[${model}] [${image.basename}] rep ${rep}: ttft=${fmtNum(result.ttftMs, 1)}ms total=${fmtNum(nsToMs(result.finalObj.total_duration), 1)}ms prefill=${fmtNum(rec.derived_ms.prompt_eval_ms, 1)}ms decode=${
            rec.derived_ms.decode_tok_s !== null ? rec.derived_ms.decode_tok_s.toFixed(1) : 'n/a'
          }tok/s cached=${result.finalObj.prompt_eval_cached_count ?? 'n/a'}`,
        );
      }
    }

    if (residentMib === null) {
      // --no-warmup was set, so the model was never explicitly warmed (and resident VRAM never
      // sampled) before the measured loop above — the measured loop's first call necessarily
      // loaded it though, so sample resident VRAM/ps now, post-hoc.
      residentMib = await sampleVramMedianMib();
      const ps = await getPs();
      const psEntry = (ps.models || []).find((m) => m.name === model || m.model === model);
      ollamaSizeVramMib = psEntry ? bytesToMiB(psEntry.size_vram) : null;
      console.log(
        `[${model}] resident VRAM (sampled post-hoc, --no-warmup): ${residentMib} MiB (delta ${residentMib - baselineMib} MiB), ollama-reported size_vram=${ollamaSizeVramMib ?? 'n/a'} MiB`,
      );
    }

    const coldLoadMedianMs = coldLoadSamples.length ? median(coldLoadSamples) : null;

    for (const image of images) {
      const stats = perImageStats.get(image.path);
      const identical = stats.hashes.every((h) => h === stats.hashes[0]);
      const anyCacheContaminated = stats.cacheContaminated.some(Boolean);
      summaryData.push({
        model,
        image,
        ttfts: stats.ttfts,
        totals: stats.totals,
        decodeTokS: stats.decodeTokS,
        identical,
        anyCacheContaminated,
        coldLoadMs: coldLoadMedianMs,
        coldLoadSamples,
      });
      const entry = (answersByImage.get(image.path) || []).find((e) => e.model === model);
      if (entry) entry.identical = identical;
    }

    console.log(`[${model}] unloading ...`);
    const unloaded = await unloadModel(model);
    if (!unloaded) {
      throw new BenchError(`Model ${model} did not report unloaded within ${UNLOAD_POLL_TIMEOUT_MS}ms`);
    }

    const postUnloadMib = await sampleVramMedianMib();
    const returnedToBaseline = Math.abs(postUnloadMib - baselineMib) <= VRAM_TOLERANCE_MIB;
    console.log(`[${model}] post-unload VRAM: ${postUnloadMib} MiB (baseline was ${baselineMib} MiB) — returned_to_baseline=${returnedToBaseline}`);

    const vramRow = {
      model,
      baseline_mib: baselineMib,
      resident_mib: residentMib,
      delta_mib: residentMib - baselineMib,
      ollama_size_vram_mib: ollamaSizeVramMib,
      post_unload_mib: postUnloadMib,
      returned_to_baseline: returnedToBaseline,
    };
    vramRows.push(vramRow);
    appendVramCsvRow(outDir, vramRow);

    if (!returnedToBaseline) {
      throw new BenchError(
        `VRAM did not return to baseline after unloading ${model}: baseline=${baselineMib} MiB, post-unload=${postUnloadMib} MiB, tolerance=${VRAM_TOLERANCE_MIB} MiB. Aborting before the next model.`,
      );
    }
  }

  writeSummaryMd(outDir, { summaryData, vramRows, images, models: args.models });
  const blindKey = writeAnswersMd(outDir, { answersByImage, images });
  fs.writeFileSync(path.join(outDir, 'blind-key.json'), JSON.stringify(blindKey, null, 2));

  console.log(`\nDone. Output written to: ${outDir}`);
}

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

function requireVal(argv, i, flag) {
  if (i >= argv.length) throw new BenchError(`${flag} requires a value`);
  return argv[i];
}

function parseIntStrict(s, flag) {
  const n = Number(s);
  if (!Number.isFinite(n) || !Number.isInteger(n)) throw new BenchError(`${flag} expects an integer, got "${s}"`);
  return n;
}

function parseArgs(argv) {
  const args = {
    models: [],
    images: DEFAULT_IMAGES.slice(),
    reps: 3,
    numCtx: 8192,
    numPredict: 2048,
    promptFile: null,
    out: DEFAULT_OUT_DIR,
    gpuTrace: false,
    mode: 'matrix',
    noWarmup: false,
    warmupImage: null,
  };

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    switch (a) {
      case '--models':
        args.models = requireVal(argv, ++i, a).split(',').map((s) => s.trim()).filter(Boolean);
        break;
      case '--images':
        args.images = requireVal(argv, ++i, a).split(',').map((s) => s.trim()).filter(Boolean);
        break;
      case '--reps':
        args.reps = parseIntStrict(requireVal(argv, ++i, a), a);
        break;
      case '--num-ctx':
        args.numCtx = parseIntStrict(requireVal(argv, ++i, a), a);
        break;
      case '--num-predict':
        args.numPredict = parseIntStrict(requireVal(argv, ++i, a), a);
        break;
      case '--prompt-file':
        args.promptFile = path.resolve(requireVal(argv, ++i, a));
        break;
      case '--out':
        args.out = path.resolve(requireVal(argv, ++i, a));
        break;
      case '--gpu-trace':
        args.gpuTrace = true;
        break;
      case '--mode':
        args.mode = requireVal(argv, ++i, a);
        break;
      case '--no-warmup':
        args.noWarmup = true;
        break;
      case '--warmup-image':
        args.warmupImage = path.resolve(requireVal(argv, ++i, a));
        break;
      default:
        throw new BenchError(`Unknown argument: ${a}`);
    }
  }

  if (args.models.length === 0) {
    throw new BenchError('--models is required, e.g. --models qwen3-vl:4b-instruct-q4_K_M,qwen3-vl:8b-instruct-q4_K_M');
  }
  if (!['matrix', 'gate'].includes(args.mode)) {
    throw new BenchError(`--mode must be "matrix" or "gate", got "${args.mode}"`);
  }
  if (!(args.reps >= 1)) {
    throw new BenchError('--reps must be >= 1');
  }

  return args;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function run(args, outDir, startedAt) {
  const { version, tags, images } = await preflight(args);
  const warmupImage = loadWarmupImage(args);

  const gpuInfo = getGpuInfo();
  console.log(`GPU: ${gpuInfo.name} (driver ${gpuInfo.driver_version})`);

  const promptText = args.promptFile ? fs.readFileSync(args.promptFile, 'utf8') : DEFAULT_PROMPT;
  const promptSha256 = sha256Hex(promptText);
  const options = { num_ctx: args.numCtx, num_predict: args.numPredict, temperature: 0, seed: 42, top_p: 1 };
  const warmupOptions = { ...options, num_predict: WARMUP_NUM_PREDICT };

  const harnessSha256 = sha256Hex(fs.readFileSync(__filename));

  const manifest = buildManifest({ startedAt, version, tags, args, images, warmupImage, promptText, promptSha256, options, gpuInfo, harnessSha256 });
  fs.writeFileSync(path.join(outDir, 'manifest.json'), JSON.stringify(manifest, null, 2));
  console.log(`Wrote ${path.join(outDir, 'manifest.json')}`);

  const ctx = { args, outDir, images, warmupImage, promptText, promptSha256, options, warmupOptions, tags };

  if (args.mode === 'gate') {
    await runGate(ctx);
  } else {
    await runMatrix(ctx);
  }
}

async function main() {
  const startedAt = new Date();
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (e) {
    // We don't know --out yet if parsing itself failed early, so fall back to the default.
    const outDir = path.join(DEFAULT_OUT_DIR, `A2-${isoStampSafe(startedAt)}`);
    writeFailuresMd(outDir, [e.message || String(e)]);
    console.error(`\nFATAL: ${e.message || e}\n`);
    process.exit(1);
    return;
  }

  const outDir = path.join(args.out, `A2-${isoStampSafe(startedAt)}`);
  fs.mkdirSync(path.join(outDir, 'runs'), { recursive: true });
  console.log(`Output directory: ${outDir}`);

  try {
    await run(args, outDir, startedAt);
  } catch (e) {
    const msg = e instanceof BenchError ? e.message : e.stack || String(e);
    writeFailuresMd(outDir, [msg]);
    console.error(`\nFATAL: ${msg}\n`);
    process.exit(1);
    return;
  }

  process.exit(0);
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === __filename;
if (isMain) {
  main();
}
