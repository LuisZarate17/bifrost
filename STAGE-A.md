# Stage A — Execution Packet

**Paste this into a fresh chat.** Self-contained; it doesn't assume the planning conversation.

---

## Context

I'm building **Warden**: a private, fully local AI that screenshots my screen when I press a
hotkey and answers in a popup. Windows 11. This is Stage A — foundations only, no GUI yet.

**Machine (already verified, don't re-check):**
- RTX 5070 Ti, 16 GB VRAM, Blackwell (sm_120), driver 610.62
- Ryzen 7 9700X, 31 GB RAM, 509 GB free on C:
- One monitor, 1920×1080, 100% scaling
- Windows 11 Pro build 26200, not domain-joined, Windows Defender only
- Docker Desktop and Node 24 installed. **No Rust, no Ollama, no Python, no CUDA toolkit.**
- Repo root: `C:\Users\you\source\repos\warden`

**About me:** I've read about Rust but never written any. Explain what you're doing in A3/A4
rather than just handing me code — I want to actually learn this, not paste it.

**Hard rule for this project:** everything runs locally. No cloud API calls, ever, for any
reason. If a local model is bad at something the answer is a different local model, not a
fallback.

---

## A1 — Toolchain

Install `rustup` with the MSVC toolchain, and Ollama. Pull any small text model and confirm
inference works on the GPU.

**Done when:** `ollama run <model>` answers me, and I've seen tokens/sec confirming it's on the
5070 Ti and not the CPU.

**Notes:** Rust on Windows needs MSVC build tools — rustup will offer to install them, say yes.
Verify the GPU is actually being used, don't assume.

---

## A2 — The model bake-off — DONE 2026-09-08

> **Result: `qwen3-vl:8b-instruct-q4_K_M`.** Ran the full 2×2 rather than the pair below. Both 4B
> models read the code correctly and still called a file that doesn't compile "fine"; quantization
> changed time-to-first-token by under 2% and fixed no accuracy failure. Harness at
> `evals/bench.mjs`, write-up at [docs/A2-results.md](docs/A2-results.md). The packet below is
> kept as the brief that went in.

This is the most important chunk in Stage A. **Don't just pull one model.**

My screen is mostly **code and terminals**, so reading small dense text accurately is the entire
job. Benchmarks put Qwen3-VL-4B at 95.3 on DocVQA and 8B at 96.1 — under a point apart.

**Pull both** (tags verified against the Ollama registry 2026-09-08):
- `qwen3-vl:8b-instruct-q4_K_M` — 6.14 GB download
- `qwen3-vl:4b-instruct-q8_0` — 5.09 GB download

Use the **Instruct** builds, not the `-thinking-` ones: latency is the product here.

An earlier draft of this packet claimed 8B at Q4_K_M needs ~12 GB VRAM, and told you to weigh a
bigger-but-coarser model against a smaller-but-precise one. **That figure was wrong** — the two
downloads are about 1 GB apart and both fit a 16 GB card comfortably. Memory doesn't force this
decision. Pick on which one reads your screenshots correctly, and measure resident VRAM anyway,
since it runs higher than the download size once the KV cache and vision encoder load.

**Test both on my own screenshots** — a real editor with code, and a terminal with an error.
Send via `curl` to `/api/generate` with a base64 image.

**Record for each:** did it read the code correctly · time to first token · time to full answer ·
resident VRAM · image token count for a 1080p screen.

**Cap the context** well below the 256K default — I'm sending one screenshot, not a book, and
context is where VRAM goes.

**Done when:** I've picked one for a reason I can state.

**If both misread code:** don't compromise on accuracy. Try Gemma 3 (day-one Ollama support,
strong OCR, GUI grounding). A misread stack trace is worse than a slow answer.

---

## A3 — First Rust: capture a PNG — DONE 2026-09-10

> Built as `capture/` using `windows-capture` 2.0.1. Writes a 1920×1080 `out.png`, verified
> against fullscreen hardware-decoded video (no black frame — genuinely WGC, not GDI).
> See [docs/A3-results.md](docs/A3-results.md).
>
> **The MSVC build tools were missing** and had to be installed first — A1 recorded the toolchain
> as verified but only ever tested Ollama, and nothing had reached the link step. If repeating on
> a fresh machine, install `Microsoft.VisualStudio.2022.BuildTools` (VCTools workload) and prove
> it with a throwaway `cargo run` before adding any dependency.

`cargo new` a CLI in the repo. Use the [`windows-capture`](https://crates.io/crates/windows-capture)
crate (Windows Graphics Capture API). Capture the full screen and write `out.png`.

**Done when:** `out.png` on disk looks exactly like my screen.

**Important:** do **not** use GDI/BitBlt. It silently returns black frames on fullscreen and
hardware-accelerated content, which is exactly what I need this to handle later.

**Teach me as you go:** this is my first Rust. Explain cargo, `Result`, `?`, and what the
compiler is complaining about when it complains.

---

## A4 — First useful thing — DONE 2026-09-12

> Built into `capture/`. `cargo run` captures the screen and streams a real answer.
> Prompt in `capture/prompt.txt`, `--model` flag to swap models.
> See [docs/A4-results.md](docs/A4-results.md).
>
> **Gotchas found:** Smart App Control blocks Rust proc-macro DLLs (symptom: `can't find crate
> for <x>_derive` with the DLL present) — `cargo clean -p` that crate and rebuild. And `reqwest`
> 0.13 needs NASM on Windows unless you select `native-tls`.


Extend A3: base64 the captured PNG, POST it to Ollama with a default prompt, stream the answer
to stdout.

**Default prompt to start with** (it lives in a config file, not in source — I'll be rewriting
it constantly):

> What am I looking at? If there's an error or something broken, explain it and the fix.
> Otherwise explain what matters here.

**Done when:** I run one command and get a real, useful answer about my screen. No GUI.

**Note:** Ollama streams newline-delimited JSON, not one blob — parse it line by line with
`serde_json`.

---

## Done with Stage A when — ACHIEVED 2026-09-12

I can run one terminal command and get a useful answer about whatever is on my screen, and I
know which model I'm using and why.

**Do not start on Tauri, the hotkey, or the popup.** That's Stage B.
