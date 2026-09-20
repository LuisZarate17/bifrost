import { invoke } from "@tauri-apps/api/core";

/**
 * Something staged in the composer, not yet sent.
 *
 * Images and text files are handled differently on purpose. An image has to
 * reach the model as base64 on the message; a text file is just words, so it is
 * folded into the message content where any model can read it — including one
 * with no `vision` capability at all.
 */
export type Attachment =
  | { kind: "image"; name: string; ext: string; base64: string; bytes: number }
  | { kind: "text"; name: string; text: string; bytes: number };

/**
 * A 1080p screen costs ~2,042 image tokens, measured at A2.
 *
 * Every image is roughly that regardless of content — the model tiles it rather
 * than reading it — so a flat figure is closer to the truth than anything
 * derived from file size.
 */
export const IMAGE_TOKENS = 2042;

/** ~4 characters per token, the same crude rule the composer budgets with. */
export const textTokens = (s: string) => Math.ceil(s.length / 4);

export const attachmentTokens = (a: Attachment) =>
  a.kind === "image" ? IMAGE_TOKENS : textTokens(a.text);

const IMAGE_EXT = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/** Anything we are willing to read as words rather than bytes. */
const TEXT_EXT = [
  "txt", "md", "rs", "ts", "tsx", "js", "jsx", "json", "toml", "yaml", "yml",
  "css", "html", "py", "go", "java", "c", "h", "cpp", "hpp", "sh", "ps1",
  "sql", "xml", "ini", "cfg", "log", "csv",
];

/** Refused outright rather than truncated, so nothing is silently half-read. */
export const MAX_TEXT_BYTES = 256 * 1024;

const extOf = (name: string) => (name.split(".").pop() ?? "").toLowerCase();

function readAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    // The result is a data: URL; the model wants raw base64 with no prefix —
    // measured across 40+ requests at A2, and sending the prefix fails silently.
    r.onload = () => resolve(String(r.result).split(",")[1] ?? "");
    r.onerror = () => reject(new Error(`could not read ${file.name}`));
    r.readAsDataURL(file);
  });
}

function readAsText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(new Error(`could not read ${file.name}`));
    r.readAsText(file);
  });
}

/**
 * Turn dropped or chosen files into attachments.
 *
 * Unknown types are **refused with a reason** rather than guessed at. Reading an
 * arbitrary binary as text produces a wall of replacement characters that
 * silently eats the context budget, which is worse than being told no.
 */
export async function readFiles(
  files: FileList | File[],
): Promise<{ attachments: Attachment[]; rejected: string[] }> {
  const attachments: Attachment[] = [];
  const rejected: string[] = [];

  for (const file of Array.from(files)) {
    const ext = extOf(file.name);
    if (IMAGE_EXT.includes(ext)) {
      attachments.push({
        kind: "image",
        name: file.name,
        ext,
        base64: await readAsBase64(file),
        bytes: file.size,
      });
    } else if (TEXT_EXT.includes(ext)) {
      if (file.size > MAX_TEXT_BYTES) {
        rejected.push(`${file.name} — over ${MAX_TEXT_BYTES / 1024} KB`);
        continue;
      }
      attachments.push({
        kind: "text",
        name: file.name,
        text: await readAsText(file),
        bytes: file.size,
      });
    } else {
      rejected.push(`${file.name} — not an image or a text file`);
    }
  }
  return { attachments, rejected };
}

/** Persist the images of a message and return the names to record. */
export async function storeImages(attachments: Attachment[]): Promise<string[]> {
  const names: string[] = [];
  for (const a of attachments) {
    if (a.kind !== "image") continue;
    names.push(await invoke<string>("put_attachment", { data: a.base64, ext: a.ext }));
  }
  return names;
}

/** Load stored images back as base64, for re-sending a reopened conversation. */
export async function loadImages(names: string[]): Promise<string[]> {
  const out: string[] = [];
  for (const name of names) {
    try {
      out.push(await invoke<string>("get_attachment", { name }));
    } catch {
      // A missing file should cost that picture, not the conversation.
    }
  }
  return out;
}

/**
 * Fold text files into the message.
 *
 * Fenced and labelled, so the model can tell the file from the question — and so
 * it renders as a code block rather than running into the prose.
 */
export function foldText(question: string, attachments: Attachment[]): string {
  const texts = attachments.filter((a) => a.kind === "text");
  if (texts.length === 0) return question;
  const blocks = texts
    .map((a) => {
      const ext = extOf(a.name);
      return `\n\n\`\`\`${ext}\n// ${a.name}\n${a.kind === "text" ? a.text : ""}\n\`\`\``;
    })
    .join("");
  return `${question}${blocks}`;
}

export function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}
