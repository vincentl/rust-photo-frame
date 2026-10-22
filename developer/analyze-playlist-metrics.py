#!/usr/bin/env python3
"""Analyze playlist scheduling metrics from a frame's journal.

Audits how the playlist scheduler behaved on a real device: photo coverage
(starvation), recurrence spacing (clumping / soon-repeats), whether the
half-life weighting actually favors newer photos, and how promptly freshly
uploaded photos debut via the priority FIFO.

────────────────────────────────────────────────────────────────────────────
1. GATHER  (on the frame; needs `metrics: true` in the active config)

   Easiest — use the companion script:
       developer/gather-playlist-metrics.sh 7        # last 7 days
       developer/gather-playlist-metrics.sh all      # everything retained

   Or by hand:
       # last N days (use --since "<date>" for an explicit start):
       journalctl -t photoframe --since "7 days ago" \\
         | grep -E 'playlist_scheduler|photo_display_metric|photo_first_shown' \\
         > photo-log.txt
       # since the beginning of what the journal still retains (drop --since):
       journalctl -t photoframe \\
         | grep -E 'playlist_scheduler|photo_display_metric|photo_first_shown' \\
         > photo-log.txt
       # current inventory (image files only):
       find /var/lib/photoframe/photos -type f \\
         \\( -iname '*.jpg' -o -iname '*.jpeg' -o -iname '*.png' -o -iname '*.webp' \\) \\
         > photo-list.txt

   Copy both files to wherever you run this script. journald retention caps how
   far back "the beginning" goes; the script reports the actual span it found.

2. RUN
       developer/analyze-playlist-metrics.py photo-log.txt photo-list.txt
       developer/analyze-playlist-metrics.py --help     # all options

   The script handles older log formats automatically (early lines predate
   age_days/created_source/photo_first_shown, and a middle band quoted
   created_source) and segments by process restart (seq resets per PID).

3. INTERPRET  (each section prints a one-line "good looks like" hint)

   COVERAGE      never-shown should trend to 0; a few recent uploads not yet
                 shown is normal (they haven't had time).
   TIMES-SHOWN   a tight range is healthy; counts near 0 for old photos = the
                 starvation that weighted-random suffered.
   RECURRENCE    low soon-repeat % = no clumping. CV runs above the idealized
                 1-min_spacing because real data mixes weights and a growing
                 library — not a regression.
   FLOOR         median gap/(min_spacing*inventory) >= 1 means the minimum-wait
                 floor held. Apparent sub-floor gaps are inflated by inventory
                 growth (gap is scored against the final, larger inventory).
   GAP vs WEIGHT medians should fall as weight rises — newer photos recurring
                 sooner is the weighting working.
   FIFO DEBUT    the head of each upload appears fast; a big batch then drains
                 at the display rate (~batch_size * dwell), so large pushes take
                 tens of minutes to fully surface. That's the frame's one-photo-
                 per-dwell limit, not a scheduler fault.
   PAIRING       (only with playlist.pairing enabled) screens vs appearances; the
                 measured paired share next to the model 2q/(1+q), q = 1-(1-pi*p)^W
                 (W = pairing_lookahead from the stamp; W=1 is 2*pi*p/(1+pi*p));
                 the repeat-partner rate; photos on consecutive screens, each
                 attributed to its cause (a debut's own Z, Z; an add or removal
                 between planning and display; a sleep/wake gap; a skipped
                 screen). Sessions start at each playlist_scheduler stamp. Only
                 UNATTRIBUTED repeats are failures (design invariant I5): good is 0.
                 The mean gap by weight x pairable should match across pairable
                 and non-pairable photos at equal weight (an end-to-end check; the
                 appearance sequence is the unpaired scheduler's).
"""

import argparse
import re
import statistics
from collections import Counter, OrderedDict, defaultdict
from datetime import datetime

ISO_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")
PID_RE = re.compile(r"photoframe\[(\d+)\]")
KV_RE = re.compile(r'(\w+)=("[^"]*"|\S+)')


