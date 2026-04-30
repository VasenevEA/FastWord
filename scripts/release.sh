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
