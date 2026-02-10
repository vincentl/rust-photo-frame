#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

failures=0

while IFS= read -r source_file; do
  source_dir="$(dirname "${source_file}")"

  while IFS= read -r raw_target; do
    target="${raw_target%% *}"
    target="${target#<}"
    target="${target%>}"

    case "${target}" in
      http://*|https://*|mailto:*|\#*)
        continue
        ;;
    esac

    target_path="${target%%#*}"
    if [[ -z "${target_path}" ]]; then
      continue
    fi

    if [[ "${target_path}" = /* ]]; then
      resolved="${target_path}"
    else
      resolved="${source_dir}/${target_path}"
    fi

    if [[ ! -e "${resolved}" ]]; then
      echo "BROKEN: ${source_file} -> ${target}"
      failures=$((failures + 1))
    fi
  done < <(
    perl -nE 'while (/\[[^\]]+\]\(([^)]+)\)/g) { say $1 }' "${source_file}"
  )
done < <(rg --files -g '*.md')

if [[ ${failures} -ne 0 ]]; then
  echo
  echo "Found ${failures} broken markdown link(s)."
  exit 1
fi

echo "Markdown link check passed."
