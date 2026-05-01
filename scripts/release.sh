#!/usr/bin/env bash
# Build, sign, notarize, and package FastWord as a distributable DMG.
#
# Prerequisites (one-time setup):
#   1. Apple Developer Program membership.
#   2. "Developer ID Application" certificate installed in your login keychain
#      (download from developer.apple.com → Certificates, or generate via Xcode).
#   3. App-specific password for notarytool — create at appleid.apple.com → Sign-In and Security.
#   4. Store credentials once:
#        xcrun notarytool store-credentials FASTWORD_NOTARY \
#            --apple-id "you@example.com" \
#            --team-id  "YOUR_TEAM_ID" \
#            --password "xxxx-xxxx-xxxx-xxxx"
#
# Required env vars (set in your shell, NOT in the repo):
#   FASTWORD_NOTARY_PROFILE  Name of the stored notarytool profile (default: FASTWORD_NOTARY)
#
# Output:
#   build/release/FastWord-<version>.dmg  (signed + notarized + stapled)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

NOTARY_PROFILE="${FASTWORD_NOTARY_PROFILE:-FASTWORD_NOTARY}"

if [[ ! -f LocalConfig.xcconfig ]]; then
    echo "LocalConfig.xcconfig not found. See LocalConfig.xcconfig.example." >&2
    exit 1
fi

VERSION="$(awk '/CFBundleShortVersionString/ {getline; gsub(/[ \t<\/string>]/, ""); print; exit}' FastWord/Resources/Info.plist 2>/dev/null || echo "0.1.0")"
[[ -z "$VERSION" ]] && VERSION="0.1.0"

OUT_DIR="$REPO_ROOT/build/release"
DERIVED="$REPO_ROOT/build/release-derived"
APP_NAME="FastWord"
APP_PATH="$DERIVED/Build/Products/Release/$APP_NAME.app"
DMG_NAME="$APP_NAME-$VERSION.dmg"
DMG_PATH="$OUT_DIR/$DMG_NAME"

echo "==> Locating Developer ID Application certificate"
DEV_ID_IDENTITY="$(security find-identity -v -p codesigning | grep -m1 "Developer ID Application" | sed -E 's/.*"(.*)".*/\1/' || true)"
if [[ -z "$DEV_ID_IDENTITY" ]]; then
    echo "No 'Developer ID Application' certificate found in keychain." >&2
    echo "Install one from developer.apple.com → Certificates, then re-run." >&2
    exit 1
fi
echo "    using: $DEV_ID_IDENTITY"

echo "==> Generating Xcode project"
xcodegen generate >/dev/null

echo "==> Building Release"
rm -rf "$DERIVED"
xcodebuild \
    -project FastWord.xcodeproj \
    -scheme FastWord \
    -configuration Release \
    -derivedDataPath "$DERIVED" \
    CODE_SIGN_STYLE=Manual \
    CODE_SIGN_IDENTITY="$DEV_ID_IDENTITY" \
    CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO \
    OTHER_CODE_SIGN_FLAGS="--timestamp --options=runtime" \
    build | tail -3

if [[ ! -d "$APP_PATH" ]]; then
    echo "Build did not produce $APP_PATH" >&2
    exit 1
fi

if ! command -v uv >/dev/null 2>&1; then
    echo "uv not found. Install with: brew install uv" >&2
    exit 1
fi

echo "==> Bundling standalone Python + mlx-whisper into the app"
APP_PY="$APP_PATH/Contents/Resources/python"
rm -rf "$APP_PY"

# Ensure a managed (relocatable) Python is installed via uv.
uv python install 3.11 >/dev/null
PY_SRC="$(uv python find 3.11 2>/dev/null | xargs -I{} dirname {} | xargs dirname)"
if [[ ! -d "$PY_SRC" ]] || [[ ! -f "$PY_SRC/bin/python3.11" ]]; then
    echo "Failed to locate managed Python install at $PY_SRC" >&2
    exit 1
fi

# Copy the standalone Python distribution into the bundle (resolving symlinks).
mkdir -p "$APP_PY"
rsync -aL "$PY_SRC/" "$APP_PY/"

# Mark this as a regular Python install rather than uv-managed, so pip works.
rm -f "$APP_PY/lib/python3.11/EXTERNALLY-MANAGED" 2>/dev/null || true

