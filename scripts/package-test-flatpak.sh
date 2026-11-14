#!/usr/bin/env bash
# Install a local candidate and test the *real* host player bridge in its sandbox.
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
[[ $# == 1 ]] || { echo "Usage: $0 path/to/unsigned.flatpak" >&2; exit 2; }
[[ ${XDG_RUNTIME_DIR:-} == "/run/user/$(id -u)" ]] || {
  echo "Run in a desktop session with canonical XDG_RUNTIME_DIR=/run/user/$(id -u)" >&2
  exit 1
}
if [[ -z ${DBUS_SESSION_BUS_ADDRESS:-} ]]; then
  exec dbus-run-session -- "$0" "$@"
fi
mkdir -p target/packaging
STATE=$(mktemp -d "$ROOT/target/packaging/player-smoke.XXXXXXXX")
trap 'rm -rf -- "$STATE"' EXIT
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo test --locked -p kadr-media --test players_smoke --no-run \
  --message-format=json > "$STATE/artifacts.json"
TEST_BINARY=$(python3 - "$STATE/artifacts.json" <<'PY'
import json, sys
print(next(d["executable"] for line in open(sys.argv[1])
           if (d := json.loads(line)).get("executable")))
PY
)
# Flatpak 1.14 reports "already installed" even with --reinstall on a bundle.
# Compare the actual commit, not just the app ID, before reusing an install.
ostree --repo="$STATE/repo" init --mode=archive-z2
flatpak build-import-bundle --no-update-summary "$STATE/repo" "$1"
EXPECTED=$(ostree --repo="$STATE/repo" rev-parse app/dev.kadr.Kadr/x86_64/stable)
INSTALLED=$(flatpak --user info --show-commit dev.kadr.Kadr 2>/dev/null || true)
if [[ "$INSTALLED" != "$EXPECTED" ]]; then
  flatpak --user install -y --noninteractive --or-update "$1"
fi
test "$(flatpak --user info --show-commit dev.kadr.Kadr)" = "$EXPECTED"
# Only this test executable's directory is exposed, read-only, for this launch.
# The installed application's normal permissions remain the manifest's.
flatpak run --filesystem="$(dirname "$TEST_BINARY")":ro --command="$TEST_BINARY" \
  dev.kadr.Kadr --ignored --nocapture
