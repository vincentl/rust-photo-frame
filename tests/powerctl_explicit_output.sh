#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/setup/assets/app/bin/powerctl"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

log_file="$tmpdir/log.txt"
export POWERCTL_TEST_LOG="$log_file"

cat <<'STUB' > "$tmpdir/wlr-randr"
#!/usr/bin/env bash
set -Eeuo pipefail
: "${POWERCTL_TEST_LOG:?missing log path}"
if [[ "${1:-}" == "--output" ]]; then
  printf 'command:%s\n' "$*" >>"$POWERCTL_TEST_LOG"
  exit 1
fi
printf 'detect:%s\n' "$*" >>"$POWERCTL_TEST_LOG"
exit 42
STUB
chmod +x "$tmpdir/wlr-randr"

cat <<'STUB' > "$tmpdir/vcgencmd"
#!/usr/bin/env bash
set -Eeuo pipefail
: "${POWERCTL_TEST_LOG:?missing log path}"
printf 'vcgencmd %s\n' "$*" >>"$POWERCTL_TEST_LOG"
STUB
chmod +x "$tmpdir/vcgencmd"

PATH="$tmpdir:$PATH" "$SCRIPT" wake HDMI-A-2

grep -q 'command:--output HDMI-A-2 --on' "$log_file"
grep -q 'vcgencmd display_power 1' "$log_file"
if grep -q '^detect:' "$log_file"; then
  echo "detect_out should not run for explicit output" >&2
  exit 1
fi

echo "powerctl explicit output fallback test passed"