def parse_kv(text):
    return {m.group(1): m.group(2).strip('"') for m in KV_RE.finditer(text)}


def parse_log(path):
    """Return (displays, firsts, schedulers) as lists of field dicts."""
    displays, firsts, schedulers = [], [], []
    with open(path, encoding="utf-8", errors="replace") as fh:
        for index, line in enumerate(fh):
            if "photo_display_metric" in line:
                tag, bucket = "photo_display_metric", displays
            elif "photo_first_shown" in line:
                tag, bucket = "photo_first_shown", firsts
            elif "playlist_scheduler" in line:
                tag, bucket = "playlist_scheduler", schedulers
            else:
                continue
            ts = ISO_RE.search(line)
            pid = PID_RE.search(line)
            after = line.split(tag, 1)[1]
            # path can contain spaces, so split it off as the rest of the line.
            if " path=" in after:
                kv_part, photo = after.split(" path=", 1)
                photo = photo.strip()
            else:
                kv_part, photo = after, None
            rec = parse_kv(kv_part)
            rec["dt"] = (
                datetime.strptime(ts.group(1)[:19], "%Y-%m-%dT%H:%M:%S") if ts else None
            )
            rec["pid"] = pid.group(1) if pid else "?"
            rec["path"] = photo
            rec["line"] = index
            bucket.append(rec)
    return displays, firsts, schedulers


def as_int(rec, key):
    try:
        return int(rec[key])
    except (KeyError, ValueError, TypeError):
        return None


def as_float(rec, key):
    try:
        return float(rec[key])
    except (KeyError, ValueError, TypeError):
        return None


def pct(sorted_vals, p):
    return sorted_vals[min(len(sorted_vals) - 1, int(p * len(sorted_vals)))]


