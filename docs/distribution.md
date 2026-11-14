# Distribution and release candidates

Package identity is **`dev.kadr.Kadr`** on Linux and macOS. The executable inside
packages is `kadr`; Cargo's executable is `kadr-app`. Packaging reads the version
from the local workspace, never clones/uploads source, and writes only to ignored
`target/packaging/` and `target/dist/`. Do not commit generated packages or icons.
Project code, desktop assets and icon are GPL-3.0-only; AppStream metadata is
CC0-1.0. Rust dependency notices and GPUI provenance accompany every format;
the AppImage also carries the bundled native-library notices.

These are **unsigned release candidates**, not production-signed releases.
The reusable/manual `release.yml` workflow builds candidates but does not create
GitHub releases, push refs, publish packages, or run on tags. Workflow artifact
retention is off by default. Dispatching **Build and test** with
`retain_artifacts=true` first requires both platform test jobs to pass, then
builds optimized AppImage/Flatpak and arm64 DMG candidates and retains them for
seven days. Artifact names include the exact Git SHA; package filenames contain
the workspace version, target and unsigned status. Checksums and dependency/
notice requirements accompany each package. The same authorized run retains
only the smoke harnesses' own empty application-window screenshots for visual
review, never a desktop capture or a real user's browsing/player session.

```sh
gh workflow run ci.yml --ref mvp/full-desktop -f retain_artifacts=true
```

This works on a feature branch without merging it into the default branch.
Ordinary pushes and pull requests run validation but do not upload candidates.

## Supported targets and minimums

| Format | Target | Minimum / evidence |
| --- | --- | --- |
| AppImage | Native Linux x86_64, little-endian ELF64 | Intended baseline glibc 2.35 (Ubuntu 22.04 build runner). Actual candidate's complete GLIBC symbol list and bundled package versions are emitted in `.requirements.txt`; a newer build host can raise this floor. Tested locally on Debian 12/glibc 2.36, not on every glibc 2.35 distribution. |
| Flatpak | Native Linux x86_64 | Flatpak 1.14+ tooling tested; Freedesktop Platform/SDK **25.08**. Runtime supplies userspace ABI and GPU extension; host must support Flatpak, graphics and its required kernel features. No host glibc floor is inferred from the AppImage. |
| DMG | Native macOS arm64 (Apple Silicon) | **macOS 13.0** deployment target and `LSMinimumSystemVersion`. Script verifies Mach-O arm64, `vtool` minos ≤13.0, and rejects non-system dylibs. This is a declared minimum, **not a macOS 13 runtime test result**; CI uses macOS 15 arm64. Intel/universal DMGs are not supported by these scripts. |

Linux GPUI uses Vulkan, not a bundled browser engine. Blade 0.7.1's adapter
inspection requires Vulkan ≥1.1 **and its required device extensions/features**;
the version number alone does not guarantee compatibility. Install a working
host Vulkan loader/ICD (for example Mesa or the vendor driver), fontconfig
configuration, fonts, and an X11 or Wayland graphical session. Fontconfig,
Wayland client/cursor/EGL, X11/XCB and XKB libraries are bundled into the AppImage;
glibc, Vulkan loader/ICDs and graphics drivers are deliberately not bundled.
Flatpak provides these libraries and driver extensions through its runtime.
mpv/VLC are **not bundled in either format** and must be installed on the host.

Do not build with `-C target-cpu=native` when making portable candidates.
The scripts refuse the wrong OS/architecture; they do not cross-compile or
magically lower a binary's ABI floor. `--binary` packages the supplied build
without recompiling it, which is useful for smoke checks but is not proof that a
release-optimized integrated build has passed.

## Linux build prerequisites

Use Rust stable with the checked-in lockfile, Python ≥3.11, and Debian/Ubuntu
build hosts for the AppImage dependency/license collector. Source Rust's
environment when necessary:

