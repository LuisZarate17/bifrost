// Builds a practice folder for trying Mimir's file tools by hand.
//
//   node tools/practice.mjs          create it, or put it back the way it started
//   node tools/practice.mjs --path   print where it is, and change nothing
//
// docs/G-testing.md walks through it. Every file here has known contents, so each
// check has an exact right answer — "line 2 changes, nothing else" is only
// checkable when you know what line 2 said.
//
// Resetting deletes and rewrites the folder, so it only ever does that to a
// folder carrying its own marker file. Anything else at that path is left alone.

import fs from "node:fs";
import path from "node:path";

// Inside the repo, gitignored: it belongs to the project and goes when the project does.
const DIR = path.resolve(import.meta.dirname, "..", "practice");
const MARKER = ".mimir-practice";

// CRLF, like most files on this machine. Mimir keeps whatever endings a file
// has, and a practice file with Windows endings exercises the case that matters.
const crlf = (s) => s.replace(/\r?\n/g, "\r\n");

const FILES = {
  "greet.rs": `fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

fn main() {
    println!("{}", greet("world"));
}
`,
  "shopping.md": `# Shopping list

- apples
- bread
- milk
`,
  // A fact no model can know. An answer quoting it proves the file was read;
  // an answer without it after a Deny proves it was not.
  "facts.txt": `The practice code is 4417-AMBER.
`,
  // Makes greet.rs a project `cargo check` can build, so Mimir offers a check
  // here. The empty [workspace] makes this folder its own workspace: without
  // it, cargo finds Bifrost's Cargo.toml above and refuses to build.
  "Cargo.toml": `[package]
name = "greet"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "greet"
path = "greet.rs"

[workspace]
`,
};

// The smallest valid zip: an empty archive, just the end-of-directory record.
// It exists to be refused — Mimir takes images and text, nothing else.
const EMPTY_ZIP = Buffer.from([0x50, 0x4b, 0x05, 0x06, ...new Array(18).fill(0)]);

if (process.argv.includes("--path")) {
  console.log(DIR);
  process.exit(0);
}

// A folder made before the apps were renamed carries the old marker, and is
// still a practice folder.
const MARKERS = [MARKER, ".citadel-practice"];

if (fs.existsSync(DIR)) {
  if (!MARKERS.some((m) => fs.existsSync(path.join(DIR, m)))) {
    console.error(`${DIR} exists and is not a practice folder — leaving it alone.`);
    process.exit(1);
  }
  fs.rmSync(DIR, { recursive: true, force: true });
}

fs.mkdirSync(DIR);
fs.writeFileSync(path.join(DIR, MARKER), "Made by tools/practice.mjs. Safe to delete.\r\n");
for (const [name, text] of Object.entries(FILES)) {
  fs.writeFileSync(path.join(DIR, name), crlf(text));
}
fs.writeFileSync(path.join(DIR, "refuse-me.zip"), EMPTY_ZIP);

console.log(`Ready: ${DIR}`);