def main():
    ap = argparse.ArgumentParser(
        description="Analyze playlist scheduling metrics (see module docstring "
        "for how to gather, run, and interpret).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="example: developer/analyze-playlist-metrics.py photo-log.txt photo-list.txt",
    )
    ap.add_argument("log", help="photo-log.txt (grepped journal lines)")
    ap.add_argument("list", help="photo-list.txt (find of the photo library)")
    ap.add_argument(
        "--min-spacing",
        type=float,
        default=None,
        help="playlist.min-spacing for the floor check; auto-detected from the "
        "playlist_scheduler line when present, else 0.7",
    )
    ap.add_argument(
        "--fresh-window-secs",
        type=int,
        default=3600,
        help="first_shown latency below this counts as a freshly uploaded debut "
        "rather than a startup/old-library first show (default 3600)",
    )
    args = ap.parse_args()

    displays, firsts, schedulers = parse_log(args.log)
    if not displays:
        raise SystemExit("no photo_display_metric lines found in " + args.log)

    # --- scheduler config (context + min-spacing auto-detect) ---
    min_spacing = args.min_spacing
    print("# SCHEDULER CONFIG" + (" (from playlist_scheduler lines)" if schedulers else ""))
    if schedulers:
        seen = OrderedDict()
        for s in schedulers:
            key = (s.get("order"), s.get("min_spacing"), s.get("new_multiplicity"), s.get("half_life_secs"))
            seen[key] = s
        for order, ms, mult, hl in seen:
            print(f"  order={order} min_spacing={ms} new_multiplicity={mult} half_life_secs={hl}")
        for s in schedulers:
            if s.get("pairing") is not None:
                print(
                    f"  pairing={s.get('pairing')} probability={s.get('pairing_probability')} "
                    f"lookahead={s.get('pairing_lookahead', '1')} "
                    f"bar_ratio={s.get('pairing_bar_ratio')} dwell_multiplier={s.get('pairing_dwell_multiplier')}"
                )
                break
        if min_spacing is None:
            last = list(seen)[-1][1]
            min_spacing = float(last) if last is not None else 0.7
    else:
        print("  (no playlist_scheduler lines; gather with the playlist_scheduler grep to capture config)")
    if min_spacing is None:
        min_spacing = 0.7
    print(f"  using min_spacing={min_spacing:.2f} for the floor check")

    # --- overview ---
    pids = list(OrderedDict((d["pid"], 1) for d in displays))
    span0, span1 = displays[0]["dt"], displays[-1]["dt"]
    dur = span1 - span0
    print("\n# OVERVIEW")
    print(f"  displays={len(displays)}  first_shown={len(firsts)}")
    print(f"  span    {span0} -> {span1}  ({dur.days}d {dur.seconds // 3600}h)")
    print(f"  sessions(PIDs)={len(pids)} (seq resets each restart)")
    print(f"  inventory {as_int(displays[0], 'inventory')} -> {as_int(displays[-1], 'inventory')}")

    # --- coverage ---
    listed = {ln.strip() for ln in open(args.list, encoding="utf-8", errors="replace") if ln.strip()}
    shown = Counter(d["path"] for d in displays if d["path"])
    never = listed - set(shown)
    gone = set(shown) - listed
    print(f"\n# COVERAGE  (good: never-shown -> 0; recent uploads excepted)")
    print(f"  distinct shown ever : {len(shown)}")
    print(f"  in list, never shown: {len(never)} ({100 * len(never) / max(1, len(listed)):.1f}%)")
    print(f"  shown but not listed: {len(gone)} (removed since)")
    for p in list(never)[:6]:
        print(f"      never: {p}")

    # --- times shown ---
    counts = sorted(shown.values())
    print("\n# TIMES-SHOWN  (good: tight range; near 0 for old photos = starvation)")
    print(
        f"  min={counts[0]} p50={statistics.median(counts):.0f} "
        f"p90={pct(counts, 0.9)} max={counts[-1]} mean={statistics.mean(counts):.1f}"
    )

    # --- recurrence gaps (logged, per-session; gap>0) ---
    gaps = [
        (as_int(d, "gap"), as_float(d, "weight"), as_int(d, "inventory"))
        for d in displays
        if as_int(d, "gap") and as_int(d, "gap") > 0
    ]
    g = sorted(x[0] for x in gaps)
    cv = statistics.pstdev(g) / statistics.mean(g)
    print("\n# RECURRENCE GAP  (good: soon-repeat % low; CV >= 1-min_spacing in practice)")
    print(
        f"  n={len(g)} min={g[0]} p5={pct(g, .05)} p50={statistics.median(g):.0f} "
        f"p95={pct(g, .95)} max={g[-1]} CV={cv:.2f} (ideal {1 - min_spacing:.2f})"
    )
    for thr in (10, 25, 50, 100):
        c = sum(1 for x in g if x < thr)
        print(f"  gap<{thr:4d}: {c:5d} ({100 * c / len(g):.2f}%)")

    # --- floor check (weight~1) ---
    w1 = [(gp, inv) for gp, w, inv in gaps if w is not None and abs(w - 1.0) < 0.05 and inv]
    if w1:
        ratios = sorted(gp / (min_spacing * inv) for gp, inv in w1)
        viol = sum(1 for r in ratios if r < 1.0)
        print("\n# FLOOR CHECK  (weight~1.0; good: median ratio >= 1)")
        print(
            f"  gap/(min_spacing*inventory): p5={pct(ratios, .05):.2f} "
            f"p50={statistics.median(ratios):.2f}   below 1.0: {100 * viol / len(ratios):.1f}% "
            f"(inflated by inventory growth)"
        )

    # --- gap vs weight ---
    print("\n# GAP vs WEIGHT  (good: median falls as weight rises = weighting active)")
    buckets = [("~1.0", 0, 1.25), ("1.25-2", 1.25, 2), ("2-2.75", 2, 2.75), (">=2.75", 2.75, 1e9)]
    by = defaultdict(list)
    for gp, w, _ in gaps:
        if w is None:
            continue
        for name, lo, hi in buckets:
            if lo <= w < hi:
                by[name].append(gp)
                break
    for name, _, _ in buckets:
        if by[name]:
            print(f"  w {name:8s}: n={len(by[name]):5d}  median gap={statistics.median(by[name]):.0f}")

    # --- FIFO debut latency ---
    lat = sorted(as_float(r, "latency_secs") for r in firsts if as_float(r, "latency_secs") is not None)
    if lat:
        fresh = [x for x in lat if x < args.fresh_window_secs]
        print(f"\n# FIFO DEBUT LATENCY  (good: head fast; big batches drain at ~dwell/photo)")
        print(f"  first_shown n={len(lat)}  fresh(<{args.fresh_window_secs}s)={len(fresh)}  startup/old={len(lat) - len(fresh)}")
        if fresh:
            print(
                f"  fresh debut secs: min={fresh[0]:.0f} p50={statistics.median(fresh):.0f} "
                f"p90={pct(fresh, .9):.0f} max={fresh[-1]:.0f}"
            )
            for thr, lbl in ((60, "1m"), (300, "5m"), (900, "15m"), (1800, "30m")):
                c = sum(1 for x in fresh if x < thr)
                print(f"    under {lbl:3s}: {c:4d} ({100 * c / len(fresh):.0f}%)")

    pairing_section(displays, schedulers)

    # --- inventory growth (upload events) ---
    print("\n# INVENTORY GROWTH  (upload events, step >= 5)")
    prev = None
    for d in displays:
        inv = as_int(d, "inventory")
        if inv is not None and prev is not None and inv - prev >= 5:
            print(f"  {d['dt']}  {prev} -> {inv}  (+{inv - prev})")
        if inv is not None:
            prev = inv


