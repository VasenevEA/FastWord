#!/usr/bin/env bash
# Build, sign, notarize, and package FastWord as a distributable DMG.
#
# Prerequisites (one-time setup):
#   1. Apple Developer Program membership.
#   2. "Developer ID Application" certificate installed in your login keychain.
#   3. App Store Connect API key (.p8) in .secrets/.
#   4. Store credentials once:
#        xcrun notarytool store-credentials FASTWORD_NOTARY \
#            --key .secrets/AuthKey_XXXXXXXXXX.p8 \
#            --key-id  "XXXXXXXXXX" \
#            --issuer  "<issuer-uuid>"
#   5. Rust toolchain (`rustup` + the `aarch64-apple-darwin` target).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

NOTARY_PROFILE="${FASTWORD_NOTARY_PROFILE:-FASTWORD_NOTARY}"
MODEL_FILE="ggml-large-v3-turbo-q5_0.bin"
MODEL_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/${MODEL_FILE}"
MODEL_CACHE="$HOME/Library/Caches/fastword/models/$MODEL_FILE"

if [[ ! -f LocalConfig.xcconfig ]]; then
    echo "LocalConfig.xcconfig not found. See LocalConfig.xcconfig.example." >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found. Install Rust via https://rustup.rs/" >&2
    exit 1
fi

# Read version from project.yml (source of truth) — Info.plist is generated
# later by xcodegen, so reading it here would pick up a stale value.
VERSION="$(grep CFBundleShortVersionString project.yml | head -1 \
    | sed -E 's/.*"(.*)".*/\1/')"
[[ -z "$VERSION" ]] && VERSION="0.2.0"

OUT_DIR="$REPO_ROOT/build/release"
DERIVED="$REPO_ROOT/build/release-derived"
APP_NAME="FastWord"
APP_PATH="$DERIVED/Build/Products/Release/$APP_NAME.app"
DMG_PATH="$OUT_DIR/$APP_NAME-$VERSION.dmg"

echo "==> Locating Developer ID Application certificate"
DEV_ID_IDENTITY="$(security find-identity -v -p codesigning \
    | grep -m1 'Developer ID Application' \
    | sed -E 's/.*"(.*)".*/\1/' || true)"
if [[ -z "$DEV_ID_IDENTITY" ]]; then
    echo "No 'Developer ID Application' certificate found in keychain." >&2
    exit 1
fi
echo "    using: $DEV_ID_IDENTITY"

echo "==> Building Rust sidecar (release)"
( cd sidecar-rust && cargo build --release --target aarch64-apple-darwin )
RUST_BIN="$REPO_ROOT/sidecar-rust/target/aarch64-apple-darwin/release/fastword-sidecar"
if [[ ! -f "$RUST_BIN" ]]; then
    echo "Rust sidecar not found at $RUST_BIN" >&2
    exit 1
fi
ls -lh "$RUST_BIN"

echo "==> Ensuring Whisper Q5 model is downloaded"
mkdir -p "$(dirname "$MODEL_CACHE")"
if [[ ! -f "$MODEL_CACHE" ]]; then
    echo "    fetching from $MODEL_URL ..."
    curl -L --fail --progress-bar -o "$MODEL_CACHE" "$MODEL_URL"
fi
ls -lh "$MODEL_CACHE"

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

echo "==> Bundling Rust sidecar + Whisper model"
RES="$APP_PATH/Contents/Resources"
mkdir -p "$RES/models"
cp "$RUST_BIN" "$RES/fastword-sidecar"
chmod +x "$RES/fastword-sidecar"
cp "$MODEL_CACHE" "$RES/models/$MODEL_FILE"

echo "==> Signing the Rust sidecar"
codesign --force --sign "$DEV_ID_IDENTITY" \
    --timestamp --options=runtime \
    "$RES/fastword-sidecar"

echo "==> Re-sealing the .app"
codesign --force --sign "$DEV_ID_IDENTITY" \
    --timestamp --options=runtime \
    --entitlements "$REPO_ROOT/FastWord/Resources/FastWord.entitlements" \
    "$APP_PATH"

echo "==> Verifying signature"
codesign --verify --strict --verbose=2 "$APP_PATH"

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

echo "==> Submitting to Apple notary service"
xcrun notarytool submit "$DMG_PATH" \
    --keychain-profile "$NOTARY_PROFILE" \
    --wait

echo "==> Stapling notarization ticket"
xcrun stapler staple "$DMG_PATH"

echo
echo "Done."
ls -lh "$DMG_PATH"
echo
echo "Verify on a clean machine:"
echo "  spctl -a -vvv -t install '$DMG_PATH'"
echo
echo "Upload to GitHub Releases (only with explicit approval):"
echo "  gh release create v$VERSION '$DMG_PATH' --title 'FastWord $VERSION' --generate-notes"
