#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_PATH="${1:-$ROOT_DIR/hyperlinked/hyperlinked/GraphQL/Schema/schema.graphqls}"

cd "$ROOT_DIR"
cargo run --quiet --bin hyperlinked -- export-graphql-schema --out "$OUT_PATH"