def split_sessions(displays, schedulers):
    """Process sessions in log order. A session starts at each
    `playlist_scheduler` stamp (one per process start with metrics on), and
    also wherever `screen=` goes backwards or the PID changes, so two runs
    are never merged when the stamp fell outside the gathered span or a PID
    was reused. Returns [(stamp or None, [display rows])]."""
    events = sorted(
        [(r["line"], "stamp", r) for r in schedulers] + [(r["line"], "display", r) for r in displays],
        key=lambda e: e[0],
    )
    sessions = []
    current = None
    for _, kind, rec in events:
        if kind == "stamp":
            current = (rec, [])
            sessions.append(current)
            continue
        if current is not None and current[1]:
            last = current[1][-1]
            screen, last_screen = as_int(rec, "screen"), as_int(last, "screen")
            restarted = screen is not None and last_screen is not None and screen < last_screen
            if restarted or rec["pid"] != last["pid"]:
                current = None
        if current is None:
            current = (None, [])
            sessions.append(current)
        current[1].append(rec)
    return sessions


def pairing_section(displays, schedulers):
    """# PAIRING: portrait pairing's screens, from the per-appearance lines'
    screen=, pair=, partner=, pairable=, sent=, removed= and added= fields.
    Only sessions that ran with pairing on (stamp `pairing=true`, or lines
    carrying `pair=`) are counted; with pairing off the section is silent."""
    sessions = []
    models = set()  # (p, W) per stamp
    for stamp, rows in split_sessions(displays, schedulers):
        rows = [d for d in rows if "pair" in d and "screen" in d]
        on = stamp.get("pairing") == "true" if stamp is not None else bool(rows)
        if not on or not rows:
            continue
        if stamp is not None and stamp.get("pairing_probability") is not None:
            models.add((float(stamp["pairing_probability"]), int(stamp.get("pairing_lookahead") or 1)))
        screens = OrderedDict()
        for d in rows:
            screens.setdefault(d["screen"], []).append(d)
        sessions.append(list(screens.values()))
    if not sessions:
        return
    rows = [d for session in sessions for screen in session for d in screen]
    n_screens = sum(len(s) for s in sessions)
    appearances = len(rows)
    pairable_apps = sum(1 for d in rows if d.get("pairable") == "true")
    paired_apps = sum(1 for d in rows if d.get("pair") in ("first", "second"))
    pi = pairable_apps / appearances
    share = paired_apps / max(1, pairable_apps)

    print("\n# PAIRING  (good: unattributed consecutive repeats = 0; equal gaps for pairable and not)")
    print(f"  sessions={len(sessions)} screens={n_screens} appearances={appearances} pair screens={paired_apps // 2}")
    def model(p, w):
        # Look-ahead design: a pairable head pairs when any of the W unsent
        # positions after it is pairable and wins the coin; W=1 is 2*pi*p/(1+pi*p).
        q = 1 - (1 - pi * p) ** max(1, w)
        return 2 * q / (1 + q)

    predicted = ", ".join(f"{model(p, w):.3f} (p={p:g}, W={w})" for p, w in sorted(models or {(1.0, 1)}))
    print(
        f"  pi (pairable share of appearances)={pi:.3f}  paired share={share:.3f}  "
        f"model={predicted} (2q/(1+q), q=1-(1-pi*p)^W; an approximation)"
    )

    last, rep, tot = {}, 0, 0
    for session in sessions:
        for screen in session:
            if len(screen) == 2:
                a, b = screen[0]["path"], screen[1]["path"]
                for u, v in ((a, b), (b, a)):
                    if u in last:
                        tot += 1
                        rep += last[u] == v
                    last[u] = v
    print(f"  repeat-partner rate={rep / max(1, tot):.3f}")

    causes = Counter()
    examples = []
    for session in sessions:
        times = [s[0]["dt"] for s in session if s[0]["dt"]]
        steps = sorted((b - a).total_seconds() for a, b in zip(times, times[1:]))
        typical = steps[len(steps) // 2] if steps else 0
        for prev, cur in zip(session, session[1:]):
            shared = {d["path"] for d in prev} & {d["path"] for d in cur}
            if not shared:
                continue
            sent_prev, sent_cur = as_int(prev[0], "sent"), as_int(cur[0], "sent")
            gap_secs = (
                (cur[0]["dt"] - prev[0]["dt"]).total_seconds()
                if cur[0]["dt"] and prev[0]["dt"]
                else 0
            )
            changed = any(
                (as_int(d, "removed") or 0) > 0 or (as_int(d, "added") or 0) > 0 for d in cur
            )
            if any(d["path"] in shared and as_int(d, "shown_count") == 1 for d in prev):
                cause = "debut (the unpaired sequence's own Z, Z)"
            elif changed:
                cause = "inventory change (add or removal) between planning and display"
            elif typical and gap_secs > max(120, 10 * typical):
                cause = "sleep/wake gap"
            elif sent_prev is not None and sent_cur is not None and sent_prev >= 0 and sent_cur >= 0 and sent_cur - sent_prev != 1:
                cause = "skipped screen (failed decode, purge, reset)"
            else:
                cause = "UNATTRIBUTED"
                if len(examples) < 5:
                    examples.append((prev[0]["dt"], sorted(shared)))
            causes[cause] += 1
    total = sum(causes.values())
    print(f"  photos on consecutive screens: {total}")
    for cause, n in causes.most_common():
        print(f"    {n:5d}  {cause}")
    for dt, paths in examples:
        print(f"      unattributed at {dt}: {', '.join(p for p in paths if p)}")

    buckets = [("~1.0", 0, 1.25), ("1.25-2", 1.25, 2), ("2-2.75", 2, 2.75), (">=2.75", 2.75, 1e9)]
    by = defaultdict(list)
    for d in rows:
        gp, w = as_int(d, "gap"), as_float(d, "weight")
        if not gp or gp <= 0 or w is None:
            continue
        for name, lo, hi in buckets:
            if lo <= w < hi:
                by[(name, d.get("pairable"))].append(gp)
                break
    print("  mean recurrence gap (appearances) by weight x pairable:")
    for name, _, _ in buckets:
        cells = []
        for flag in ("true", "false"):
            g = by.get((name, flag))
            cells.append(f"pairable={flag}: " + (f"n={len(g):5d} mean={statistics.mean(g):7.1f}" if g else "-"))
        if any(by.get((name, f)) for f in ("true", "false")):
            print(f"    w {name:8s}  " + "   ".join(cells))


if __name__ == "__main__":
    main()
