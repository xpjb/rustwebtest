#!/usr/bin/env bash
# Produce a zip for itch.io: HTML5 project → upload this file.
# https://itch.io/docs/creators/html5 — index.html must be at the zip root.
# Output defaults to pkg/rustwebtest-itch-YYYYMMDDTHHMMSSZ.zip (UTC, ISO-8601 basic; under .gitignored pkg/).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

DATE="$(date -u +"%Y%m%dT%H%M%SZ")"
ZIP_NAME="${ZIP_NAME:-pkg/rustwebtest-itch-${DATE}.zip}"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  ./build.sh
else
  if [[ ! -f pkg/rustwebtest_bg.wasm ]]; then
    echo "No pkg/rustwebtest_bg.wasm — run ./build.sh or SKIP_BUILD=0" >&2
    exit 1
  fi
fi

if ! command -v 7z &>/dev/null; then
  echo "7z not found. Install 7-Zip and add 7z.exe to PATH." >&2
  echo "  https://www.7-zip.org/" >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cp index.html "$TMP/"
mkdir -p "$TMP/pkg"
# Omit any prior itch bundles sitting in pkg/ so they are not packed recursively.
shopt -s nullglob
for f in pkg/*; do
  [[ -e "$f" ]] || continue
  base="${f##*/}"
  [[ "$base" == *.zip ]] && continue
  cp -a "$f" "$TMP/pkg/"
done

mkdir -p "$(dirname "$ROOT/$ZIP_NAME")"
OUT="$ROOT/$ZIP_NAME"
rm -f "$OUT"
# itch expects a .zip; -tzip writes standard zip format.
(cd "$TMP" && 7z a -tzip -bd -bso0 -bsp0 "$OUT" index.html pkg)

echo "itch.io bundle -> $OUT"
echo "Upload as an HTML5 game; set index to index.html."
echo "Project → Embed / Advanced → enable SharedArrayBuffer support (required for wasm threads)."
