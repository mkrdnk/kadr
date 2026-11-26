#!/usr/bin/env bash
set -euo pipefail
export MACOSX_DEPLOYMENT_TARGET=13.0
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || {
  echo "DMG currently supports native Apple Silicon macOS only" >&2; exit 1;
}
# shellcheck source=scripts/package-common.sh
source "$(dirname -- "$0")/package-common.sh"
lipo "$BINARY" -verify_arch arm64
# Refuse binaries depending on a developer's Homebrew/local installation.
otool -L "$BINARY" > "$STAGE/dylibs.txt"
python3 - "$STAGE/dylibs.txt" <<'PY'
import sys
for line in open(sys.argv[1]).readlines()[1:]:
    library = line.strip().split(" (")[0]
    if not library.startswith(("/usr/lib/", "/System/Library/")):
        sys.exit(f"Unbundled non-system library: {library}")
PY
# Info.plist cannot lower the minimum compiled into a Mach-O.
xcrun vtool -show-build "$BINARY" > "$STAGE/build-version.txt"
python3 - "$STAGE/build-version.txt" <<'PY'
import re, sys
versions = re.findall(r"minos\s+([0-9.]+)", open(sys.argv[1]).read())
if not versions or any(tuple(map(int, v.split("."))) > (13, 0, 0) for v in versions):
    sys.exit("Binary has unknown or newer-than-macOS-13 deployment target")
PY
APP="$STAGE/image/kadr.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m755 "$BINARY" "$APP/Contents/MacOS/kadr"
cp packaging/macos/Info.plist "$APP/Contents/"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
swift packaging/macos/icon.swift "$STAGE/kadr.iconset"
iconutil -c icns "$STAGE/kadr.iconset" -o "$APP/Contents/Resources/kadr.icns"
cp LICENSE "$APP/Contents/Resources/LICENSE"
cp -R "$LICENSES" "$APP/Contents/Resources/licenses"
ln -s /Applications "$STAGE/image/Applications"
LABEL=unsigned
if [[ -n ${CODESIGN_IDENTITY:-} ]]; then
  codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
  codesign --verify --strict --verbose=2 "$APP"
  LABEL=developer-signed-unnotarized
else
  # Apple Silicon requires an ad-hoc code signature; this conveys no identity.
  codesign --force --sign - "$APP"
fi
if [[ -n ${NOTARYTOOL_PROFILE:-} ]]; then
  [[ -n ${CODESIGN_IDENTITY:-} ]] || { echo "Notarization requires Developer ID" >&2; exit 1; }
  LABEL=notarized
fi
# Keep even a named "notarized" candidate out of dist until all checks pass.
ARTIFACT="$STAGE/kadr-$VERSION-macos-arm64-$LABEL.dmg"
hdiutil create -volname kadr -srcfolder "$STAGE/image" -ov -format UDZO "$ARTIFACT"
if [[ -n ${CODESIGN_IDENTITY:-} ]]; then
  codesign --timestamp --sign "$CODESIGN_IDENTITY" "$ARTIFACT"
fi
if [[ -n ${NOTARYTOOL_PROFILE:-} ]]; then
  xcrun notarytool submit "$ARTIFACT" --keychain-profile "$NOTARYTOOL_PROFILE" --wait
  xcrun stapler staple "$ARTIFACT"
  xcrun stapler validate "$ARTIFACT"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$ARTIFACT"
fi
hdiutil verify "$ARTIFACT"
DESTINATION="$OUT/$(basename "$ARTIFACT")"
mv "$ARTIFACT" "$DESTINATION"
ARTIFACT="$DESTINATION"
{
  notice_requirements
  cat "$STAGE/build-version.txt"
  echo "Input executable SHA-256:"
  shasum -a 256 < "$BINARY"
  echo "Rust notice inventory SHA-256 (not a legal audit/SBOM):"
  shasum -a 256 < "$LICENSES/third-party/manifest.json"
} > "$ARTIFACT.requirements.txt"
(cd "$OUT"; shasum -a 256 "$(basename "$ARTIFACT")" > "$(basename "$ARTIFACT").sha256")
echo "$ARTIFACT"
