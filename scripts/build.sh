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
    echo "LocalConfig.xcconfig not found — using ad-hoc signing." >&2
    echo "  (Local builds will work, but TCC permissions reset on every rebuild." >&2
    echo "   For stable signing, copy LocalConfig.xcconfig.example to LocalConfig.xcconfig" >&2
    echo "   and set your DEVELOPMENT_TEAM. Free Apple ID works.)" >&2
    cat > LocalConfig.xcconfig <<'EOF'
// Auto-generated fallback for ad-hoc signing.
DEVELOPMENT_TEAM =
CODE_SIGN_STYLE = Manual
CODE_SIGN_IDENTITY = -
EOF
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
