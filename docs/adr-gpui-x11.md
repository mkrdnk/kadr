# ADR: GPUI 0.2.2 local compatibility patches

## Current status: registry dependency, local patches inactive

The workspace now uses the published `gpui = "=0.2.2"` directly from crates.io,
without `[patch.crates-io]`. Its `Cargo.lock` entry records the registry source
and published archive checksum. No other dependency versions were changed.
The local patches described below are **not** part of the current build.

The initial registry migration was built with Rust/Cargo 1.99.1 on Fedora 43.
The real application passed `--smoke-ui` startup, frame callback and exit code 0
in the available native Wayland and XWayland sessions, using isolated XDG
data/config/cache/state directories. An ImageMagick capture of only the
XWayland client showed 1,573 colors, mean 0.138139 and standard deviation
0.0980076; visual inspection confirmed readable application controls and text.
This was not the private native Xorg test below and did not check resize or
window-manager close. The Wayland frame callback is not a pixel assertion.

The published crate still lacks both fixes below. Successful compilation on
Rust 1.99.1 does not establish compatibility with later compilers; working
XWayland pixels on Fedora do not disprove the Debian native X11 failure.
There is no equivalent public GPUI application option for choosing the initial
X11 visual.

`vendor/gpui` is retained as an **inactive fallback**, excluded from the workspace.
Do not remove it or its conditional packaging notices until the native X11
pixel/lifecycle and Wayland replacement checks below have been repeated.
The current environment lacks Xorg/Xvfb, Openbox, Weston, xdotool and wmctrl,
so those scripted checks could not run. Native macOS validation also remains
required before declaring platform parity.

The remaining sections record the previous local-patch decision, its evidence
and how to reproduce or temporarily restore it; they do not describe the
currently selected dependency.

## Previous decision

Use a local copy of the published `gpui = 0.2.2` crate in `vendor/gpui`.
The source changes are:

- `src/platform/linux/x11/window.rs`, `X11WindowState::new`: choose
  `visual_set.opaque` instead of `visual_set.transparent`, adjust the fallback
  warning, and explain the patch inline.
