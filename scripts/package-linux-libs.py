#!/usr/bin/env python3
"""Collect ELF dependencies from this trusted local build, not arbitrary downloads."""
import ctypes.util
import hashlib
import pathlib
import re
import shutil
import subprocess
import sys

appdir = pathlib.Path(sys.argv[1])
libdir = appdir / "usr/lib"
# These are part of the host's libc/graphics driver ABI, never redistribute them.
system = re.compile(r"^(ld-linux.*|lib(c|m|pthread|dl|rt|resolv|util)\.so\..*|libvulkan\.so\..*)$")
pending = [appdir / "usr/bin/kadr"]
# GPUI loads fontconfig, X11/Wayland and XKB libraries at runtime.
for name in ("fontconfig", "X11", "X11-xcb", "xcb", "xkbcommon", "xkbcommon-x11",
             "wayland-client", "wayland-cursor", "wayland-egl"):
    soname = ctypes.util.find_library(name)
    if not soname:
        sys.exit(f"Missing GPUI runtime library: {name}")
    output = subprocess.check_output(["/sbin/ldconfig", "-p"], text=True)
    match = re.search(rf"^\s*{re.escape(soname)} .* => (.+)$", output, re.M)
    if not match:
        sys.exit(f"Cannot resolve {soname}")
    pending.append(pathlib.Path(match[1]))
seen = set()
notices = appdir / "usr/share/licenses/kadr/third-party/native"
notices.mkdir(parents=True)
inventory = []
common_license_hashes = {}
while pending:
    path = pending.pop()
    if path.name in seen or system.match(path.name):
        continue
    seen.add(path.name)
    output = subprocess.check_output(["ldd", str(path)], text=True)
    if "not found" in output:
        sys.exit(f"Unresolved dependency for {path}:\n{output}")
    pending.extend(pathlib.Path(p) for p in re.findall(r"=> (/\S+)", output))
    if path != appdir / "usr/bin/kadr":
        shutil.copy2(path, libdir / path.name, follow_symlinks=True)
        # This packager targets Debian/Ubuntu build hosts. Fail rather than
        # silently dropping redistributable dependency copyright notices.
        candidates = {str(path), str(path.resolve())}
        candidates.update(p.replace("/usr/lib/", "/lib/", 1) for p in list(candidates))
        candidates.update(p.replace("/lib/", "/usr/lib/", 1) for p in list(candidates)
                          if p.startswith("/lib/"))
        owner = None
        for candidate in sorted(candidates):
            result = subprocess.run(["dpkg-query", "-S", candidate], capture_output=True, text=True)
            if result.returncode == 0:
                owner = result.stdout.split(": ", 1)[0].split(", ")[0]
                break
        if not owner:
            sys.exit(f"Cannot identify dependency license package for {path}")
        package = owner.split(":")[0]
        copyright_file = pathlib.Path("/usr/share/doc") / package / "copyright"
        if not copyright_file.is_file():
            sys.exit(f"Missing copyright file: {copyright_file}")
        shutil.copy2(copyright_file, notices / f"{package}.copyright")
        # Debian copyright notices often refer to standard texts instead of
        # embedding them. Preserve those texts in the portable image as well.
        for reference in sorted(set(re.findall(
            r"/usr/share/common-licenses/([A-Za-z0-9][A-Za-z0-9.+_-]*)",
            copyright_file.read_text(encoding="utf-8", errors="replace"),
        ))):
            name = reference.rstrip(".")
            source = pathlib.Path("/usr/share/common-licenses") / name
            if not source.is_file():
                sys.exit(f"Missing referenced common license: {name} ({package})")
            destination = notices / "common-licenses" / name
            destination.parent.mkdir(exist_ok=True)
            shutil.copyfile(source, destination)
            common_license_hashes[name] = hashlib.sha256(destination.read_bytes()).hexdigest()
        version = subprocess.check_output(
            ["dpkg-query", "-W", "-f=${Version}", owner], text=True)
        inventory.append(f"{path.name}\t{owner}\t{version}")
(notices / "BUNDLED-LIBRARIES.txt").write_text("\n".join(sorted(inventory)) + "\n")
(notices / "COMMON-LICENSES.sha256").write_text(
    "".join(f"{value}  common-licenses/{name}\n" for name, value in sorted(common_license_hashes.items()))
)
