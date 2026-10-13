//! What the card actually holds.
//!
//! This is the second opinion. `ollama ps` reports what Ollama *believes* is
//! loaded; NVML reports what the GPU is really using. PLAN.md §6b requires the
//! state readout come from here, so that "Off" means the card genuinely gave the
//! memory back rather than that Ollama thinks it did.
//!
//! Measured live while planning this: with one model resident, the card reported
//! 9,116 MiB used while Ollama claimed 6,233 MiB for it. The difference is the
//! baseline plus everything else on the desktop — which is the whole argument
//! against a hard-coded baseline.
//!
//! NVML lives in exactly this file. "No NVIDIA GPU" then degrades in one place
//! and cannot leak into the rest of the program.

use std::time::Duration;

use nvml_wrapper::Nvml;

/// Median of three, as `evals/bench.mjs` does — the baseline drifts, and a
/// single sample catches whatever a compositor happened to be doing.
const SAMPLE_COUNT: usize = 3;

/// bench.mjs waits a full second between samples because each one spawns an
/// `nvidia-smi` process. NVML is an in-process call, so 250ms is enough to
/// average out driver jitter and keeps `warm` from growing four seconds of
/// sampling on top of a 3.7s load. Deliberate divergence from the reference.
const SAMPLE_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug)]
pub struct VramSample {
    pub used_mib: u64,
    pub total_mib: u64,
}

pub struct Card {
    nvml: Nvml,
    pub name: String,
}

impl Card {
    /// `Nvml::init` is the expensive part, so a command opens the card once and
    /// samples it repeatedly.
    ///
    /// `BIFROST_NVML_PATH` overrides the library location. It exists so the
    /// "readout unavailable" path can be exercised without touching the system.
    pub fn open() -> Result<Self, String> {
        let nvml = match std::env::var("BIFROST_NVML_PATH") {
            Ok(path) => Nvml::builder()
                .lib_path(path.as_ref())
                .init()
                .map_err(|e| format!("NVML at {path}: {e}"))?,
            Err(_) => Nvml::init().map_err(|e| format!("NVML: {e}"))?,
        };

        let name = nvml
            .device_by_index(0)
            .and_then(|d| d.name())
            .unwrap_or_else(|_| "unknown GPU".to_string());

        Ok(Self { nvml, name })
    }

    pub fn sample(&self) -> Result<VramSample, String> {
        let device = self
            .nvml
            .device_by_index(0)
            .map_err(|e| format!("no GPU at index 0: {e}"))?;

        let mut used = Vec::with_capacity(SAMPLE_COUNT);
        let mut total_mib = 0;

        for i in 0..SAMPLE_COUNT {
            let mem = device
                .memory_info()
                .map_err(|e| format!("could not read GPU memory: {e}"))?;
            used.push(mem.used / (1024 * 1024));
            total_mib = mem.total / (1024 * 1024);
            if i < SAMPLE_COUNT - 1 {
                std::thread::sleep(SAMPLE_DELAY);
            }
        }

        used.sort_unstable();
        Ok(VramSample {
            used_mib: used[used.len() / 2],
            total_mib,
        })
    }
}
