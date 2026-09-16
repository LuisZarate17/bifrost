//! `warm`, `off`, `status` — the CLI face of power control.
//!
//! The model costs ~8 GB of a 16 GB card while resident (A2). Holding it there
//! permanently is wrong when the popup has to work over games wanting 8–12 GB,
//! and a timeout is wrong too: it decides for you, and leaves you guessing
//! whether the next press is instant or a stall. So: states you set, and can see.
//!
//! **Every number here is measured against a sample taken moments earlier, never
//! against a constant.** PLAN.md §6b: the idle baseline was 667, 1033 and 945 MiB
//! at different points in a single session. "Sample it fresh; never hard-code it."
//! `warm` and `off` therefore each measure their own before/after delta, which
//! makes them immune to whatever else is holding video memory.

use warden_core::ollama::{self, LoadedModel};
use warden_core::vram::Card;

/// Formats a signed MiB delta the way a human reads it.
fn delta(before: u64, after: u64) -> String {
    if after >= before {
        format!("+{} MiB", after - before)
    } else {
        format!("-{} MiB", before - after)
    }
}

/// Open the card, or explain why we cannot.
///
/// A missing NVML is not fatal to the *operation* — loading and unloading are
/// Ollama calls. It is only fatal to the verification, which the caller reports.
fn card() -> Result<Card, String> {
    Card::open()
}

fn print_card_line(card: &Result<Card, String>, sample: Option<u64>, total: Option<u64>) {
    match (card, sample, total) {
        (Ok(c), Some(used), Some(total)) => {
            println!("card    {used} MiB used of {total}  ({})", c.name);
        }
        (Err(why), _, _) => {
            println!("card    unavailable ({why})");
        }
        _ => println!("card    unavailable"),
    }
}

/// Ollama returns an RFC3339 timestamp. With `keep_alive: -1` it is pushed far
/// into the future; on the 5-minute default it is minutes away. Comparing years
/// is crude but needs no date library, and the exact value is printed anyway.
fn pinned(expires_at: &str) -> bool {
    expires_at
        .get(0..4)
        .and_then(|y| y.parse::<i32>().ok())
        .is_some_and(|year| year >= 2100)
}

fn describe(model: &LoadedModel) -> String {
    let vram = model
        .size_vram_mib()
        .map(|m| format!("size_vram {m} MiB"))
        .unwrap_or_else(|| "size_vram unknown".to_string());

    match model.expires_at.as_deref() {
        Some(at) if pinned(at) => format!("resident, {vram}, pinned (keep_alive -1)"),
        Some(at) => format!("resident, {vram}, expires {at}"),
        None => format!("resident, {vram}"),
    }
}

pub fn status(tag: &str) -> Result<(), String> {
    let loaded = ollama::ps()?;
    let found = ollama::find(&loaded, tag);
    let card = card();
    let sample = card.as_ref().ok().and_then(|c| c.sample().ok());

    println!("model   {tag}");
    println!("state   {}", if found.is_some() { "Ready" } else { "Off" });
    match found {
        Some(m) => println!("ollama  {}", describe(m)),
        None => println!("ollama  not loaded"),
    }
    print_card_line(&card, sample.map(|s| s.used_mib), sample.map(|s| s.total_mib));

    // Ollama's figure counts only its own runner; the card counts everything.
    // So the card should always read higher — never equal. Measured live during
    // planning: 9,116 MiB on the card against Ollama's 6,233 MiB claim.
    if let (Some(m), Some(s)) = (found, sample) {
        if let Some(claim) = m.size_vram_mib() {
            if s.used_mib < claim {
                println!(
                    "note    card reads below Ollama's claim ({} < {claim}) — unexpected",
                    s.used_mib
                );
            }
        }
    }

    if card.is_err() {
        // Still exit 0: the question was answered, just with one source missing.
        println!("note    falling back to Ollama's claim, which PLAN.md §6b warns is not proof");
    }
    Ok(())
}

pub fn warm(tag: &str, num_ctx: u32) -> Result<(), String> {
    if ollama::find(&ollama::ps()?, tag).is_some() {
        println!("already resident — nothing to do");
        return status(tag);
    }

    let card = card();
    let before = card.as_ref().ok().and_then(|c| c.sample().ok());

    println!("warming {tag}...");
    let elapsed = ollama::load(tag, num_ctx)?;

    let after = card.as_ref().ok().and_then(|c| c.sample().ok());

    match (before, after) {
        (Some(b), Some(a)) => {
            println!("card    {} MiB -> {} MiB", b.used_mib, a.used_mib);
            println!(
                "delta   {}   in {:.1}s",
                delta(b.used_mib, a.used_mib),
                elapsed.as_secs_f64()
            );
        }
        _ => {
            println!("loaded in {:.1}s", elapsed.as_secs_f64());
            print_card_line(&card, None, None);
        }
    }

    println!("state   Ready (pinned, keep_alive -1)");
    if card.is_err() {
        return Err("loaded, but the VRAM delta could not be verified".to_string());
    }
    Ok(())
}

pub fn off(tag: &str) -> Result<(), String> {
    if ollama::find(&ollama::ps()?, tag).is_none() {
        println!("not loaded — nothing to do");
        return Ok(());
    }

    let card = card();
    let before = card.as_ref().ok().and_then(|c| c.sample().ok());

    println!("releasing {tag}...");
    let confirmed = ollama::unload(tag)?;

    let after = card.as_ref().ok().and_then(|c| c.sample().ok());

    match (before, after) {
        (Some(b), Some(a)) => {
            println!("card    {} MiB -> {} MiB", b.used_mib, a.used_mib);
            println!("delta   {}", delta(b.used_mib, a.used_mib));
        }
        _ => print_card_line(&card, None, None),
    }

    if !confirmed {
        return Err("Ollama still lists the model after 30s".to_string());
    }
    println!("state   Off (unload confirmed by /api/ps)");

    if card.is_err() {
        return Err("unloaded, but the VRAM delta could not be verified".to_string());
    }
    Ok(())
}
