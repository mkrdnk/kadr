#!/usr/bin/env python3
"""Collect local notices, not a license-compatibility audit or linked-code SBOM."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
from urllib.parse import urlsplit, urlunsplit


EXCLUDED = {
    ".git", ".hg", ".svn", "target", "node_modules", "build", "dist",
    ".venv", "venv", "__pycache__",
}
NOTICE_NAME = re.compile(r"^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT|AUTHORS|UNLICENSE|OFL)", re.I)
MAX_NOTICE_BYTES = 2 * 1024 * 1024
OVERRIDES_ROOT = Path(__file__).resolve().parent.parent / "packaging/licenses"


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def public_url(value):
    """Metadata can contain credentialed URLs; never preserve userinfo/query."""
    if not value:
        return None
    prefix = ""
    for candidate in ("registry+", "sparse+", "git+"):
        if value.startswith(candidate):
            prefix, value = candidate, value[len(candidate):]
            break
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        return None
    fragment = parsed.fragment if re.fullmatch(r"[0-9a-fA-F]{7,64}", parsed.fragment) else ""
    return prefix + urlunsplit((parsed.scheme, parsed.hostname, parsed.path, "", fragment))


def copy_notice(source, destination, allowed_roots):
    if source.is_symlink():
        raise ValueError("symlink notice is not copied")
    resolved = source.resolve()
    if not any(resolved.is_relative_to(root) for root in allowed_roots):
        raise ValueError("notice resolves outside its crate/project")
    if not resolved.is_file() or resolved.stat().st_size > MAX_NOTICE_BYTES:
        raise ValueError("notice is missing, non-file, or larger than 2 MiB")
    if b"\0" in resolved.read_bytes():
        raise ValueError("notice is not a plain text file")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(resolved, destination)
    return digest(destination)


def notice_files(root):
    for directory, names, files in os.walk(root, followlinks=False):
        names[:] = sorted(name for name in names if name not in EXCLUDED)
        notice_directory = any(NOTICE_NAME.match(part) for part in Path(directory).relative_to(root).parts)
        for name in sorted(files):
            if notice_directory or NOTICE_NAME.match(name):
                yield Path(directory) / name


def closure(metadata):
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    roots = [p for p in metadata["packages"] if p["name"] == "kadr-app"
             and p["id"] in metadata["workspace_members"]]
    if len(roots) != 1:
        raise ValueError("expected one workspace kadr-app package")
    selected = set()
    pending = [roots[0]["id"]]
    while pending:
        identity = pending.pop()
        if identity in selected:
            continue
        selected.add(identity)
        pending.extend(
            dep["pkg"] for dep in nodes[identity]["deps"]
            if any(kind["kind"] != "dev" for kind in dep["dep_kinds"])
        )
    return packages, nodes, selected


def package_key(package, identity):
    source_key = package["source"] or identity
    suffix = hashlib.sha256(source_key.encode()).hexdigest()[:10]
    return re.sub(r"[^A-Za-z0-9._+-]", "_", f"{package['name']}-{package['version']}-{suffix}")


def upstream_notices(package, root, destination, output):
    record = OVERRIDES_ROOT / f"{package['name']}-{package['version']}" / "source.json"
    if not record.is_file():
        return []
    override = json.loads(record.read_text(encoding="utf-8"))
    vcs = json.loads((root / ".cargo_vcs_info.json").read_text(encoding="utf-8"))
    commit = vcs["git"]["sha1"]
    if (
        override["name"] != package["name"]
        or override["version"] != package["version"]
        or override["license_expression"] != package.get("license")
        or not re.fullmatch(r"[0-9a-f]{40}", commit)
        or override["vcs_sha1"] != commit
        or override["path_in_vcs"] != vcs.get("path_in_vcs", "")
        or not override["files"]
    ):
        raise ValueError(f"upstream notice provenance mismatch: {package['name']}")
    provenance = destination / "upstream/source.json"
    provenance.parent.mkdir(parents=True)
    shutil.copyfile(record, provenance)
    notices = []
    for entry in override["files"]:
        relative = Path(entry["path"])
        upstream_path = Path(entry["upstream_path"])
        url = urlsplit(entry["url"])
        if (
            relative.is_absolute() or ".." in relative.parts
            or upstream_path.is_absolute() or ".." in upstream_path.parts
            or not upstream_path.name
            or url.scheme != "https" or url.username or url.password
            or url.query or url.fragment or commit not in url.path.split("/")
            or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"])
        ):
            raise ValueError(f"unsafe/unpinned upstream notice record: {package['name']}")
        source = OVERRIDES_ROOT / relative
        if not source.resolve().is_relative_to(OVERRIDES_ROOT.resolve()) or source.is_symlink():
            raise ValueError(f"unsafe upstream notice path: {package['name']}")
        encoding = entry.get("encoding", "verbatim")
        if encoding == "json-utf8":
            # A few public notices intentionally lack a terminal newline.
            # JSON preserves those raw bytes through line-oriented source edits.
            data = json.loads(source.read_text(encoding="utf-8"))["text"].encode("utf-8")
        elif encoding == "verbatim":
            data = source.read_bytes()
        else:
            raise ValueError(f"unknown upstream notice encoding: {package['name']}")
        if (
            len(data) > MAX_NOTICE_BYTES or b"\0" in data
            or hashlib.sha256(data).hexdigest() != entry["sha256"]
        ):
            raise ValueError(f"upstream notice hash mismatch: {package['name']}")
        copied = destination / "upstream" / upstream_path
        copied.parent.mkdir(parents=True, exist_ok=True)
        copied.write_bytes(data)
        notices.append({
            "path": copied.relative_to(output).as_posix(),
            "sha256": digest(copied),
            "upstream_url": entry["url"],
            "vcs_sha1": commit,
            "provenance": provenance.relative_to(output).as_posix(),
        })
    return notices


def collect(project, binary, output, target, strict):
    # Do not fetch, update the lockfile, or run build scripts.
    command = [
        "cargo", "metadata", "--offline", "--locked", "--format-version", "1",
        "--manifest-path", str(project / "Cargo.toml"), "--filter-platform", target,
    ]
    metadata = json.loads(subprocess.check_output(command, cwd=project, text=True))
    packages, nodes, selected = closure(metadata)
    output.mkdir(parents=True, exist_ok=False)
    third_party = output / "third-party"
    third_party.mkdir()
    copy_notice(project / "LICENSE", output / "LICENSE", [project])
    warnings = []
    records = []
    workspace = set(metadata["workspace_members"])
    keys = {identity: package_key(packages[identity], identity) for identity in selected}
    for identity in sorted(selected, key=lambda item: (packages[item]["name"], packages[item]["version"], item)):
        package = packages[identity]
        root = Path(package["manifest_path"]).resolve().parent
        key = keys[identity]
        destination = third_party / "rust" / key
        files = set(notice_files(root))
        if package.get("license_file"):
            files.add(root / package["license_file"])
        texts = []
        for path in sorted(files):
            # Keep nested notices distinct, without copying entire source trees.
            resolved = path.resolve()
            relative = (resolved.relative_to(root) if resolved.is_relative_to(root)
                        else Path("declared-license") / path.name)
            try:
                copied = destination / relative
                if not copied.resolve().is_relative_to(destination.resolve()):
                    raise ValueError("notice destination escapes package namespace")
                copied_hash = copy_notice(path, copied, [root, project])
                texts.append({
                    "path": (destination / relative).relative_to(output).as_posix(),
                    "sha256": copied_hash,
                })
            except (OSError, ValueError) as error:
                warnings.append(f"{key}: {relative.as_posix()}: {error}")
        if not texts:
            texts = upstream_notices(package, root, destination, output)
        project_license = identity in workspace and not texts
        if not texts and not project_license:
            warnings.append(f"{key}: no local license/copyright/notice text found")
        if not package.get("license") and not package.get("license_file") and identity not in workspace:
            warnings.append(f"{key}: Cargo metadata has no license expression or license_file")
        records.append({
            "key": key,
            "name": package["name"],
            "version": package["version"],
            "license_expression": package.get("license"),
            "declared_license_file": package.get("license_file"),
            "authors": package.get("authors", []),
            "source": public_url(package.get("source")),
            "repository": public_url(package.get("repository")),
            "homepage": public_url(package.get("homepage")),
            "crates_io": (f"https://crates.io/crates/{package['name']}/{package['version']}"
                          if (package.get("source") or "").startswith(("registry+", "sparse+")) else None),
            "workspace_package": identity in workspace,
            "project_license_fallback": "LICENSE" if project_license else None,
            "license_text_status": "project-root-license" if project_license else ("present" if texts else "missing"),
            "manifest_sha256": digest(Path(package["manifest_path"])),
            "resolved_features": sorted(nodes[identity]["features"]),
            "dependencies": [
                {
                    "key": keys[dep["pkg"]],
                    "kinds": sorted({kind["kind"] or "normal" for kind in dep["dep_kinds"]
                                     if kind["kind"] != "dev"}),
                }
                for dep in sorted(nodes[identity]["deps"], key=lambda dep: dep["pkg"])
                if dep["pkg"] in selected and any(kind["kind"] != "dev" for kind in dep["dep_kinds"])
            ],
            "notices": texts,
        })
    vendor = project / "vendor/gpui"
    vendor_notices = []
    if any(Path(packages[item]["manifest_path"]).resolve().parent == vendor for item in selected):
        # Mandatory for the local GPUI delta: preserve upstream terms and the
        # project's detailed provenance/modification explanation, not just SPDX.
        for source, name in (
            (vendor / "LICENSE-APACHE", "LICENSE-APACHE"),
            (project / "docs/adr-gpui-x11.md", "adr-gpui-x11.md"),
        ):
            destination = third_party / "vendor/gpui" / name
            vendor_notices.append({
                "path": destination.relative_to(output).as_posix(),
                "sha256": copy_notice(source, destination, [project]),
            })
        (third_party / "vendor/gpui/MODIFICATIONS.txt").write_text(
            "kadr distributes a modified copy of GPUI 0.2.2 (Apache-2.0).\n"
            "Copyright 2022-2025 Zed Industries, Inc.; upstream terms are retained.\n"
            "kadr contributors changed src/platform/linux/x11/window.rs to choose\n"
            "the opaque X11 visual, adjust its warning and explain the patch.\n"
            "See adr-gpui-x11.md for exact scope, upstream archive checksum,\n"
            "provenance, evidence and maintenance instructions.\n",
            encoding="utf-8",
        )
    inventory = {
        "schema": 1,
        "purpose": "Conservative app dependency notice inventory; NOT a complete SBOM or legal audit.",
        "scope": "kadr-app non-dev normal/build dependency closure from Cargo metadata for this target.",
        "limitations": [
            "Cargo metadata feature unification/build dependencies can overinclude code not linked into the executable.",
            "Native/vendored C, generated code, embedded assets and tool runtimes need separate review.",
            "The supplied binary hash associates inputs, but does not attest it was built from this lockfile/checkout.",
            "Local notices and commit/hash-verified upstream overrides are copied verbatim; missing text is never invented from SPDX.",
            "License compatibility, source-offer and relinking obligations remain a pre-publication review.",
        ],
        "target": target,
        "project_license": {
            "path": "LICENSE",
            "expression": "GPL-3.0-only",
            "basis": "kadr distribution policy; workspace manifests may omit a license field",
        },
        "input_binary": {"name": binary.name, "sha256": digest(binary)},
        "cargo_lock_sha256": digest(project / "Cargo.lock"),
        "workspace_manifest_sha256": digest(project / "Cargo.toml"),
        "package_count": len(records),
        "packages": records,
        "vendor_notices": vendor_notices,
        "warnings": warnings,
        "status": "needs-review" if warnings else "texts-collected-not-audit",
    }
    (third_party / "manifest.json").write_text(
        json.dumps(inventory, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    missing = sum(package["license_text_status"] == "missing" for package in records)
    (third_party / "REVIEW.txt").write_text(
        f"NOTICE_STATUS: {inventory['status']}\n"
        + (f"UNSIGNED LOCAL QA / AUDIT ONLY — NOT FOR DISTRIBUTION: {missing} package(s) missing text; "
           f"{len(warnings)} warning(s).\n" if warnings else "")
        + inventory["purpose"] + "\n\n" + "\n".join(inventory["limitations"])
        + "\n\nCollection warnings:\n" + ("\n".join(warnings) if warnings else "None detected.")
        + "\n", encoding="utf-8",
    )
    print(f"Collected notices for {len(records)} Rust packages; {len(warnings)} warning(s).")
    for warning in warnings:
        print(f"NOTICE REVIEW: {warning}", file=sys.stderr)
    if strict and warnings:
        raise ValueError("notice collection has unresolved warnings (--strict)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project-root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="NEW output directory")
    parser.add_argument("--target", help="native rustc host target by default")
    parser.add_argument("--strict", action="store_true", help="fail if local notice text/metadata gaps are found")
    args = parser.parse_args()
    target = args.target
    if target is None:
        version = subprocess.check_output(["rustc", "-vV"], text=True)
        target = next(line.removeprefix("host: ") for line in version.splitlines() if line.startswith("host: "))
    try:
        collect(args.project_root.resolve(), args.binary.resolve(), args.output.resolve(), target, args.strict)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Notice collection failed: {error}\n")


if __name__ == "__main__":
    main()
