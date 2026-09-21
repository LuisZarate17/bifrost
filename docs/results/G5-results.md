# G5 — Images and text files into a message

Attachments: images as base64 on the message, text files folded into the question.
**Built 2026-09-19.**

## Measured

| | |
|---|---|
| A 51 KB PNG, base64-encoded | **68 KB** — the ~33% overhead, on every save |
| Stored and read back | **byte-identical** |
| That image sent to the model | answered *"Rust compilation error message"* |
| The prompt it cost | **2,065 tokens** |
| An image, flat | **~2,042 tokens**, whatever it shows |

**The token count is the proof the image arrived.** A model that never received it would still
answer — vaguely, from the text alone — and the reply would read fine. 2,065 prompt tokens for a
short question cannot be anything but the picture.

## An image costs the same whatever it shows

~2,042 tokens for a screenshot of code and ~2,042 for a photograph of a wall, because the model
**tiles it** rather than reading it. So the chip shows a flat figure, which is closer to the truth
than anything derived from file size.

The consequence is the packet's watch-for: **two images plus a little history is most of 32k.**
That is a budget you can exhaust in three messages without typing much, which is why the cost is on
the chip *before* you send rather than in the footer afterwards.

Sizing also settled G4's storage question: 51 KB of PNG becomes 68 KB of base64, and a 1080p
screenshot ~790 KB. Inlining that into the conversation JSON would rewrite the whole file every
turn — so the bytes go to `%APPDATA%\odin\attachments` and the record keeps the filename.

## Text files are not images and are not treated like them

They fold into the question as a **labelled code fence**. Which means they work on a model with no
`vision` capability at all — most of the useful cases, at no cost.

## Nothing but base64 crosses the boundary

The window sets `dragDropEnabled: false` so the **webview** handles drag and drop natively. The
frontend gets a browser `File` and reads it with `FileReader`.

No dialog plugin, no filesystem permission, and **no path from outside ever reaches Rust**. The
only thing crossing the IPC boundary is base64 — which is a meaningfully smaller attack surface
than a command that takes a path and opens it.

## Refused rather than guessed at

- **Unsupported types are refused with a reason.** Reading an arbitrary binary as text produces a
  wall of replacement characters that silently eats the context budget. Being told no is better.
- **Text files over 256 KB are refused, not truncated** — for the same reason G6 caps reads:
  half a file answered confidently is worse than being told it was too big.

## Deleting takes the attachments with it

Best effort, deliberately: **a file that will not delete should not block removing the
conversation.** The alternative is an undeletable conversation because one orphaned PNG is locked.

## Two bugs this chunk shipped

Both found by a code review the same day, both invisible to `tsc` and to the build:

- **The tools path built its own wire payload and dropped every image** — so turning tools on
  quietly made the model blind. Both paths share one `toWire()` now.
- **Trimming counted words and ignored the ~2,042 tokens per image**, so image-heavy threads never
  trimmed and Ollama truncated the prompt instead. That looks exactly like the model losing the
  plot, with nothing logged anywhere.

## What needed a human

Dragging a file in, the chip's figure, and the refusals. In
[PENDING-CHECKS.md](../PENDING-CHECKS.md).