```sh
. "$HOME/.cargo/env"
export CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential pkg-config clang cmake libssl-dev \
  libfontconfig1-dev libwayland-dev libxcb1-dev libx11-xcb-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libxcb-xkb-dev \
  libxcb-randr0-dev libxcb-xfixes0-dev libxcb-shape0-dev \
  libxcb-sync-dev libxcb-render0-dev libxcb-render-util0-dev \
  libxcb-composite0-dev libxcb-damage0-dev libxcb-present-dev \
  libvulkan1 mesa-vulkan-drivers fonts-dejavu-core \
  libwayland-cursor0 libwayland-egl1 \
  flatpak flatpak-builder ostree elfutils appstream-util desktop-file-utils \
  patchelf squashfs-tools curl xvfb openbox xdotool wmctrl mpv vlc dbus-x11

flatpak --user remote-add --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak --user install -y --noninteractive flathub \
  org.freedesktop.Platform//25.08 org.freedesktop.Sdk//25.08

cargo build --locked --release -p kadr-app
scripts/package-appimage.sh --binary target/release/kadr-app
scripts/package-flatpak.sh --binary target/release/kadr-app
```

Without `--binary`, either packaging script first performs the exact release
build above. Flatpak packaging installs an already-built local ELF into the
runtime; it is not a remote-source Flathub submission recipe. Use the SDK
matching the manifest runtime. Its modern `appstreamcli compose` is invoked
explicitly, so Debian 12's older flatpak-builder does not require the removed
`appstream-compose` program.

Both Linux formats emit `.sha256` and `.requirements.txt` sidecars, including
the input ELF hash. The Flatpak requirements record its runtime/SDK commits
and final exported permissions; those runtime branches receive signed updates
from the configured remote rather than being silently treated as immutable.

AppImage tooling: appimagetool **1.9.1** and the embedded type-2 runtime are
downloaded from public upstream GitHub assets and checked against SHA-256 pins
in the script **before execution/embedding**. The runtime upstream tag is
mutable: a changed asset fails closed; review and replace its checksum
deliberately, never bypass verification. No FUSE daemon is needed to package.
The tool itself runs in extract-and-run mode.

The AppImage uses ELF RPATH, not exported `LD_LIBRARY_PATH`, so its libraries do
not contaminate host mpv/VLC. It includes Debian/Ubuntu dependency copyright
files and a package/version inventory. Before public redistribution, review all
third-party notices and fulfill corresponding-source/relinking obligations
(including any LGPL dependencies); generating an unsigned candidate is not a
license-compliance sign-off.

### Rust, vendored GPUI and native notices

All three packagers run `scripts/package-licenses.py` against
`cargo metadata --offline --locked --format-version 1`, filtered to the native
Rust target. Cargo and already-cached dependency sources are required even with
`--binary`; the five declared-standard cases also require their cached `.crate`
archives. Collection never downloads missing sources or executes build scripts.
Use the checkout/lockfile that actually produced the supplied executable.

The collector follows kadr-app's non-dev normal/build dependency closure,
recursively copies local LICENSE/LICENCE/COPYING/NOTICE/COPYRIGHT/AUTHORS/OFL
texts (including license directories), and excludes build/cache directories.
It emits a versioned JSON inventory with SPDX expressions as declared by Cargo,
authors, versions, public source/repository URLs, dependency edges/features,
text hashes, and the input executable, Cargo.lock and manifest hashes. Workspace
crates without their own declaration explicitly refer to the root GPL-3.0-only
project policy; upstream declarations are not rewritten.

When a published crate omits its workspace notices, checked-in overrides under
`packaging/licenses/<crate>-<version>/source.json` preserve actual public upstream
texts. The offline collector checks exact package/version/license expression,
published `.cargo_vcs_info.json` commit/path, pinned-commit URL and SHA-256 before
using one. Shared texts are content-addressed. A few raw notices intentionally
lack a final newline: their `json-utf8` storage encodes the exact UTF-8 text, and
the hash is checked against the decoded bytes embedded in the package.
These source-file supplements never invent copyright attributions.

