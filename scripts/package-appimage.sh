#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || {
  echo "AppImage currently supports native Linux x86_64 only" >&2; exit 1;
}
# shellcheck source=scripts/package-common.sh
source "$(dirname -- "$0")/package-common.sh"
for tool in patchelf desktop-file-validate appstream-util curl sha256sum mksquashfs; do
  command -v "$tool" >/dev/null
done
desktop-file-validate packaging/dev.kadr.Kadr.desktop
appstream-util validate-relax --nonet packaging/dev.kadr.Kadr.metainfo.xml
APPDIR="$STAGE/kadr.AppDir"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" "$APPDIR/usr/share/metainfo" \
  "$APPDIR/usr/share/licenses/kadr"
install -m755 "$BINARY" "$APPDIR/usr/bin/kadr"
install -m755 packaging/AppRun "$APPDIR/AppRun"
cp packaging/dev.kadr.Kadr.{desktop,svg} "$APPDIR/"
cp packaging/dev.kadr.Kadr.metainfo.xml "$APPDIR/usr/share/metainfo/"
cp -R "$LICENSES/." "$APPDIR/usr/share/licenses/kadr/"
# Bundle the non-driver runtime closure, including GPUI's dlopen dependencies.
# Keep glibc, Vulkan loader/ICDs and graphics drivers on the target host.
python3 scripts/package-linux-libs.py "$APPDIR"
patchelf --set-rpath "\$ORIGIN/../lib" "$APPDIR/usr/bin/kadr"
for library in "$APPDIR"/usr/lib/*.so*; do
  patchelf --set-rpath "\$ORIGIN" "$library"
done
TOOLS="$ROOT/target/packaging/tools"
mkdir -p "$TOOLS"
download() {
  local name=$1 hash=$2 url=$3
  if [[ ! -f "$TOOLS/$name" ]] ||
    ! printf '%s  %s\n' "$hash" "$TOOLS/$name" | sha256sum --check --status; then
    curl --fail --location --retry 3 "$url" -o "$TOOLS/$name"
  fi
  printf '%s  %s\n' "$hash" "$TOOLS/$name" | sha256sum --check
}
download appimagetool-1.9.1 ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 \
  https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
# Upstream's runtime tag is mutable; checksum pin deliberately fails closed on update.
download runtime-x86_64 617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46 \
  https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64
chmod +x "$TOOLS/appimagetool-1.9.1"
ARTIFACT="$OUT/kadr-$VERSION-linux-x86_64-unsigned.AppImage"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOLS/appimagetool-1.9.1" \
  --runtime-file "$TOOLS/runtime-x86_64" --no-appstream --mksquashfs-opt -processors \
  --mksquashfs-opt 2 "$APPDIR" "$ARTIFACT"
{
  notice_requirements
  echo "Unsigned Linux x86_64; host Vulkan driver/loader and fonts required."
  echo "Input ELF SHA-256:"
  sha256sum < "$BINARY"
  echo "Required GLIBC symbols (binary plus bundled libraries):"
  readelf --version-info "$APPDIR/usr/bin/kadr" "$APPDIR"/usr/lib/*.so* |
    grep -oE 'GLIBC_[0-9.]+' | sort -Vu
  echo "Rust notice inventory SHA-256 (not a legal audit/SBOM):"
  sha256sum < "$APPDIR/usr/share/licenses/kadr/third-party/manifest.json"
  echo "Bundled library package versions (license notices are inside the image):"
  cat "$APPDIR/usr/share/licenses/kadr/third-party/native/BUNDLED-LIBRARIES.txt"
} > "$ARTIFACT.requirements.txt"
(cd "$OUT"; sha256sum "$(basename "$ARTIFACT")" > "$(basename "$ARTIFACT").sha256")
echo "$ARTIFACT"
