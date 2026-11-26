#!/usr/bin/env bash
# Sourced by package-* scripts; all output stays inside the ignored target/.
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
# shellcheck disable=SC2034 # Used by the scripts sourcing this shared setup.
VERSION=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')
BINARY=
if [[ ${1:-} == --binary && $# == 2 ]]; then
  BINARY=$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$2")
elif [[ $# != 0 ]]; then
  echo "Usage: $0 [--binary path/to/kadr-app]" >&2; exit 2
fi
if [[ -z "$BINARY" ]]; then
  export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
  cargo build --locked --release -p kadr-app
  BINARY="$ROOT/target/release/kadr-app"
fi
test -x "$BINARY"
if [[ $(uname -s) == Linux ]]; then
  python3 - "$BINARY" <<'PY'
import struct, sys
with open(sys.argv[1], "rb") as binary:
    header = binary.read(20)
if header[:6] != b"\x7fELF\x02\x01" or struct.unpack("<H", header[18:20])[0] != 62:
    sys.exit("Expected a little-endian ELF64 x86_64 binary")
PY
fi
OUT="$ROOT/target/dist"
mkdir -p "$OUT" "$ROOT/target/packaging"
STAGE=$(mktemp -d "$ROOT/target/packaging/stage.XXXXXXXX")
trap 'rm -rf -- "$STAGE"' EXIT
LICENSES="$STAGE/licenses"
case ${KADR_ALLOW_INCOMPLETE_NOTICES:-0} in
  0)
    python3 scripts/package-licenses.py --binary "$BINARY" --output "$LICENSES" --strict
    ;;
  1)
    if [[ -n ${CODESIGN_IDENTITY:-} || -n ${NOTARYTOOL_PROFILE:-} ]]; then
      echo "Incomplete-notice QA mode is allowed only for unsigned local builds" >&2
      exit 1
    fi
    echo "UNSIGNED LOCAL QA ONLY: incomplete notices are explicitly allowed; NOT FOR DISTRIBUTION" >&2
    python3 scripts/package-licenses.py --binary "$BINARY" --output "$LICENSES"
    ;;
  *)
    echo "KADR_ALLOW_INCOMPLETE_NOTICES must be 0 or 1" >&2
    exit 2
    ;;
esac
NOTICE_STATUS=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["status"])' \
  "$LICENSES/third-party/manifest.json")
DECLARED_STANDARD_COUNT=$(python3 -c \
  'import json,sys; print(json.load(open(sys.argv[1]))["declared_standard_package_count"])' \
  "$LICENSES/third-party/manifest.json")
notice_requirements() {
  if [[ "$NOTICE_STATUS" == needs-review ]]; then
    cat "$LICENSES/third-party/REVIEW.txt"
  else
    echo "NOTICE_STATUS: $NOTICE_STATUS"
    echo "NOTICE_DECLARED_STANDARD_PACKAGES: $DECLARED_STANDARD_COUNT"
    echo "Notice texts collected; not a complete SBOM, compatibility audit or distribution approval."
  fi
}