Five separately labeled `declared-spdx` records cover only `convert_case 0.4.0`,
`hexf-parse 0.2.1`, `simd_helpers 0.1.0`, `seahash 4.1.0`, and
`pathfinder_geometry 0.5.1`. Their exact published archives must match the
checked-in record **and Cargo.lock SHA-256**; published normalized/original
manifests and metadata must agree on the recognized grant. The collector
preserves actual published metadata/README/authors and source copyright/license
comment notices, then supplies canonical MIT/CC0/Apache terms from SPDX
license-list-data v3.26.0 commit
`8a04f0303d95c4f932210b118588c662665bd383`, with pinned raw hashes.
Instructional copyright placeholders are omitted from embedded terms and that
transformation is recorded—no year, owner or permission is invented.
Each case is marked **`declared-standard-text`**, not as a recovered upstream
LICENSE or human legal approval. Unknown/custom/NOASSERTION declarations,
conflicts, unreviewed versions and checksum failures still block collection.
The [decision record](../packaging/licenses/POLICY.md) explains this refinement
of the initial overly strict text-file-only rule and is embedded in each format.

For vendored GPUI, every format includes its `LICENSE-APACHE`, the complete
`docs/adr-gpui-x11.md` provenance, and an explicit modification notice. AppImage
also includes each bundled Debian/Ubuntu library's package copyright file,
package/version inventory and referenced `/usr/share/common-licenses` texts.
Locations:

- AppImage: `usr/share/licenses/kadr/third-party/`
- Flatpak: `/app/share/licenses/kadr/third-party/`
- macOS: `kadr.app/Contents/Resources/licenses/third-party/`

`manifest.json` and `REVIEW.txt` expose missing text/metadata as **needs-review**,
never as a successful legal audit. All three packagers use `--strict` and fail
closed on unresolved gaps. To run the same gate separately:

```sh
python3 scripts/package-licenses.py --binary target/release/kadr-app \
  --output target/packaging/license-review --strict
python3 scripts/package-licenses-test.py
```

The original 43 text-file gaps are covered by 38 commit/hash-verified source-file
supplements and five authenticated declared-standard records. Normal strict
commands accept those recognized grants without needing the QA override; the
overall status remains `texts-collected-not-audit`. Package manifests and
requirements explicitly count the declared-standard packages. This is notice
collection, not a representation that upstream shipped otherwise absent files.

For explicitly authorized **unsigned local QA only**, a developer can choose:

```sh
KADR_ALLOW_INCOMPLETE_NOTICES=1 scripts/package-appimage.sh --binary target/debug/kadr-app
KADR_ALLOW_INCOMPLETE_NOTICES=1 scripts/package-flatpak.sh --binary target/debug/kadr-app
# On macOS, for local DMG mount/native UI QA:
KADR_ALLOW_INCOMPLETE_NOTICES=1 scripts/package-dmg.sh --binary target/debug/kadr-app
```

This is never an automatic fallback. It preserves all found notices and emits
prominent `NOTICE_STATUS: needs-review`, missing-package counts and the exact
warnings in the embedded report and `.requirements.txt`. Such package files are
**not license-cleared and must not be distributed or uploaded**. Developer-ID
signing/notarization environment variables are rejected in this QA mode; an
ad-hoc macOS signature still conveys no signing identity. The release workflow
explicitly forces strict mode and independently rejects incomplete notice
statuses before any user-authorized workflow artifact upload, regardless of the
ambient QA flag. A separate native CI smoke job may deliberately use QA mode
without uploading its packages, but must not describe that as legal clearance.
Omit `--strict` on the collector itself only for a local audit inventory.

This is a conservative notice inventory, **not a complete linked-code SBOM**.
Cargo feature unification and build dependencies can overinclude crates; native
vendored sources, generated code, embedded assets and packaging runtimes require
additional review. Hash association is not an attestation that an arbitrary
`--binary` was compiled from this checkout. Resolve all reported gaps and review
license compatibility, corresponding-source offers, relinking and attribution
obligations before public distribution; candidate generation alone proves none
of those conclusions.

## Install and smoke-test Linux candidates

In a real desktop session with system/session D-Bus, fonts, graphics, host
mpv/VLC and canonical `XDG_RUNTIME_DIR=/run/user/$(id -u)`:

