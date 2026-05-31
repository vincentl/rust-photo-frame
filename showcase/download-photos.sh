#!/usr/bin/env bash
set -euo pipefail

URL="https://github.com/vincentl/rust-photo-frame/releases/download/v1.0.0/showcase-photos.tar.gz"
DEST="$(dirname "$0")/photos"

mkdir -p "$DEST"
echo "downloading showcase-photos.tar.gz"
curl -fL --progress-bar "$URL" | tar -xz -C "$DEST" --strip-components=1
echo "done — photos in $DEST"
