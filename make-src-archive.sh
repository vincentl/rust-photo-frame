#!/usr/bin/env bash
set -euo pipefail

# Always run from project root
ARCHIVE_NAME="../photoframe-src.tar.gz"

tar \
  --exclude=images \
  --exclude=target \
  --exclude=.git \
  --exclude=.idea \
  --exclude=.vscode \
  --exclude='*.DS_Store' \
  --exclude=make-src-archive.sh \
  --exclude=old-src \
  -czf "$ARCHIVE_NAME" \
  -C .. rust-photo-frame

echo "Archive written to $ARCHIVE_NAME"
