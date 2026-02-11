#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

failures=0

github_anchor_slug() {
  local heading="$1"

  # Normalize heading text similarly to GitHub-style markdown anchors.
  # - drop trailing heading hashes
  # - lowercase
  # - remove punctuation-like symbols
  # - collapse whitespace runs into hyphens
  heading="$(printf '%s' "${heading}" | sed -E 's/[[:space:]]*#+[[:space:]]*$//')"
  heading="$(printf '%s' "${heading}" | tr '[:upper:]' '[:lower:]')"
  heading="$(printf '%s' "${heading}" | sed -E 's/[^[:alnum:][:space:]_-]//g')"
  heading="$(printf '%s' "${heading}" | sed -E 's/[[:space:]]+/-/g; s/^-+//; s/-+$//')"
  printf '%s' "${heading}"
}

anchor_exists_in_file() {
  local target_file="$1"
  local anchor="$2"
  local heading slug base_slug duplicate_count
  local seen_slugs_file
  seen_slugs_file="$(mktemp)"

  while IFS= read -r heading; do
    base_slug="$(github_anchor_slug "${heading}")"
    [[ -n "${base_slug}" ]] || continue

    duplicate_count="$(grep -Fxc "${base_slug}" "${seen_slugs_file}" || true)"
    slug="${base_slug}"
    if [[ "${duplicate_count}" -gt 0 ]]; then
      slug="${base_slug}-${duplicate_count}"
    fi
    printf '%s\n' "${base_slug}" >> "${seen_slugs_file}"

    if [[ "${slug}" == "${anchor}" ]]; then
      rm -f "${seen_slugs_file}"
      return 0
    fi
  done < <(sed -nE 's/^#{1,6}[[:space:]]+(.*)$/\1/p' "${target_file}")

  # Also allow explicit HTML anchors in markdown.
  if rg -q "<a[[:space:]][^>]*(id|name)=[\"']${anchor}[\"']" "${target_file}"; then
    rm -f "${seen_slugs_file}"
    return 0
  fi

  rm -f "${seen_slugs_file}"
  return 1
}

while IFS= read -r source_file; do
  source_dir="$(dirname "${source_file}")"

  while IFS= read -r raw_target; do
    target="${raw_target%% *}"
    target="${target#<}"
    target="${target%>}"

    case "${target}" in
      http://*|https://*|mailto:*)
        continue
        ;;
    esac

    target_path="${target%%#*}"
    target_anchor=""
    if [[ "${target}" == *"#"* ]]; then
      target_anchor="${target#*#}"
    fi

    if [[ "${target}" == \#* ]]; then
      resolved="${source_file}"
    elif [[ -z "${target_path}" ]]; then
      resolved="${source_file}"
    elif [[ "${target_path}" = /* ]]; then
      resolved="${target_path}"
    else
      resolved="${source_dir}/${target_path}"
    fi

    if [[ ! -e "${resolved}" ]]; then
      echo "BROKEN: ${source_file} -> ${target}"
      failures=$((failures + 1))
      continue
    fi

    if [[ -n "${target_anchor}" && "${resolved}" == *.md ]]; then
      if ! anchor_exists_in_file "${resolved}" "${target_anchor}"; then
        echo "BROKEN ANCHOR: ${source_file} -> ${target}"
        failures=$((failures + 1))
      fi
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
