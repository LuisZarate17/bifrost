# A1 — Toolchain: results

**Completed 2026-09-07.** Chunk A1 of [BUILD.md](../BUILD.md).

## Installed

| | |
|---|---|
| Rust | `rustc 1.98.1`, host `x86_64-pc-windows-msvc` |
| Ollama | `0.33.3` |
| Test model | `llama3.1:8b` (4.9 GB, Q4) — text-only, big enough that a GPU/CPU gap is unambiguous |

Both `cargo` and `ollama` are on the user PATH.

## GPU verification — passed

| Signal | Result |
|---|---|
| `ollama ps` PROCESSOR | **100% GPU** |
| GPU process list | `lib\ollama\llama-server.exe` present |
| `nvidia-smi` | 6147 MiB / 16303 MiB, 96% utilization |
| Eval rate, warm | **152.09 tok/s** |
| Eval rate, on CPU (before the fix below) | 12.74 tok/s |

**12× speedup.** Not a rounding difference.

## Cold vs warm — the number that matters for this project

Same prompt, same model:

| | Cold | Warm |
|---|---|---|
| Load duration | **17.94 s** | 0.002 s |
| Total | **32.85 s** | **0.70 s** |

This is the `keep_alive: -1` decision validated with a real measurement, and the stall is worse
than the plan assumed. An 18-second load is exactly the "first press after lunch" failure that
would stop the habit forming. **Do not ship without the model held resident.**

Also note: first-run numbers are meaningless on Blackwell — kernel warmup made the cold run
report a prompt eval rate of 1.55 tok/s, versus 90.76 tok/s warm. **Always measure warm.**

## What went wrong, and the fix

Ollama installed and ran, but inference was **100% CPU** at 12.74 tok/s. Its log went straight
from `discovering available GPUs` to registering CPU as the only device, never attempting CUDA —
with an irrelevant "AMD driver is too old" warning in between, triggered by the 9700X's
integrated graphics.

Cause: **the installer had silently failed to extract its CUDA backend.** Missing from
`lib\ollama\cuda_v12`:

- `ggml-cuda.dll` (335 MB) — the actual GPU inference backend
- `cublasLt64_12.dll` (660 MB)
- `cudart64_12.dll`

A half-written `is-V84GKFBU68.tmp` (660 MB) was left behind — Inno Setup's naming for a file
caught mid-extraction. The NVIDIA driver was fine throughout; `nvidia-smi` reported the card
correctly the whole time.

**Fix:** delete `lib\ollama\cuda_v12` entirely, download the current installer fresh
(1.5 GB from `https://ollama.com/download/OllamaSetup.exe`), run it with
`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`. All four libraries landed clean, install tree 2.8 GB.

## Gotchas — these cost real time

1. **`ollama app.exe` opens a first-run GUI window.** It blocks forever in a non-interactive
   shell. This killed two automated attempts. Start the server with
   `ollama.exe serve` detached instead, never the tray app.
2. **A broken CUDA install fails silently.** Ollama does not error — it just runs on CPU. If
   inference feels slow, check that `ggml-cuda.dll` exists before debugging anything else.
3. **`Start-Process -Wait` on the installer never returns**, because it waits on the tray app
   the installer launches at the end. The install itself completes fine; only the wait hangs.
   Verify by checking for the DLLs, not by trusting the exit code.
4. **Windows Defender real-time scanning is on** and is the plausible cause of the interrupted
   660 MB extraction. Worth suspecting if it recurs.

## Known gap

**Ollama has no autostart entry** — it will not come back after a reboot. Start it manually
with `ollama serve` for now. C4 assumes the model is warm at login, so this needs solving
before then.
