#!/usr/bin/env node
// evals/fixcheck.mjs
//
// Borrow-checks the fixes a model proposes, instead of reading them and nodding.
//
// cases.md grades whether an answer identifies the bug. It never graded whether the
// *fix* works - and C1b found that gap matters: the answer A2 recorded as a PASS on
// case-02 reaches the right conclusion and then proposes a fix that fails with the
// identical E0502 it just explained. Two other plausible-looking fixes (`Vec::reserve`,
// `Vec::with_capacity`) fail the same way, because capacity is irrelevant to a static
// borrow conflict. None of that is visible by eye.
//
// Uses `rustc --emit=metadata`: it runs the borrow checker but never links, so this
// needs no MSVC linker and takes about a second per candidate.
//
//   node evals/fixcheck.mjs
//
// Add a candidate whenever a model proposes a fix worth holding to account.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

// The case-02 / case-01 fixture, reduced to the part the borrow conflict lives in.
// Kept in sync with evals/fixtures/frametest/src/main.rs by hand - it is 15 lines.
const HEAD = `use std::collections::HashMap;
struct CaptureFrame { width: u32, height: u32, pixels: Vec<u8> }
fn capture_screen(width: u32, height: u32) -> CaptureFrame {
    CaptureFrame { width, height, pixels: vec![0u8; (width*height*4) as usize] }
}
fn main() {
    let mut frames: Vec<CaptureFrame> = Vec::new();
    frames.push(capture_screen(1920, 1080));
`;
const TAIL = `    let mut cache: HashMap<String, u32> = HashMap::new();
    cache.insert("frames".to_string(), frames.len() as u32);
    println!("{:?}", cache);
}
`;

const CANDIDATES = [
  {
    name: 'baseline (the bug itself)',
    expect: 'reject',
    body: `    let first_buffer = &frames[0].pixels;
    frames.push(capture_screen(1920, 1080));
    println!("first frame has {} bytes", first_buffer.len());`,
  },
  {
    name: 'A2 PASS answer, "Option 1"',
    expect: 'reject',
    body: `    let first_frame = &frames[0];
    println!("w {} h {}", first_frame.width, first_frame.height);
    frames.push(capture_screen(1920, 1080));
    println!("first frame has {} bytes", first_frame.pixels.len());`,
  },
  {
    name: 'Vec::reserve before borrowing',
    expect: 'reject',
    body: `    frames.reserve(2);
    let first_buffer = &frames[0].pixels;
    frames.push(capture_screen(1920, 1080));
    println!("first frame has {} bytes", first_buffer.len());`,
  },
  {
    name: 'clone the buffer',
    expect: 'compile',
    body: `    let first_buffer = frames[0].pixels.clone();
    frames.push(capture_screen(1920, 1080));
    println!("first frame has {} bytes", first_buffer.len());`,
  },
  {
    name: 'read before the push (end the borrow)',
    expect: 'compile',
    body: `    let first_buffer = &frames[0].pixels;
    println!("first frame has {} bytes", first_buffer.len());
    frames.push(capture_screen(1920, 1080));`,
  },
];

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fixcheck-'));
let mismatches = 0;

for (const c of CANDIDATES) {
  const file = path.join(dir, 'candidate.rs');
  fs.writeFileSync(file, HEAD + c.body + '\n' + TAIL);

  let out = '';
  try {
    execFileSync('rustc', ['--edition', '2021', '--emit=metadata', '--out-dir', dir, file],
      { stdio: ['ignore', 'pipe', 'pipe'] });
  } catch (e) {
    out = String(e.stderr ?? '');
  }

  const rejected = /^error/m.test(out);
  const code = (/E\d{4}/.exec(out) ?? ['-'])[0];
  const got = rejected ? 'reject' : 'compile';
  const ok = got === c.expect;
  if (!ok) mismatches++;

  console.log(
    (ok ? '  ok   ' : '  BAD  ') +
    c.name.padEnd(40) +
    (rejected ? 'REJECTED ' + code : 'COMPILES')
  );
}

fs.rmSync(dir, { recursive: true, force: true });
console.log('');
if (mismatches) throw new Error(mismatches + ' candidate(s) did not match expectation');
console.log('All candidates behaved as expected.');
