//! Writing a run down, so it can be graded and argued with later.
//!
//! The timings print to the terminal and that is enough to compare speed. Picking
//! a *model* needs the answers themselves, read side by side — which is what this
//! writes.
//!
//! ## Why the answers are blinded
//!
//! Model identity is replaced with `Model A`, `Model B`, … and **shuffled
//! independently per image**, with the mapping written to a separate
//! `blind-key.json`.
//!
//! This is not ceremony. The eval set exists to catch a model being wrong in a way
//! you did not expect, and a grader who knows which answer came from the bigger
//! model will read it more generously — especially the grader who chose that model
//! last week. A2 picked the shipping model on exactly this kind of evidence.
//! Ported from `evals/bench.mjs:770-808`, where it was got right first.
//!
//! **There is no accuracy column here, and there must not be one.** Accuracy is a
//! human judgement about whether an answer is true; a column invites the harness
//! to imply it measured something it did not.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bifrost_core::ollama::Measured;

/// One measured repetition, kept whole so the report can say more than the
/// terminal did.
pub struct Run {
    pub model: String,
    pub image: String,
    pub prompt: String,
    pub rep: usize,
    pub measured: Measured,
    pub baseline_mib: Option<u64>,
    pub loaded_mib: Option<u64>,
}

pub struct Manifest {
    pub started: String,
    pub card: String,
    pub num_ctx: u32,
    pub reps: usize,
    pub models: Vec<String>,
    /// name, path, byte length, sha256
    pub images: Vec<(String, String, usize, String)>,
    /// name, sha256, full text
    pub prompts: Vec<(String, String, String)>,
}

/// A run's directory name. Sortable, and distinct from `bench.mjs`'s `A2-*`
/// directories so the two harnesses' output never gets confused.
pub fn stamp_dir(out_root: &str) -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Path::new(out_root).join(format!("F1-{secs}"))
}

// ── sha256, without a dependency ─────────────────────────────────────────────
//
// The manifest needs content hashes for the images and, far more importantly,
// for the prompts: at C1b a single trailing newline changed a case-02 verdict
// from correct to wrong at temperature 0. A manifest that records a prompt's
// name but not its bytes cannot tell you which of two such runs you are reading.
//
// FIPS 180-4. Small enough to be obviously right, and it keeps the crate at two
// dependencies.

pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (i, v) in [a, b, c, d, e, f, g, hh].iter().enumerate() {
            h[i] = h[i].wrapping_add(*v);
        }
    }

    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    sha256(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Fisher-Yates over indices, with the randomness drawn from a hash.
///
/// **The first version of this did not shuffle.** It seeded xorshift directly
/// from `SystemTime` nanoseconds and took bits off the high end of a multiply.
/// Windows' clock is coarse, so the low bits of that seed barely move between
/// runs while the high bits barely move within a session — and three consecutive
/// runs produced the identical permutation. A shuffle that quietly returns input
/// order is worse than no shuffle at all, because the output still claims to be
/// blind.
///
/// Hashing the clock fixes it by construction: every bit of the digest depends on
/// every bit of the input, so a nanosecond of movement redraws the whole stream.
/// `--selftest-shuffle` exists to keep it honest.
pub fn shuffled(n: usize) -> Vec<usize> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    shuffled_seeded(n, &format!("{nanos}"))
}

/// The seam the self-test drives, so the shuffle can be checked without spending
/// a minute of GPU per sample.
pub fn shuffled_seeded(n: usize, seed: &str) -> Vec<usize> {
    let digest = sha256(seed.as_bytes());
    let mut idx: Vec<usize> = (0..n).collect();
    for i in (1..idx.len()).rev() {
        let b = digest[i % digest.len()] as usize;
        idx.swap(i, b % (i + 1));
    }
    idx
}