```sh
(cd target/dist && sha256sum -c kadr-0.1.0-linux-x86_64-unsigned.AppImage.sha256)
chmod +x target/dist/kadr-0.1.0-linux-x86_64-unsigned.AppImage
target/dist/kadr-0.1.0-linux-x86_64-unsigned.AppImage
# On machines without usable FUSE:
APPIMAGE_EXTRACT_AND_RUN=1 target/dist/kadr-0.1.0-linux-x86_64-unsigned.AppImage

flatpak --user install -y target/dist/kadr-0.1.0-linux-x86_64-unsigned.flatpak
flatpak run dev.kadr.Kadr
flatpak --user info --show-permissions dev.kadr.Kadr

APPIMAGE_EXTRACT_AND_RUN=1 python3 scripts/smoke-x11.py \
  target/dist/kadr-0.1.0-linux-x86_64-unsigned.AppImage
cargo test --locked -p kadr-media --test flatpak_player --test players
scripts/package-test-flatpak.sh target/dist/kadr-0.1.0-linux-x86_64-unsigned.flatpak
```

Substitute the generated version if it changes. The final script installs the
local bundle (or verifies the already-installed commit is identical), builds
only the media smoke-test executable, exposes that executable's directory
read-only for a single test invocation, and runs the existing real mpv/VLC
HTTP/resume/telemetry/stop/EOF checks **inside the installed Flatpak**. The
one-run test filesystem override is not part of the normal manifest.
The helper always creates its **own private session bus**, even when invoked
from a desktop or an outer `dbus-run-session`; it never stops an existing desktop
bus or its services. Activated private-bus services inherit a regular log file,
not the caller's PTY. After the command exits or is interrupted, bounded
TERM/KILL cleanup covers only the helper's newly created process group, including
services whose original launcher already exited. This prevents an orphaned
accessibility/document service from holding the terminal open after the footer.
`python3 scripts/package-private-bus-test.py` exercises real D-Bus activation,
PTY EOF, cancellation and preservation of an existing separate bus without a
Cargo/GPUI build. A working **system bus** must already exist.
Headless CI/cloud containers may also need their normal `/run/user/UID`
directory provisioned; a nonstandard host runtime directory is not a substitute
because the host and sandbox IPC paths must match.

### Flatpak host-player trust boundary

The manifest deliberately grants:

- `--talk-name=org.freedesktop.Flatpak`: `flatpak-spawn --host` executes a
  separately installed player with the user's **host privileges**. This is
  broad host-command trust, not a narrowly sandboxed media permission.
- `--filesystem=xdg-run/kadr:create`: only the `kadr` runtime subdirectory is
  shared for player Unix sockets, not all host `/tmp`. Home access is a separate
  permission below.
- `--filesystem=home`: **read/write access to the user's home directory**, not
  just Videos or Downloads. The native save path needs sibling temporary files
  and a hardlink-based atomic no-clobber commit. A single-file document-portal
  grant does not support that hardlink operation. This permission is deliberate
  additional trust; it is not a narrowly isolated save-file capability.
- `--share=network`: host players can reach the sandbox's loopback HTTP
  endpoint. Bearer URLs remain private capabilities; never log or paste them.
- Wayland/fallback X11, IPC sharing and DRI for GPUI's window and Vulkan renderer.

When `FLATPAK_ID` is present on Linux, the media player path uses
`flatpak-spawn --host --watch-bus <executable> <player argv> -- <media>` with
**argv, never a shell**. The shared runtime parent is created with mode 0700,
opened with `O_NOFOLLOW|O_DIRECTORY`, checked for this UID and no group/world
writes, and tightened to 0700 through that descriptor before creating a
separate 0700 session directory. (Flatpak's `:create` initially creates the
parent as 0755.) Symlinks, untrusted ownership, unsafe permissions, missing
runtime directory and overlong Unix socket paths fail rather than falling
back to sandbox-private `/tmp`. The exact same socket pathname is passed to
the host player. Normal shutdown sends quit over IPC and waits for the bridge.
`--watch-bus` requests host termination with SIGINT when the bridge exits.
**Limitation:** Tokio can force-kill/reap the direct child (the bridge), not the
separately privileged host PID. A custom/broken host player that ignores both
IPC quit and SIGINT can survive forced bridge shutdown; this is not a host
process-group SIGKILL guarantee. Real mpv/VLC normal shutdown and EOF were tested,
but arbitrary hostile executables are outside that guarantee.

