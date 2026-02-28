#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IOS_DIR="$ROOT_DIR/hyperlinked"
CLI_BIN="${APOLLO_IOS_CLI:-}"
INSTALLER_DIR="/tmp/hyperlinks-apollo-cli-installer"

"$ROOT_DIR/scripts/export-graphql-schema.sh"

if [[ -n "$CLI_BIN" && -x "$CLI_BIN" ]]; then
  :
elif command -v apollo-ios-cli >/dev/null 2>&1; then
  CLI_BIN="$(command -v apollo-ios-cli)"
elif [[ -x "/tmp/apollo-cli-installer/apollo-ios-cli" ]]; then
  CLI_BIN="/tmp/apollo-cli-installer/apollo-ios-cli"
elif [[ -x "$INSTALLER_DIR/apollo-ios-cli" ]]; then
  CLI_BIN="$INSTALLER_DIR/apollo-ios-cli"
else
  mkdir -p "$INSTALLER_DIR/Sources/ApolloCLIInstaller"
  cat > "$INSTALLER_DIR/Package.swift" <<'PACKAGE'
// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ApolloCLIInstaller",
    dependencies: [
        .package(url: "https://github.com/apollographql/apollo-ios.git", from: "1.0.0")
    ],
    targets: [
        .target(name: "ApolloCLIInstaller")
    ]
)
PACKAGE
  cat > "$INSTALLER_DIR/Sources/ApolloCLIInstaller/placeholder.swift" <<'SOURCE'
import Foundation
SOURCE

  (
    cd "$INSTALLER_DIR"
    swift package --allow-writing-to-package-directory --allow-network-connections all apollo-cli-install
  )
  CLI_BIN="$INSTALLER_DIR/apollo-ios-cli"
fi

cd "$IOS_DIR"
"$CLI_BIN" generate --path apollo-codegen-config.json
