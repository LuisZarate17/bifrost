#!/usr/bin/env bash
# Try local models against Stage H's probes, one at a time, and leave nothing behind.
#
#   "C:\Program Files\Git\usr\bin\bash.exe" tools/model-trial.sh <out-dir> <tag> [<tag> ...]
#
# For each tag: pull it (unless it was already installed before the run — those
# are measured but never pulled or removed), resident VRAM at num_ctx 16384 and
# 32768 as the card reports it and as Ollama splits it between CPU and GPU, the
# tool-call probe (G0), the full task benchmark (H3, 16 tasks, 3 runs at the app's
# sampling plus 1 at temperature 0), then remove it.
#
# Written for H4 (docs/archive/results/H4-results.md). Use Git's bash, not `bash` on the
# PATH — on this machine that is WSL's. And start it detached (Start-Process): a
# run takes hours, longer than a tool's background task is allowed to live.
#
# Every step logs to <out-dir>/trial.log; each probe's full output goes beside it,
# and the log names the evals/out folder each benchmark saved its JSON to.
set -u
OUT=${1:?usage: model-trial.sh <out-dir> <tag> [<tag> ...]}
shift
REPO=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$OUT"
cd "$REPO"
log() { echo "$(date '+%F %T') $*" | tee -a "$OUT/trial.log"; }

# Installed before the run: measured, never pulled or removed.
PREEXISTING=$(ollama list 2>/dev/null | awk 'NR>1 {print $1}')

ensure_ollama() {
  curl -s -m 3 http://127.0.0.1:11434/api/tags >/dev/null && return
  log "Ollama was not running — starting it"
  (ollama serve >/dev/null 2>&1 &)
  for _ in $(seq 1 30); do sleep 1; curl -s -m 3 http://127.0.0.1:11434/api/tags >/dev/null && return; done
  log "Ollama did not start"
}

stop_all() {
  for m in $(ollama ps | awk 'NR>1 {print $1}'); do ollama stop "$m" >/dev/null 2>&1; done
  sleep 3
}

vram_at() { # tag ctx
  local tag=$1 ctx=$2 before after ps
  stop_all
  before=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' ')
  curl -s http://127.0.0.1:11434/api/generate \
    -d "{\"model\":\"$tag\",\"prompt\":\"\",\"keep_alive\":\"10m\",\"options\":{\"num_ctx\":$ctx}}" >/dev/null
  sleep 3
  after=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' ')
  ps=$(ollama ps | awk 'NR>1' | tr -s ' ')
  log "VRAM $tag ctx=$ctx: card ${before}->${after} MiB (+$((after - before))) | ollama ps: $ps"
  stop_all
}

ensure_ollama
log "=== trial of: $* (installed before: $(echo $PREEXISTING | tr '\n' ' '))"
for tag in "$@"; do
  safe=${tag//[:\/]/_}
  kept=no
  echo "$PREEXISTING" | grep -qx "$tag" && kept=yes

  if [ "$kept" = no ]; then
    log "=== $tag: pull"
    if ! ollama pull "$tag" > "$OUT/$safe-pull.txt" 2>&1; then log "pull FAILED for $tag"; continue; fi
  else
    log "=== $tag: already installed — measured, not removed"
  fi
  log "model $tag: $(ollama list | grep "^$tag " | tr -s ' ')"

  vram_at "$tag" 16384
  vram_at "$tag" 32768

  log "toolprobe $tag"
  cargo run -q -p bench --bin toolprobe -- --model "$tag" > "$OUT/$safe-toolprobe.txt" 2>&1
  log "toolprobe done $tag: $(grep -iE 'pass rate|correct' "$OUT/$safe-toolprobe.txt" | tail -2 | tr -s ' ' | tr '\n' ' ')"

  log "taskprobe $tag"
  cargo run -q -p bench --bin taskprobe -- --model "$tag" > "$OUT/$safe-taskprobe.txt" 2>&1
  log "taskprobe done $tag: $(grep -E '^\| (app|t=0) ' "$OUT/$safe-taskprobe.txt" | tr '\n' ' ') $(grep -E '^saved' "$OUT/$safe-taskprobe.txt")"

  stop_all
  if [ "$kept" = no ]; then
    ollama rm "$tag" >> "$OUT/trial.log" 2>&1 && log "removed $tag"
  fi
done
stop_all
log "=== trial done; installed now: $(ollama list | awk 'NR>1 {print $1}' | tr '\n' ' ')"