fn label(n: usize) -> String {
    // A..Z, then AA, AB — more models than that and the grading is the problem.
    let mut s = String::new();
    let mut n = n;
    loop {
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn write_file(dir: &Path, name: &str, body: &str) -> Result<(), String> {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write_manifest(dir: &Path, m: &Manifest) -> Result<(), String> {
    let mut s = String::from("{\n");
    let _ = writeln!(s, "  \"started\": \"{}\",", json_escape(&m.started));
    let _ = writeln!(s, "  \"card\": \"{}\",", json_escape(&m.card));
    let _ = writeln!(s, "  \"num_ctx\": {},", m.num_ctx);
    let _ = writeln!(s, "  \"reps\": {},", m.reps);

    let models: Vec<String> = m.models.iter().map(|x| format!("\"{}\"", json_escape(x))).collect();
    let _ = writeln!(s, "  \"models\": [{}],", models.join(", "));

    s.push_str("  \"images\": [\n");
    for (i, (name, path, bytes, sha)) in m.images.iter().enumerate() {
        let _ = writeln!(
            s,
            "    {{ \"name\": \"{}\", \"path\": \"{}\", \"bytes\": {}, \"sha256\": \"{}\" }}{}",
            json_escape(name),
            json_escape(path),
            bytes,
            sha,
            if i + 1 < m.images.len() { "," } else { "" }
        );
    }
    s.push_str("  ],\n");

    // The prompt's full text and its hash. C1b: one trailing newline flipped a
    // verdict, so the name alone identifies nothing.
    s.push_str("  \"prompts\": [\n");
    for (i, (name, sha, text)) in m.prompts.iter().enumerate() {
        let _ = writeln!(
            s,
            "    {{ \"name\": \"{}\", \"sha256\": \"{}\", \"text\": \"{}\" }}{}",
            json_escape(name),
            sha,
            json_escape(text),
            if i + 1 < m.prompts.len() { "," } else { "" }
        );
    }
    s.push_str("  ]\n}\n");

    write_file(dir, "manifest.json", &s)
}

pub fn write_run(dir: &Path, r: &Run) -> Result<(), String> {
    let m = &r.measured;
    let mut s = String::from("{\n");
    let _ = writeln!(s, "  \"model\": \"{}\",", json_escape(&r.model));
    let _ = writeln!(s, "  \"image\": \"{}\",", json_escape(&r.image));
    let _ = writeln!(s, "  \"prompt\": \"{}\",", json_escape(&r.prompt));
    let _ = writeln!(s, "  \"rep\": {},", r.rep);
    let _ = writeln!(s, "  \"ttft_ms\": {:.1},", m.ttft.as_secs_f64() * 1000.0);
    let _ = writeln!(
        s,
        "  \"prefill_ms\": {:.1},",
        (m.ttft.as_secs_f64() - m.load_ns as f64 / 1e9).max(0.0) * 1000.0
    );
    let _ = writeln!(s, "  \"total_s\": {:.3},", m.total.as_secs_f64());
    let _ = writeln!(s, "  \"decode_tps\": {:.1},", m.decode_tps());
    let _ = writeln!(s, "  \"prompt_eval_count\": {},", m.prompt_eval_count);
    let _ = writeln!(s, "  \"eval_count\": {},", m.eval_count);
    let _ = writeln!(s, "  \"load_ns\": {},", m.load_ns);
    let _ = writeln!(s, "  \"done_reason\": \"{}\",", json_escape(&m.done_reason));
    let _ = writeln!(
        s,
        "  \"baseline_mib\": {},",
        r.baseline_mib.map(|v| v.to_string()).unwrap_or("null".into())
    );
    let _ = writeln!(
        s,
        "  \"loaded_mib\": {},",
        r.loaded_mib.map(|v| v.to_string()).unwrap_or("null".into())
    );
    let _ = writeln!(
        s,
        "  \"delta_mib\": {},",
        match (r.baseline_mib, r.loaded_mib) {
            (Some(b), Some(l)) => l.saturating_sub(b).to_string(),
            _ => "null".into(),
        }
    );
    let _ = writeln!(s, "  \"answer_chars\": {},", m.answer.len());
    let _ = writeln!(s, "  \"answer_sha256\": \"{}\",", sha256_hex(m.answer.as_bytes()));
    let _ = write!(s, "  \"answer\": \"{}\"\n}}\n", json_escape(&m.answer));

    let safe = |x: &str| x.replace([':', '/', '\\', ' '], "_");
    let name = format!(
        "runs/{}__{}__{}__rep{}.json",
        safe(&r.model),
        safe(&r.image),
        safe(&r.prompt),
        r.rep
    );
    write_file(dir, &name, &s)
}

/// The grading surface, and the key that unblinds it.
///
/// Returns the blind key so the caller can report where it went.
pub fn write_answers(
    dir: &Path,
    runs: &[Run],
    images: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let mut md = String::new();
    md.push_str("# Blind answer grading\n\n");
    md.push_str(
        "Grouped by image. **Model identity is blinded and shuffled independently per image** — \
         see `blind-key.json` to unblind, after grading rather than before.\n\n\
         There is deliberately **no accuracy column**, here or in `summary.md`. Whether an answer \
         is *true* is a judgement about the screen it describes, and a column would imply the \
         harness measured something it did not.\n\n",
    );

    let mut key: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    for image in images {
        // One entry per model for this image: the first rep's answer, plus
        // whether the other reps agreed with it.
        let mut by_model: BTreeMap<String, Vec<&Run>> = BTreeMap::new();
        for r in runs.iter().filter(|r| &r.image == image) {
            by_model.entry(r.model.clone()).or_default().push(r);
        }
        if by_model.is_empty() {
            continue;
        }

        let models: Vec<String> = by_model.keys().cloned().collect();
        let order = shuffled(models.len());

        let _ = write!(md, "## Image: {image}\n\n");
        let mut per_image: BTreeMap<String, String> = BTreeMap::new();

        for (position, &original) in order.iter().enumerate() {
            let model = &models[original];
            let reps = &by_model[model];
            let lbl = label(position);
            per_image.insert(format!("Model {lbl}"), model.clone());

            let first = reps[0];
            let identical = reps.iter().all(|r| r.measured.answer == first.measured.answer);

            let _ = write!(md, "### Model {lbl}\n\n");
            let _ = writeln!(md, "- reps: {}", reps.len());
            let _ = writeln!(md, "- output_chars: {}", first.measured.answer.len());
            if reps.len() > 1 {
                // A model that answers differently run to run is telling you its
                // verdict is not stable, which matters more than the verdict.
                let _ = writeln!(
                    md,
                    "- identical across reps: {}",
                    if identical { "yes" } else { "**NO**" }
                );
            }
            md.push('\n');
            md.push_str(&first.measured.answer);
            md.push_str("\n\n---\n\n");
        }

        key.insert(image.clone(), per_image);
    }

    write_file(dir, "answers.md", &md)?;

    let mut j = String::from("{\n");
    for (i, (image, labels)) in key.iter().enumerate() {
        let _ = writeln!(j, "  \"{}\": {{", json_escape(image));
        for (n, (lbl, model)) in labels.iter().enumerate() {
            let _ = writeln!(
                j,
                "    \"{}\": \"{}\"{}",
                json_escape(lbl),
                json_escape(model),
                if n + 1 < labels.len() { "," } else { "" }
            );
        }
        let _ = writeln!(j, "  }}{}", if i + 1 < key.len() { "," } else { "" });
    }
    j.push_str("}\n");
    write_file(dir, "blind-key.json", &j)?;

    Ok(key)
}

/// The timing table, saved.
///
/// **Not blinded** — this is the speed sheet, and a time-to-first-token says
/// nothing about whether the answer was right. Blinding it would only make the
/// numbers harder to act on.
pub fn write_summary(dir: &Path, table: &str) -> Result<(), String> {
    let mut md = String::new();
    md.push_str("# Summary\n\n");
    md.push_str(
        "Timings and VRAM, by model. **Not blinded** — speed is not the thing a grader can talk \
         themselves into. For the answers, see `answers.md`, which is.\n\n\
         **TTFT** is what pressing the hotkey feels like, cold load included. **PREFILL** takes \
         that load back out: what the model costs once resident, and the only column comparable \
         with A2's table.\n\n```\n",
    );
    md.push_str(table);
    md.push_str("```\n");
    write_file(dir, "summary.md", &md)
}
