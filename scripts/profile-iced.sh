#!/bin/bash
# Profile the iced standalone while it plays a file.
#
# Builds a release standalone, loads the wav, starts playback, and writes a
# CPU / RSS / footprint log. Egui and rune are not launched. The editor
# backend in src/ui/ui.rs must be Ui::Iced.
#
# SONOMORI_PROFILE_BUILD=profiling uses the profiling profile instead, which
# keeps line tables so the stack sample at the end of the log has names. That
# profile does not share the release build cache.
#
#   scripts/profile-iced.sh [wav] [seconds]
#
# The default wav is the organ drone used for this pass. Seconds is the
# steady-state window after the file is loaded and playback has started.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WAV="${1:-/Users/giordanopiazza/Downloads/mixkit-creepy-organ-drone-2746.wav}"
WINDOW="${2:-15}"
STAMP="$(date +%Y%m%d-%H%M%S)"
DIR="$ROOT/target/profile/iced-$STAMP"
READY="$DIR/ready"
APPLOG="$DIR/app.log"
PROFILE="${SONOMORI_PROFILE_BUILD:-release}"
BIN="$ROOT/target/$PROFILE/sonomori"
PID=""

if [[ ! "$WINDOW" =~ ^[0-9]+$ ]] || [[ "$WINDOW" -lt 5 ]]; then
    echo "measurement window must be at least 5 seconds" >&2
    exit 2
fi

mkdir -p "$DIR"

cleanup() {
    if [[ -n "${PID}" ]] && kill -0 "$PID" 2>/dev/null; then
        kill -INT "$PID" 2>/dev/null || true
        for _ in 1 2 3 4 5; do
            kill -0 "$PID" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "$PID" 2>/dev/null; then
            kill -KILL "$PID" 2>/dev/null || true
        fi
    fi
}
trap cleanup EXIT

if ! grep -q 'pub const BACKEND: Ui = Ui::Iced;' "$ROOT/src/ui/ui.rs"; then
    echo "src/ui/ui.rs is not Ui::Iced. This script skips egui and rune." >&2
    exit 2
fi

if [[ ! -f "$WAV" ]]; then
    echo "audio file not found: $WAV" >&2
    exit 2
fi

echo "building $PROFILE binary"
if [[ "$PROFILE" == "release" ]]; then
    cargo build --release --bin sonomori --manifest-path "$ROOT/Cargo.toml"
else
    cargo build --profile "$PROFILE" --bin sonomori --manifest-path "$ROOT/Cargo.toml"
fi

if [[ ! -x "$BIN" ]]; then
    echo "binary missing: $BIN" >&2
    exit 1
fi

echo "launching $(basename "$BIN")"
SONOMORI_LOAD="$WAV" \
SONOMORI_PLAY=1 \
SONOMORI_READY="$READY" \
SONOMORI_STATS="$DIR/stats.txt" \
"$BIN" >"$APPLOG" 2>&1 &
PID=$!

echo "waiting for load"
loaded=0
for _ in $(seq 1 90); do
    if [[ -f "$READY" ]]; then
        loaded=1
        break
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "app exited before the file loaded" >&2
        cat "$APPLOG" >&2 || true
        exit 1
    fi
    sleep 1
done

if [[ "$loaded" -ne 1 ]]; then
    echo "timed out waiting for playback" >&2
    cat "$APPLOG" >&2 || true
    exit 1
fi

status="$(head -n 1 "$READY" || true)"
if [[ "$status" != "ok" ]]; then
    echo "load failed ($status)" >&2
    cat "$APPLOG" >&2 || true
    exit 1
fi

# Let the first playhead frames and the GPU settle before the window.
sleep 2

SAMPLES="$DIR/samples.txt"
python3 - "$PID" "$WINDOW" "$SAMPLES" <<'PY'
import subprocess
import sys
import time

pid = int(sys.argv[1])
window = int(sys.argv[2])
out_path = sys.argv[3]


