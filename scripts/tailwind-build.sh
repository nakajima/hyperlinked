#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INPUT="$ROOT_DIR/src/server/assets/tailwind.input.css"
OUTPUT="$ROOT_DIR/src/server/assets/app.css"
CONFIG="$ROOT_DIR/tailwind.config.js"

if [[ -n "${TAILWINDCSS_BIN:-}" ]]; then
  BIN="$TAILWINDCSS_BIN"
elif command -v tailwindcss >/dev/null 2>&1; then
  BIN="$(command -v tailwindcss)"
elif [[ -x "$ROOT_DIR/tools/tailwindcss" ]]; then
  BIN="$ROOT_DIR/tools/tailwindcss"
else
  echo "tailwindcss binary not found." >&2
  echo "Set TAILWINDCSS_BIN or place the standalone binary at tools/tailwindcss." >&2
  exit 1
fi

exec "$BIN" -c "$CONFIG" -i "$INPUT" -o "$OUTPUT" "$@"
