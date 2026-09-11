# A3 — First Rust: capture a PNG — results

**Completed 2026-09-10.** Chunk A3 of [BUILD.md](../BUILD.md).

## What was built

`capture/` — a standalone CLI crate that captures the primary monitor via Windows Graphics
Capture and writes `out.png`. Optional first argument delays the capture by N seconds.

```
cargo run -- 12      # wait 12s, then capture
```

| | |
|---|---|
| Crate | `capture/`, edition 2024, root-level (not a workspace) |
| Dependencies | `windows-capture` 2.0.1 · `image` 0.25.10 |
| Output | 1920×1080 PNG, 8,294,400 raw bytes |

`frametest` stays standalone under `evals/fixtures/` — it is deliberately broken eval fixture
data, not a component, and must never compile.

---

## The prerequisite nobody knew was missing

**There was no MSVC linker on this machine.** No Visual Studio, no Build Tools, no Windows SDK,
no `link.exe` — confirmed via vswhere, the registry, and PATH. `rustup` had only
`stable-x86_64-pc-windows-msvc`, which needs a linker it did not have.

A1 recorded the toolchain as installed and verified. **It was neither compiled nor linked** —
A1 only ever verified Ollama. `evals/fixtures/frametest` fails at the *borrow check*, which
happens before the link step, so nothing ever exercised the linker and the gap stayed invisible
for two chunks.

Fixed with:

```
winget install --id Microsoft.VisualStudio.2022.BuildTools \
  --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Landed Build Tools 2022 17.14.37628.2, MSVC 14.44.35207, Windows SDK 10.0.26100.

**Lesson worth generalising: "installed" is not "verified."** A1 checked that Ollama produced
tokens but never that Rust produced a binary. The gate now used — a throwaway `cargo new` +
`cargo run` printing `Hello, world!` before touching any dependency — takes ten seconds and
proves compile *and* link independently of the crate under test.

---

## What the compiler taught

The chunk's stated purpose was learning, and the borrow checker was walked into deliberately
rather than avoided. Three attempts, each failing for a different and instructive reason.

**Attempt 1 — store the pixels in the struct.**

```rust
self.pixels = buffer.as_nopadding_buffer(&mut scratch);
```
```
error[E0308]: mismatched types — expected `Vec<u8>`, found `&[u8]`
```

`as_nopadding_buffer` returns a *view*, not data. The API refuses to copy 8.3 MB behind your
back. rustc suggested `.to_vec()` itself.

**Attempt 2 — keep the view instead, to avoid the copy.**

```rust
struct Capture<'a> { pixels: &'a [u8], .. }
```
```
error: lifetime may not live long enough
       assignment requires that `'1` must outlive `'a`