# Install runtime dependencies into the bundled Python via uv (fastest, ignores PEP 668).
uv pip install --python "$APP_PY/bin/python3" -r "$REPO_ROOT/sidecar/requirements.txt" >/dev/null
cp "$REPO_ROOT/sidecar/sidecar.py" "$APP_PY/sidecar.py"

echo "==> Downloading Whisper model (~1.5 GB) into the bundle"
APP_MODELS="$APP_PATH/Contents/Resources/models"
MODEL_REPO="${FASTWORD_BUNDLE_MODEL:-mlx-community/whisper-large-v3-turbo}"
MODEL_DIR_NAME="$(echo "$MODEL_REPO" | tr '/' '_')"
MODEL_LOCAL="$APP_MODELS/$MODEL_DIR_NAME"
mkdir -p "$APP_MODELS"
"$APP_PY/bin/python3" -c "
from huggingface_hub import snapshot_download
import sys
path = snapshot_download(repo_id='$MODEL_REPO', local_dir='$MODEL_LOCAL')
print(path)
" >/dev/null

# Strip caches, tests, and other non-runtime files to shrink the bundle.
find "$APP_PY" -type d \( -name "__pycache__" -o -name "tests" -o -name "test" -o -name "*.dist-info" \) -exec rm -rf {} + 2>/dev/null || true
find "$APP_PY" -type f \( -name "*.pyc" -o -name "*.pyo" \) -delete 2>/dev/null || true

# Pip writes console_scripts with absolute shebangs pointing at the build path
# (leaks /Users/<builder>/... and breaks on the user's machine anyway). We don't
# call any of these CLI scripts at runtime — sidecar invokes Python directly —
# so just delete everything in bin/ except the Python interpreters.
find "$APP_PY/bin" -type f ! -name "python*" -delete 2>/dev/null || true

# Python's _sysconfigdata embeds the build-time install prefix
# (~/.local/share/uv/python/...). Replace it with a neutral path so the bundle
# doesn't leak the builder's home directory.
SYSCFG="$APP_PY/lib/python3.11/_sysconfigdata__darwin_darwin.py"
if [[ -f "$SYSCFG" ]]; then
    UV_PY_PREFIX="$(uv python find 3.11 2>/dev/null | xargs dirname | xargs dirname)"
    if [[ -n "$UV_PY_PREFIX" ]]; then
        sed -i '' "s|$UV_PY_PREFIX|/usr/local|g" "$SYSCFG" 2>/dev/null || true
    fi
fi

echo "==> Signing every Mach-O binary inside the bundled Python (parallelized)"
# Find every dylib, .so, and executable file. Sign them in parallel (timestamp
# server is the slow link, so parallelism helps a lot).
find "$APP_PY" -type f \( -name "*.dylib" -o -name "*.so" -o -perm +111 \) -print0 \
    | xargs -0 -P 8 -n 10 codesign --force --sign "$DEV_ID_IDENTITY" \
        --timestamp --options=runtime 2>&1 | tail -5

# Re-sign the .app to seal in the new contents.
echo "==> Re-signing the .app"
codesign --force --sign "$DEV_ID_IDENTITY" \
    --timestamp --options=runtime \
    --entitlements "$REPO_ROOT/FastWord/Resources/FastWord.entitlements" \
    "$APP_PATH"

echo "==> Verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"

mkdir -p "$OUT_DIR"
rm -f "$DMG_PATH"

echo "==> Creating DMG"
TMP_DMG_DIR="$(mktemp -d)"
cp -R "$APP_PATH" "$TMP_DMG_DIR/"
ln -s /Applications "$TMP_DMG_DIR/Applications"
hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$TMP_DMG_DIR" \
    -ov \
    -format UDZO \
    "$DMG_PATH" >/dev/null
rm -rf "$TMP_DMG_DIR"

echo "==> Signing DMG"
codesign --sign "$DEV_ID_IDENTITY" --timestamp "$DMG_PATH"

echo "==> Submitting to Apple notary service (this can take a few minutes)"
xcrun notarytool submit "$DMG_PATH" \
    --keychain-profile "$NOTARY_PROFILE" \
    --wait

echo "==> Stapling notarization ticket"
xcrun stapler staple "$DMG_PATH"

echo
echo "Done."
echo "  $DMG_PATH"
echo
echo "Verify on a clean machine:"
echo "  spctl -a -vvv -t install '$DMG_PATH'"
echo
echo "Upload to GitHub Releases:"
echo "  gh release create v$VERSION '$DMG_PATH' --title 'FastWord $VERSION' --generate-notes"