### Flatpak permanent saves

The chooser still uses the normal system portal. For home destinations already
accessible under the installed manifest, the portal can return the original
native URI rather than a single-file document-FUSE grant. This preserves atomic
save/no-clobber semantics; the app does not fall back to a partial final write.
Home-based Videos/Downloads destinations are supported. External/removable
directories are **not** granted by this manifest: grant the specific destination
directory explicitly if needed, otherwise expect a permission error. Do not
assume an individual file chooser grant is enough for an atomic hardlink save.

Source review of xdg-desktop-portal 1.16 confirms that SaveFile's
`AS_NEEDED_BY_APP` export can retain the native URI when the installed app already
has access, and document-FUSE hardlinks require a directory grant. An actual
cache helper inside the installed Flatpak failed for an ungranted home path,
then succeeded with a home permission: exact 200,000-byte content verified on
the host, repeat save rejected as AlreadyExists, cache clear left the permanent
file intact, and no `.part` file remained. After rebuilding/installing the home
manifest, the same check passed **without any one-run filesystem override**;
`flatpak info --file-access` reported read-write under the installed policy.
**The interactive portal chooser was not run in this cloud environment**, which
lacked portal services. The helper proves the filesystem/API boundary, not the
actual graphical chooser; the final manifest also makes home access persistent
for portal access checks.

Native AppImage/macOS builds continue to launch the player directly with
private `/tmp` IPC. Browser playback is the parent's portal/system-browser
integration, **not a bundled browser or `flatpak-spawn` shell**. The browser
receives the local HTTP URL through that launcher; this packaging work does
not assert browser codec support or persist browser playback telemetry.

## macOS DMG and optional external signing

Run on an Apple Silicon Mac with full Xcode selected (GPUI requires `metal`),
Rust stable and Python ≥3.11:

```sh
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
xcrun --find metal
export MACOSX_DEPLOYMENT_TARGET=13.0 CARGO_BUILD_JOBS=2
cargo build --locked --release -p kadr-app
scripts/package-dmg.sh --binary target/release/kadr-app
hdiutil attach target/dist/kadr-0.1.0-macos-arm64-unsigned.dmg -nobrowse -readonly
open /Volumes/kadr/kadr.app
# After quitting the app:
hdiutil detach /Volumes/kadr
```

The DMG contains `kadr.app`, icon, plist, license and `/Applications` shortcut.
The app has an ad-hoc signature because Apple Silicon needs it, **not** a
Developer ID signature; the artifact remains labeled `unsigned`. Gatekeeper
may block a downloaded unsigned app. Do not disable Gatekeeper globally.
Install mpv or VLC separately; the application must be able to resolve its
executable. Finder's PATH may omit Homebrew paths; launching the app's executable
from a configured terminal avoids that PATH mismatch while native player-path
discovery is being integrated. Packaging does not secretly install a player.

### Native macOS lifecycle and optional pixel smoke

The native smoke harness runs an existing executable, not `open` or a detached
launcher, so the observed window must belong to the actual child PID:

```sh
python3 scripts/smoke-macos.py --binary target/debug/kadr-app \
  --artifacts target/smoke-macos --screenshot
# Or exercise the executable inside a mounted candidate; use a NEW artifact dir:
python3 scripts/smoke-macos.py --binary /Volumes/kadr/kadr.app/Contents/MacOS/kadr \
  --artifacts target/smoke-macos-dmg --screenshot
```

Requires macOS, an active WindowServer session, Python and Xcode's `xcrun swiftc`.
It compiles `macos-window.swift` using only Foundation/CoreGraphics/ImageIO before
launching the application. Compilation is bounded to 30 seconds; the app run has
a separate 30-second maximum (or a smaller `--timeout`). Cleanup has bounded
TERM/KILL waits. `--smoke-ui` opens the initial empty state, emits the exact static
`kadr-ui-smoke: frame` marker on the GPUI frame callback, and uses normal Quit
after approximately five seconds. The harness requires that marker, a visible
layer-zero window owned by the child PID, and exit 0—not merely a live process.

