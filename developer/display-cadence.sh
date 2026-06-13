#!/usr/bin/env bash
set -euo pipefail

# Analyze a slow-motion phone recording of the panel to recover the TRUE
# displayed frame cadence — the ground truth that journal frame stats cannot
# provide (stats measure submission cadence; the panel shows what was
# latched). Film the screen with the phone's slo-mo mode, transfer the clip,
# then:
#
#   developer/display-cadence.sh IMG_1234.mov [capture_fps]
#
# capture_fps is the real sensor rate of the slo-mo (iPhone: usually 120 or
# 240; the container often says 30 because the slowdown is baked in). If
# omitted, intervals are reported in captured-frame units.
#
# Reading the output: consecutive captured frames with a near-zero luma
# difference mean the panel showed the same frame; spikes mean it updated.
# Healthy 60 fps playback shows updates every capture_fps/60 frames. Long
# zero runs followed by a spike are visible hangs/pops.

VIDEO="${1:?usage: display-cadence.sh <video> [capture_fps]}"
CAPTURE_FPS="${2:-0}"
LOG="$(mktemp /tmp/display-cadence.XXXXXX.log)"
trap 'rm -f "${LOG}"' EXIT

ffmpeg -y -v error -i "${VIDEO}" \
    -vf "format=gray,scale=320:-2,tblend=all_mode=difference,signalstats,metadata=print:key=lavfi.signalstats.YAVG:file=${LOG}" \
    -f null -

python3 - "$LOG" "$CAPTURE_FPS" << 'EOF'
import re
import sys

log_path, capture_fps = sys.argv[1], float(sys.argv[2])
vals = []
with open(log_path) as f:
    for line in f:
        m = re.search(r"YAVG=([\d.]+)", line)
        if m:
            vals.append(float(m.group(1)))

n = len(vals)
if n < 10:
    sys.exit("too few frames parsed")

# Threshold between sensor noise and a real panel update: halfway between
# the median (noise floor) and the 95th percentile (updates), floored.
svals = sorted(vals)
noise = svals[n // 2]
busy = svals[int(n * 0.95)]
threshold = max(noise * 3, (noise + busy) / 2, 0.5)
print(f"frames={n} noise_floor={noise:.2f} threshold={threshold:.2f}")

def fmt(frames):
    if capture_fps > 0:
        return f"{frames / capture_fps * 1000.0:7.1f}ms"
    return f"{frames:4d}fr"

updates = [i for i, v in enumerate(vals) if v >= threshold]
if len(updates) < 2:
    sys.exit("no panel updates detected above threshold")

intervals = [b - a for a, b in zip(updates, updates[1:]) if b - a > 0]
intervals.sort()
print(f"updates={len(updates)}")
print(f"interval p50={fmt(intervals[len(intervals)//2])} "
      f"p90={fmt(intervals[int(len(intervals)*0.9)])} "
      f"max={fmt(intervals[-1])}")

print("\nlongest static runs (start_frame, length):")
runs = []
prev = updates[0]
for u in updates[1:]:
    runs.append((prev, u - prev))
    prev = u
for start, length in sorted(runs, key=lambda r: -r[1])[:5]:
    print(f"  frame {start:4d}: {fmt(length)} static")

print("\ntimeline of updates (frame: diff magnitude):")
for i in updates:
    bar = "#" * min(int(vals[i] * 2), 60)
    print(f"{i:5d} {vals[i]:7.2f} {bar}")
EOF
