#!/bin/bash
# make-fake-btime.sh
#
# Usage:
#   ./make-fake-btime.sh <source_file> <target_file> "<YYYY-MM-DD HH:MM:SS UTC>"
#
# Example:
#   ./make-fake-btime.sh orig.txt copy.txt "2020-01-01 12:00:00"
#
# Use the script to create a test photo library with "old" images, add a new image to check it is displayed more often.
#
# Example
#   find ~/photo-library-test -regex ".*\(jpeg\|png\)" \
#   -exec bash -c 'tools/fake-btime.sh "$1" "/opt/photo-frame/var/photos/$(basename "$1")" "2020-01-01 12:00:00"' _ {} \;

set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "Usage: $0 <source_file> <target_file> \"<YYYY-MM-DD HH:MM:SS UTC>\""
  exit 1
fi

src="$1"
dst="$2"
fake_time="$3"

# Save real time
real_time=$(date -u +"%Y-%m-%d %H:%M:%S")

echo "[*] Disabling NTP"
sudo timedatectl set-ntp false

echo "[*] Setting fake system clock to: $fake_time"
sudo date -u -s "$fake_time"

echo "[*] Copying $src -> $dst"
cp "$src" "$dst"

echo "[*] Restoring atime/mtime to match original"
touch -r "$src" "$dst"

echo "[*] Restoring real system clock: $real_time"
sudo date -u -s "$real_time"

echo "[*] Re-enabling NTP"
sudo timedatectl set-ntp true

echo "[*] Done. File $dst has fake btime:"
stat "$dst" | grep Birth