HOME, CF's fixed user home, XDG state and temporary files are isolated in a
private directory and removed afterwards. Only safe initial-state logs,
`window.json`, `result.json` and an optional window screenshot remain in the new
artifact directory. No user source, credentials, certificate or network input
is supplied, and no Cargo build or source upload is performed.

`--screenshot` explicitly opts into `/usr/sbin/screencapture` for that window ID
only, with no other-window or desktop fallback. The helper then decodes the PNG
with ImageIO, checks dimensions and requires more than 16 distinct visible RGB
colors. Its JSON reports dimensions and a color-count **lower bound**, not an
exact count. This rejects a uniform/empty capture; it is not a semantic UI,
video-decoding or audio/video-sync assertion. Omitting `--screenshot` never
captures pixels and proves only the native window/frame-callback lifecycle.

Screen Recording or WindowServer restrictions can prevent a capture on a CI
runner. Requested capture failures fail the smoke honestly rather than turning
into a skip/pass or capturing the desktop. The script does not grant itself
permissions, change TCC settings or disable platform protections. On a local Mac,
use the normal macOS privacy settings if you choose to grant capture permission.
Swift compilation, native execution, backend-ready/frame markers and owned-window
pixel checks passed on macOS 15 arm64 in CI run `43554313461`, both for the
workspace debug binary and its mounted unsigned DMG. The two retained window
captures were visually reviewed and show readable Sources-screen text/controls.
This does not claim macOS 13 testing or native file-dialog/browser acceptance.

For an authorized signed candidate, supply `CODESIGN_IDENTITY` through your
external environment and an already-configured keychain. The script uses
hardened runtime and timestamps and labels the DMG
`developer-signed-unnotarized`. Optionally set `NOTARYTOOL_PROFILE` to an
**existing external keychain profile**; it submits to Apple, waits, staples and
validates, and only then yields a `notarized` candidate. This network submission
is opt-in via that environment, never enabled in the workflow. No certificates,
private keys, passwords, Apple tokens or keychain exports belong in this repo.
No signing/notarization was performed during Linux implementation.

## Recorded implementation evidence and limits

- Debian 12 x86_64: optimized integrated MVP `cargo build --release --locked -p
  kadr-app`, followed by strict AppImage and Flatpak builds, passed. The final
  input ELF SHA-256 recorded by both packages was
  `efe60e657988c3ef95a150771695dff80a278e7dd0fe2ac3eb8a02b65f30a8f7`.
  The AppImage closure's highest required GLIBC version was **2.35**;
  `.requirements.txt` and verified SHA-256 sidecars were generated.
- Optimized AppImage extract-and-run: private **native Xorg** map, resize,
  nonblack/nonuniform client pixels, WM close and Ctrl+Q all passed with exit 0.
  Native Wayland in nested Weston configured, attached/committed a non-null
  buffer and exited through Ctrl+Q with code 0. This is not a FUSE mount or
  hardware-driver matrix test.
- Flatpak bundle installed locally against Platform 25.08: window map, resize,
  WM close and Ctrl+Q exited 0 under isolated X11. Root-window screenshot
  inspection confirmed the rendered Sources screen and `dev.kadr.Kadr` WM_CLASS.
  The per-window capture had transparent alpha despite valid RGB pixels; use a
  root capture or remove alpha when inspecting that screenshot.
  Real media tests inside the installed sandbox passed for **host mpv and VLC**, proving shared IPC,
  sandbox-served loopback HTTP, resume, mpv telemetry, explicit stop and EOF.
- Normal integration tests cover both player argv, shared directory privacy,
  Flatpak's precreated 0755 directory, symlink/writable-parent rejection,
  missing runtime and overlong paths. Existing direct-player tests also pass.
- Actual native GTK open/save dialogs, visible decoded mpv video, Firefox
  local-stream playback/history, and permanent-save survival after GUI cache
  clearing are recorded separately in `docs/acceptance.md` and `DONE.md`.
- **Not claimed:** Apple signing/notarization, hardware GPU matrix,
  package manager/store publication, native macOS dialog/browser acceptance.
  Native macOS compilation/DMG/window results must come from a completed CI run,
  not from the existence of a workflow.