error[E0597]: `buffer` does not live long enough
```

`'a` is how long the struct lives — the whole capture session. `'1` is how long the frame
reference lives — one callback invocation. The assignment demands `'1` outlive `'a`, which is
backwards.

This is not pedantry. `Frame` holds `&ID3D11Device` / `&ID3D11DeviceContext` that the crate
**recreates per frame event**; the GPU texture is genuinely unmapped when the callback returns.
Keeping the pointer would read freed GPU memory. The crate blocks it twice over — `Frame` and
`FrameBuffer` carry explicit negative impls, `!Send` and `!Sync`, so routing the view through a
channel fails for a second, independent reason.

**Attempt 3 — copy out, and send.** `.to_vec()` ends the borrow and yields owned, `Send` data.
A channel carries it to `main`, because `start()` returns `Result<(), _>` and the framework
constructs the handler internally — there is no handler to read fields off afterwards.

This is the same shape as the bug in `evals/fixtures/frametest` (a reference into a `Vec` held
across a reallocating `push`) and exactly what BUILD.md predicts again at **B3**. Seen from both
directions now.

---

## Verification

| Check | Result |
|---|---|
| Toolchain gate — `cargo run` prints `Hello, world!` | **Pass.** Proves compile *and* link, no dependencies involved |
| `out.png` looks like the screen | **Pass.** Three captures: two of VS Code (different frames, 456 KB and 264 KB), one of fullscreen video |
| Dimensions | **1920×1080**, correct on every run |
| Not GDI — capture over hardware-accelerated fullscreen content | **Pass.** Fullscreen hardware-decoded YouTube video captured at full colour and sharpness, captions legible, **no black rectangle**. This is the case `BitBlt` fails, and the whole reason the crate was chosen |
| Live, not cached | Three captures produced three different frames and three different file sizes |

**Row padding turned out to be absent**, as predicted: 8,294,400 bytes is exactly
1920 × 1080 × 4, because `width * 4 = 7680` is already 256-byte aligned. That is a coincidence of
this resolution, not a guarantee — `as_nopadding_buffer` is still the right call, and `scratch`
simply stays empty here.

---

## Delegation tracking — does asking for subagents make a difference?

Recorded because the process itself is under test, not just the code.

### A3 used one agent

| | |
|---|---|
| Task | Research the `windows-capture` API |
| Model | Sonnet |
| Its own spend | **131,591 tokens**, 60 tool calls, 12.5 min |
| Returned into context | ~3,500 tokens |
| Compression | **~37×** |
| Corrections needed mid-flight | **None** |
| Did verification find problems? | **No.** Every claim held: 2.0.1, the trait-based API with no one-shot function, `as_nopadding_buffer` (not `as_raw_nopadding_buffer`), `ColorFormat::Rgba8` as default, `!Send`/`!Sync`, and the no-padding-at-1080p prediction confirmed to the byte |
| Counterfactual — needed anyway? | **Yes, and decisively** |

**It prevented a specific, likely failure.** `windows-capture` had a breaking rewrite in April
2026 (1.5.0 → 2.0.0). Any remembered or blog-sourced example is pre-2.0 and does not compile.
Without that research the overwhelmingly likely outcome was confident, non-compiling code
presented to someone learning Rust — the worst possible failure mode for this chunk. It also
found that `save_as_image` is built in (so the `image` crate is optional, not required) and that
`image`'s PNG encoder has **no `Bgra8` arm**, which would have been a silent trap had we not
requested `Rgba8`.

### Deliberately not delegated

| Not delegated | Reason |
|---|---|
| Writing the Rust | The chunk's product is the author's understanding. A subagent writing it destroys the deliverable |
| Explaining compiler errors | Same |
| The design/plan pass | Design space was small once research landed. A2's plan agent earned its keep because the measurement protocol had real failure modes; A3's did not |
| Installing Build Tools | One command needing admin; an agent adds a layer between the operator and the failure |

### The finding

**The highest-value discovery in A3 — the missing linker — came from direct environment
verification, not from any agent.** It was found by running vswhere, reading the registry, and
checking PATH during planning. No amount of delegation would have surfaced it, because it was a
fact about *this machine*, not about the world.

Compared with A2:

| Chunk | Agents | Their spend | Character of work | Delegation value |
|---|---|---|---|---|
| **A2** | 3 | ~310K+ tokens | Measurement, scripting, protocol design | **Very high** — a 1,346-line harness written and debugged outside the main context |
| **A3** | 1 | 131K tokens | Teaching, plus one research question | **Narrow but real** — research only |

**The hypothesis held.** Delegation value scales with how much *grunt work* a chunk contains, not
with its difficulty. A2 was measurement-heavy and delegation paid enormously. A3 was
teaching-heavy, and beyond the single research question there was nothing to delegate that would
not have damaged the outcome.

**Working rule going forward:** *delegate research and mechanical work; never delegate work whose
product is your own understanding; and verify the machine yourself, because agents reason about
the world, not about your box.*

One cost worth restating from A2: a cold agent cannot spot a bad premise. In A2 the harness agent
was handed a cache-mitigation assumption that was wrong and built against it until corrected
mid-flight. A3 had no such incident — the research task was self-contained, which is precisely
the kind of task that delegates safely.

---

## Notes and limitations

1. **`expect` everywhere.** Fine for a CLI where crashing is acceptable. Warden's popup must not
   panic because a frame did not arrive — this gets replaced around B3/C1.
2. **Single monitor assumed.** `Monitor::primary()` is correct for this machine's one display and
   matches PLAN.md §3, but it is an assumption baked into the code.
3. **The delay flag is a test affordance**, not a product feature. It exists so hardware-accelerated
   content can be brought to the foreground before capture.
4. **Elevation requirements unverified.** The crate's docs state nothing either way; capture worked
   unelevated here, which is all that has been established.
5. **No Blackwell-specific issues found**, but none ruled out either — a GitHub issue search
   returned zero results for RTX 50-series, which is absence of evidence rather than a clean bill.
6. **`out.png` is gitignored** (`.gitignore:22`). A full-screen capture of a real desktop can
   contain anything, same reasoning as `evals/shots/`.

---

## Next

**A4** — base64 the captured PNG, POST it to Ollama with the default prompt, stream the answer to
stdout. The pixels already arrive in `main` as owned data, which is exactly what A4 needs; the
remaining work is PNG-encoding in memory rather than to disk, then the HTTP call.
