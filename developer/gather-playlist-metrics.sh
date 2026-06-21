#!/usr/bin/env bash
set -euo pipefail

# Collect the two inputs the playlist-metrics analyzer needs, straight off a
# running frame. Requires `metrics: true` in the active config (so the manager
# emits photo_display_metric / photo_first_shown / playlist_scheduler lines).
#
#   developer/gather-playlist-metrics.sh [DAYS|all] [OUTDIR]
#
#   DAYS    how far back to pull from the journal: a number of days, or "all"
#           for everything the journal still retains. Default: all.
#   OUTDIR  where to write photo-log.txt and photo-list.txt. Default: current dir.
#
# Override the library path with PHOTO_LIBRARY=/path if it isn't the default.
#
# Examples:
#   developer/gather-playlist-metrics.sh 7            # last 7 days -> ./photo-{log,list}.txt
#   developer/gather-playlist-metrics.sh all /tmp/pf  # everything retained -> /tmp/pf/
#
# Then copy both files to wherever you run analyze-playlist-metrics.py.

DAYS="${1:-all}"
OUTDIR="${2:-.}"
LIB="${PHOTO_LIBRARY:-/var/lib/photoframe/photos}"

mkdir -p "$OUTDIR"

since=()
if [[ "$DAYS" != "all" ]]; then
  since=(--since "${DAYS} days ago")
fi

# -t photoframe = syslog identifier (the app runs in the greetd session, not a
# systemd unit, so -u won't find it). Keep playlist_scheduler so the analyzer can
# report which scheme/parameters produced the data.
journalctl -t photoframe "${since[@]}" \
  | grep -E 'playlist_scheduler|photo_display_metric|photo_first_shown' \
  > "$OUTDIR/photo-log.txt"

# Current inventory: image files only (match the formats the app schedules), so
# stray non-image files don't show up as "never shown".
find "$LIB" -type f \
  \( -iname '*.jpg' -o -iname '*.jpeg' -o -iname '*.png' -o -iname '*.webp' \) \
  > "$OUTDIR/photo-list.txt"

printf 'wrote %s/photo-log.txt  (%s lines)\n' "$OUTDIR" "$(wc -l < "$OUTDIR/photo-log.txt")"
printf 'wrote %s/photo-list.txt (%s files)\n' "$OUTDIR" "$(wc -l < "$OUTDIR/photo-list.txt")"