def parse_cputime(text):
    days = 0
    if "-" in text:
        day_text, text = text.split("-", 1)
        days = int(day_text)
    parts = [float(part) for part in text.split(":")]
    if len(parts) == 3:
        hours, minutes, seconds = parts
    elif len(parts) == 2:
        hours = 0.0
        minutes, seconds = parts
    else:
        hours = 0.0
        minutes = 0.0
        seconds = parts[0]
    return ((days * 24 + hours) * 60 + minutes) * 60 + seconds


def read():
    out = subprocess.check_output(
        ["ps", "-p", str(pid), "-o", "cputime=,rss="],
        text=True,
    ).strip()
    if not out:
        return None
    cputime, rss = out.split()
    return parse_cputime(cputime), int(rss)


def percentile(values, fraction):
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * fraction)))
    return ordered[index]


start = read()
if start is None:
    sys.exit("process disappeared before sampling")

rows = []
previous_cpu, _rss = start
for index in range(1, window + 1):
    time.sleep(1.0)
    current = read()
    if current is None:
        break
    cpu_seconds, rss_kb = current
    percent = max(0.0, (cpu_seconds - previous_cpu) * 100.0)
    previous_cpu = cpu_seconds
    rows.append((index, percent, rss_kb))

if len(rows) < window:
    sys.exit(f"only sampled {len(rows)} of {window} seconds")

cpu = [row[1] for row in rows]
rss = [row[2] / 1024.0 for row in rows]


def median(values):
    return percentile(values, 0.5)


with open(out_path, "w", encoding="utf-8") as handle:
    handle.write("second\tcpu_percent\trss_mb\n")
    for second, percent, rss_kb in rows:
        handle.write(f"{second}\t{percent:.2f}\t{rss_kb / 1024.0:.1f}\n")
    handle.write(
        "\n"
        f"cpu_median {median(cpu):.2f}\n"
        f"cpu_mean {sum(cpu) / len(cpu):.2f}\n"
        f"cpu_min {min(cpu):.2f}\n"
        f"cpu_p95 {percentile(cpu, 0.95):.2f}\n"
        f"cpu_max {max(cpu):.2f}\n"
        f"rss_median_mb {median(rss):.1f}\n"
        f"rss_min_mb {min(rss):.1f}\n"
        f"rss_max_mb {max(rss):.1f}\n"
    )
PY

FOOT="$DIR/footprint.txt"
if command -v footprint >/dev/null 2>&1; then
    footprint -p "$PID" >"$FOOT" 2>&1 || true
else
    echo "footprint not available" >"$FOOT"
fi

LOG="$DIR/profile.log"
{
    echo "sonomori iced profile"
    echo "date $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "binary $BIN"
    echo "wav $WAV"
    echo "window_s $WINDOW"
    echo "pid $PID"
    echo
    cat "$SAMPLES"
    echo
    echo "footprint"
    # phys_footprint is the memory number Activity Monitor shows.
    # head closes the pipe early; don't let that abort the log under pipefail.
    set +o pipefail
    grep -E 'phys_footprint|peak|note' "$FOOT" | head -n 40 || true
    set -o pipefail
    if [[ -f "$DIR/stats.txt" ]]; then
        echo
        echo "frames"
        cat "$DIR/stats.txt"
    fi
} >"$LOG"
cp "$LOG" "$ROOT/target/profile/iced-latest.log"

# Stacks after the timed window, so sampling does not land in the CPU numbers.
STACKS="$DIR/stacks.txt"
sample "$PID" 2 -file "$STACKS" -mayDie >/dev/null 2>&1 || true
{
    echo
    echo "stacks $STACKS"
    if [[ -f "$STACKS" ]]; then
        set +o pipefail
        awk 'BEGIN { keep = 0 } /^Call graph/ { keep = 1 } keep { print }' "$STACKS" | head -n 80 || true
        set -o pipefail
    else
        echo "(no stacks)"
    fi
} >>"$LOG"
cp "$LOG" "$ROOT/target/profile/iced-latest.log"

echo
echo "log $LOG"
grep -E '^(cpu_|rss_)' "$SAMPLES"
grep -E 'phys_footprint' "$FOOT" || true
if [[ -f "$DIR/stats.txt" ]]; then
    echo
    cat "$DIR/stats.txt"
fi
