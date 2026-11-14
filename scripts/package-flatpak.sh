#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || {
  echo "Flatpak currently supports native Linux x86_64 only" >&2; exit 1;
}
# shellcheck source=scripts/package-common.sh
source "$(dirname -- "$0")/package-common.sh"
desktop-file-validate packaging/dev.kadr.Kadr.desktop
appstream-util validate-relax --nonet packaging/dev.kadr.Kadr.metainfo.xml
cp packaging/flatpak/dev.kadr.Kadr.json "$STAGE/"
cp packaging/dev.kadr.Kadr.{desktop,metainfo.xml,svg} "$STAGE/"
install -m755 "$BINARY" "$STAGE/kadr-app"
# Offline build: runtime/SDK must already be installed from a verified remote.
flatpak-builder --user --disable-rofiles-fuse --force-clean \
  --state-dir="$ROOT/target/packaging/flatpak-state" \
  --repo="$STAGE/repo" --default-branch=stable \
  "$STAGE/build" "$STAGE/dev.kadr.Kadr.json"
ARTIFACT="$OUT/kadr-$VERSION-linux-x86_64-unsigned.flatpak"
flatpak build-bundle --arch=x86_64 "$STAGE/repo" "$ARTIFACT" dev.kadr.Kadr stable \
  --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo
{
  notice_requirements
  echo "Unsigned Linux x86_64, Freedesktop Platform/SDK 25.08."
  echo "Input ELF SHA-256:"
  sha256sum < "$BINARY"
  echo "Rust notice inventory SHA-256 (not a legal audit/SBOM):"
  sha256sum < "$LICENSES/third-party/manifest.json"
  echo "Build runtime commit:"
  flatpak --user info --show-commit org.freedesktop.Platform//25.08
  echo "Build SDK commit:"
  flatpak --user info --show-commit org.freedesktop.Sdk//25.08
  echo "Final package metadata and permissions:"
  cat "$STAGE/build/metadata"
} > "$ARTIFACT.requirements.txt"
(cd "$OUT"; sha256sum "$(basename "$ARTIFACT")" > "$(basename "$ARTIFACT").sha256")
echo "$ARTIFACT"
