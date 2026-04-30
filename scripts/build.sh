#!/usr/bin/env bash
# Generate the Xcode project and build the FastWord app.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if ! command -v xcodegen >/dev/null 2>&1; then
    echo "xcodegen not found. Install with: brew install xcodegen" >&2
    exit 1
fi

if [[ ! -f LocalConfig.xcconfig ]]; then
    echo "LocalConfig.xcconfig not found." >&2
    echo "Copy LocalConfig.xcconfig.example to LocalConfig.xcconfig and set DEVELOPMENT_TEAM." >&2
    echo "(Find your Team ID at https://developer.apple.com/account → Membership.)" >&2
    exit 1
fi

xcodegen generate

xcodebuild \
    -project FastWord.xcodeproj \
    -scheme FastWord \
    -configuration Debug \
    -derivedDataPath build \
    build

APP_PATH="$REPO_ROOT/build/Build/Products/Debug/FastWord.app"
echo
echo "Built: $APP_PATH"
echo "Run with: open '$APP_PATH'"