- `src/taffy.rs`, `Style::to_taffy`'s `to_grid_repeat` helper: explicitly type
  the `minmax(0, 1fr)` literals as `0.0_f32` and `1.0_f32`. This preserves the
  existing grid sizing without relying on the compiler's deprecated float
  fallback ([Rust issue 154024](https://github.com/rust-lang/rust/issues/154024)).
  No lint is suppressed and no dependency is changed.

The X11 change matches the existing initial `BladeSurfaceConfig { transparent: false }`
and `WindowBackgroundAppearance::Opaque`. kadr uses opaque windows. Do not use
this patch as a general implementation of transparent X11 windows: the visual
is fixed at creation, while upstream's background-appearance API can later
request transparency. Supporting that mode requires a separate design and test.
No Wayland/macOS backend, shader, event-loop, or application code is changed.

The integrating workspace must select the local crate at its root:

```toml
[patch.crates-io]
gpui = { path = "vendor/gpui" }
```

Keep the workspace's existing exact `gpui = "=0.2.2"` dependency. Update the
workspace lockfile through Cargo when integrating; a path-selected GPUI entry
does not retain the registry source/checksum fields. Do not mutate the global
Cargo registry, patch sources from a build script, or replace other dependencies.

## Evidence and scope

On Debian 12, Xvfb and Xorg with the dummy driver and Mesa 22.3.6 software
Vulkan, unmodified GPUI creates a mapped, resizable native X11 window with an
entirely black client. Window lifecycle success alone did not reveal this bug.
The same application renders correctly under native Wayland in nested Weston.

`find_visuals` distinguishes an opaque RGB visual from a depth-32 visual with
an alpha mask. Upstream unconditionally chooses the latter when available, even
though the initial Blade surface is opaque. Changing only that selection and
its warning makes the same binary's rendered content visible. This establishes
the failing configuration and a working correction; it does not establish a
general Mesa driver bug or claim that all ARGB Vulkan surfaces are broken.

Validation used isolated copied dependencies and scratch manifests, with the
workspace lockfile as the starting resolution (`libc = 0.2.186`, `mio = 1.2.4`).
No global registry source or parent application binary was modified.
Both probes used GPUI's default feature set, including X11 and Wayland:

1. A minimal application titled `kadr`, with a dark background, readable text,
   and three rounded colored tiles. Unmodified GPUI on Xvfb and Xorg produced
   a 1000×720 client PNG with **1 color, mean 0**. The opaque selection produced
   **235 colors, mean 0.131919, standard deviation 0.115557** on both servers.
   The text and tiles were manually inspected, not inferred from statistics.
2. A scratch host of the actual `kadr_ui::KadrView::new()` (the MVP Sources
   screen, without its backend controller). Patched GPUI on Xvfb and Xorg
   produced a 1000×720 client PNG with **1,478 colors, mean 0.14178,
   standard deviation 0.0980458**. Manual inspection confirmed legible Cyrillic
   labels, sidebar, controls and panels, rather than black plus resize strips.
3. Both patched probes passed Xorg map, resize, client-pixel assertions,
   `WM_DELETE_WINDOW`, Ctrl+Q, and exit code 0. They also passed the Xvfb
   lifecycle test; that older test itself does not assert pixels.
4. Both patched probes passed nested native Wayland configure, non-null buffer
   attach/commit, and Ctrl+Q with exit code 0. Screenshots were manually
   inspected; the actual MVP screen remained legible.

Environment: Debian version file `12.2`; `mesa-vulkan-drivers` and
`libgl1-mesa-dri` `22.3.6-1+deb12u2`; Xorg/Xvfb
`2:21.1.7-3+deb12u13`; Weston `10.0.1-1+b1`; Rust `1.100.0`.
The test environment had no hardware GPU. macOS was **not** compiled or run.
Full application/backend behavior remains an integration check; the probes
validate rendering and platform lifecycle, not every MVP operation.

## Reproduce after integration

Build the real app using the integrated local dependency, then exercise that
exact executable (scripts do not build it):

```sh
cargo build --locked -p kadr-app
python3 scripts/smoke-x11.py target/debug/kadr-app \
  --screenshot /tmp/kadr-xvfb-client.png
identify -format '%wx%h colors=%k mean=%[fx:mean]\n' /tmp/kadr-xvfb-client.png
python3 scripts/smoke-xorg.py --binary target/debug/kadr-app \
  --artifacts /tmp/kadr-xorg-opaque
python3 scripts/smoke-wayland.py --binary target/debug/kadr-app \
  --artifacts /tmp/kadr-wayland-opaque --screenshot
```

Use fresh artifact-directory names for each run. Inspect the X11 client PNGs
and the Wayland desktop screenshot for actual legible application content.
`smoke-xorg.py` requires more than 16 decoded client colors as well as nonblack,
nonuniform pixels: black clients with white resize strips must fail.
The expected-negative control is the same probe built against the published
unmodified crate, not a fake screenshot or an application with rendering disabled.

During the isolated experiment these commands were run with
`/home/user/kadr/target/debug/kadr-x11-probe` and
`/home/user/kadr/target/debug/kadr-ui-probe` as the executables. The scratch
manifests set `[profile.dev] debug = 0`; builds used `CARGO_BUILD_JOBS=2`
and `CARGO_TARGET_DIR=/home/user/kadr/target`. The old unmodified probe was
copied aside before rebuilding. Its Xorg check failed waiting for rendered
pixels and recorded exactly one black color.

## Provenance and maintenance

- Published package: <https://crates.io/crates/gpui/0.2.2>
- Archive: <https://static.crates.io/crates/gpui/gpui-0.2.2.crate>
- Archive SHA-256:
  `979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707`
  (also the original workspace lockfile's registry checksum).
- Upstream repository: <https://github.com/zed-industries/zed>
- Published `.cargo_vcs_info.json` records commit
  `69e2130295c2649963eb639fc70b4f2ee8ea1624`, `path_in_vcs = "crates/gpui"`,
  and `dirty = true`. The **published archive**, not that Git revision, is the
  authoritative unpatched source.
- `vendor/gpui` retains every published file, including `LICENSE-APACHE`,
  original/normalized manifests, packaged lockfile, examples, resources, and
  VCS metadata. Cargo cache-only `.cargo-ok` is not part of the archive and is
  not included. No synthetic Cargo checksum file is needed for a path patch.
- License: Apache-2.0, copyright 2022–2025 Zed Industries, Inc.

To audit the local delta, verify the archive hash, extract it into a temporary
directory, and compare it with the vendor tree:

```sh
sha256sum gpui-0.2.2.crate
mkdir /tmp/gpui-published
tar -xzf gpui-0.2.2.crate --strip-components=1 -C /tmp/gpui-published
diff -ru /tmp/gpui-published vendor/gpui
```

Only the visual-selection block and the two grid float-type annotations above
should differ. Keep these changes isolated until a tested upstream version
includes both fixes. Remove the local
override and vendor tree only after repeating the native X11 pixel/lifecycle
checks and the Wayland check on the replacement. Do not silently assume an
upstream version bump or a successful window-map test fixes rendering.
