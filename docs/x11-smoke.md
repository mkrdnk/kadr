# Graphical native X11 acceptance smoke

`scripts/smoke-xorg.py` runs an **existing real `kadr-app` executable** on a
private Xorg dummy-driver display with Openbox. It does not build the app, use
XWayland, or replace rendering with synthetic colors.

## Dependencies and invocation

On Debian/Ubuntu, the harness tools and software Vulkan renderer are:

```sh
sudo apt-get install xserver-xorg-core xserver-xorg-video-dummy openbox \
  xdotool wmctrl x11-utils imagemagick mesa-vulkan-drivers fonts-dejavu-core
```

The executable also needs its normal runtime libraries (GPUI/Blade, font,
X11, Vulkan and application dependencies); these packages are not a complete
build dependency list. Python 3 uses only its standard library.

From the repository root, after building the desired binary:

```sh
python3 scripts/smoke-xorg.py --binary target/debug/kadr-app \
  --artifacts target/smoke-xorg
```

The artifacts directory **must be new**; use a different name on subsequent
runs. Without `--artifacts`, a retained `/tmp/kadr-xorg-artifacts-*` directory is
printed. Both modes retain logs, the Xorg configuration, client PNGs and decoded
pixel statistics even on failure. `--timeout 30` sets each wait's deadline, not
a total runtime budget; external commands have a five-second timeout.

Xorg 21.1.7 with the dummy driver and Mesa 22.3.6/lavapipe has run this setup
unprivileged in the Debian cloud environment. No `sudo` is used by the script.
Provision the packages as root in CI, then invoke the harness as an ordinary
user. Some distributions install an `Xorg.wrap` that permits only console
users, or prohibit arbitrary config paths for privileged Xorg. In that case
inspect `xorg-stderr.log` and configure a disposable runner for rootless Xorg
with the actual server on `PATH` (Debian's server is `/usr/lib/xorg/Xorg`).
Do not loosen the policy of a shared workstation or run the application as
root just to pass this test.

For reproducible CPU-only CI, optionally select the installed lavapipe ICD:

```sh
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
  python3 scripts/smoke-xorg.py --binary target/debug/kadr-app \
  --artifacts target/smoke-xorg-lavapipe
```

That ICD path is architecture/distribution dependent; the harness otherwise
preserves the caller's renderer selection.

## What passes

For **each of two launches**, the harness requires a visible native X11 client
named `kadr`, owned by the launched PID, resizes its client area to 1000x720,
and captures **only that client window** using ImageMagick `import -window`.
Openbox decorations, the root background and another application's window
cannot supply the pixel evidence.

ImageMagick `identify` decodes the PNG and checks its dimensions plus normalized
mean > 0.001, standard deviation > 0.005, maximum > 0.05 and more than 16
distinct colors. This rejects black and uniform captures, not just absent/empty
files, and a black window with solid newly exposed resize strips. Rendering is retried within
the deadline to allow initial presentation. No exact palette, layout or text
is baked into the check. **Inspect both `client-*.png` files manually**: a
nonuniform corrupt image could still pass these deliberately broad statistics.
This is not an OCR or visual-regression test.

The first launch must advertise `WM_DELETE_WINDOW` and exit with code 0 after
Openbox receives a `wmctrl -c` request. The second launch must exit with code 0
after window activation and Ctrl+Q. A mapped window or successful lifecycle
alone cannot pass this graphical harness.

Tab, Shift+Tab, Enter and Space behavior is not asserted here. That needs
UI-specific state/focus assertions and must not be inferred from injected keys
or a screenshot with visible controls.

## Why Xorg, not Xvfb?

The same GPUI/Blade binary and Mesa 22.3.6/lavapipe produced a black client
capture on Xvfb but legible UI on Xorg 21.1.7 with the dummy driver. A successful
Xvfb map/resize/close test therefore does not establish working presentation.
The separate existing `smoke-x11.py` lifecycle check is not changed by this
harness.

## Isolation and cleanup

The script creates a short-lived, private HOME and XDG config, data, cache,
state and runtime directories. It clears inherited Wayland/headless/session
bus settings and Xauthority. Xorg allocates a free display atomically through
`-displayfd` with an inherited pipe; there is no fixed display number or
check/start race. Its dummy configuration disables automatic input-device
discovery, and `-nolisten tcp` prevents a network listener.

The private display is not a security boundary against other local users:
use a disposable CI runner for untrusted applications. Each started service,
application and helper has its own process group. On success, failure,
Ctrl+C or SIGTERM, owned groups are terminated and reaped before private state
is removed. The harness never scans for or kills other displays. As with any
process, SIGKILL or machine loss cannot execute cleanup.
