#!/usr/bin/env python3
"""Offline standard-library regression tests; no Cargo invocation or downloads."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import tarfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "package_licenses", Path(__file__).with_name("package-licenses.py")
)
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)


class LicenseCollectionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in ("Cargo.toml", "Cargo.lock", "LICENSE", "kadr-app"):
            (self.root / name).write_text(name + "\n")
        self.packages = []
        self.nodes = []
        self.package("kadr-app", workspace=True)
        self.package("dependency")
        self.package("dev-only")
        self.nodes[0]["deps"] = [
            {"pkg": "dependency", "dep_kinds": [{"kind": None}, {"kind": "build"}]},
            {"pkg": "dev-only", "dep_kinds": [{"kind": "dev"}]},
        ]
        self.metadata = {
            "workspace_members": ["kadr-app"],
            "packages": self.packages,
            "resolve": {"nodes": self.nodes},
        }

    def package(self, name, workspace=False):
        root = self.root / name if name != "kadr-app" else self.root / "app"
        root.mkdir()
        (root / "Cargo.toml").write_text(name + "\n")
        self.packages.append({
            "id": name, "name": name, "version": "0.1.0",
            "source": None if workspace else "registry+https://github.com/rust-lang/crates.io-index",
            "manifest_path": str(root / "Cargo.toml"),
            "license": None if workspace else "MIT",
            "license_file": None, "authors": ["Fixture Author"],
        })
        self.nodes.append({"id": name, "deps": [], "features": ["default"]})
        return root

    def run_collection(self, strict=False):
        with (
            patch.object(collector.subprocess, "check_output", return_value=json.dumps(self.metadata)) as cargo,
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            collector.collect(
                self.root, self.root / "kadr-app", self.root / "output",
                "x86_64-unknown-linux-gnu", strict,
            )
            command = cargo.call_args.args[0]
            self.assertIn("--offline", command)
            self.assertIn("--locked", command)
        return json.loads((self.root / "output/third-party/manifest.json").read_text())

    def test_nondev_closure_nested_notices_and_build_exclusion(self):
        dependency = self.root / "dependency"
        (dependency / "licenses").mkdir()
        (dependency / "licenses/MIT.txt").write_text("MIT copyright fixture\n")
        (dependency / "target").mkdir()
        (dependency / "target/NOTICE-generated").write_text("do not collect build output")
        result = self.run_collection(strict=True)
        self.assertEqual(result["status"], "texts-collected-not-audit")
        self.assertEqual(result["package_count"], 2)
        app = next(p for p in result["packages"] if p["name"] == "kadr-app")
        dep = next(p for p in result["packages"] if p["name"] == "dependency")
        self.assertEqual(app["project_license_fallback"], "LICENSE")
        self.assertEqual(app["dependencies"], [{"key": dep["key"], "kinds": ["build", "normal"]}])
        self.assertEqual(len(dep["notices"]), 1)
        self.assertTrue(dep["notices"][0]["path"].endswith("licenses/MIT.txt"))
        self.assertEqual(result["project_license"]["expression"], "GPL-3.0-only")
        self.assertEqual(result["input_binary"]["sha256"], collector.digest(self.root / "kadr-app"))

    def test_missing_text_is_visible_and_strict_rejects(self):
        with self.assertRaisesRegex(ValueError, "unresolved warnings"):
            self.run_collection(strict=True)
        result = json.loads((self.root / "output/third-party/manifest.json").read_text())
        self.assertEqual(result["status"], "needs-review")
        self.assertEqual(len(result["warnings"]), 1)
        self.assertIn("no local license", result["warnings"][0])

    def test_parent_license_file_stays_in_package_namespace(self):
        self.packages[1]["license_file"] = "../LICENSE"
        result = self.run_collection(strict=True)
        dependency = next(p for p in result["packages"] if p["name"] == "dependency")
        notice = dependency["notices"][0]
        path = Path(notice["path"])
        self.assertNotIn("..", path.parts)
        self.assertEqual(path.parts[-2:], ("declared-license", "LICENSE"))
        self.assertIn(dependency["key"], path.parts)
        self.assertEqual((self.root / "output" / path).read_bytes(), (self.root / "LICENSE").read_bytes())

    def test_copy_does_not_follow_symlinks_or_copy_binary_notices(self):
        source = self.root / "dependency/LICENSE"
        source.symlink_to(self.root / "Cargo.lock")
        with self.assertRaisesRegex(ValueError, "symlink"):
            collector.copy_notice(source, self.root / "copy", [self.root])
        source.unlink()
        source.write_bytes(b"not a text notice\0")
        with self.assertRaisesRegex(ValueError, "plain text"):
            collector.copy_notice(source, self.root / "copy", [self.root])

    def test_metadata_url_credentials_and_queries_are_not_preserved(self):
        self.assertEqual(
            collector.public_url("git+https://user:token@example.com/repo?secret=value#abcdef123"),
            "git+https://example.com/repo#abcdef123",
        )
        self.assertIsNone(collector.public_url("file:///private/checkout"))

    def override(self, corrupt=False):
        commit = "a" * 40
        raw = b"Copyright Fixture Author\nActual fixture license text without final newline"
        checksum = hashlib.sha256(raw).hexdigest()
        root = self.root / "overrides"
        (root / "_texts").mkdir(parents=True)
        (root / "_texts/notice.json").write_text(json.dumps({"text": raw.decode()}))
        (self.root / "dependency/.cargo_vcs_info.json").write_text(
            json.dumps({"git": {"sha1": commit}, "path_in_vcs": "crates/dependency"})
        )
        (root / "dependency-0.1.0").mkdir()
        (root / "dependency-0.1.0/source.json").write_text(json.dumps({
            "name": "dependency", "version": "0.1.0", "license_expression": "MIT",
            "vcs_sha1": commit, "path_in_vcs": "crates/dependency",
            "repository": "https://github.com/example/project",
            "evidence": "Synthetic test fixture, never an upstream copyright claim.",
            "files": [{
                "path": "_texts/notice.json", "encoding": "json-utf8",
                "upstream_path": "LICENSE",
                "url": f"https://raw.githubusercontent.com/example/project/{commit}/LICENSE",
                "sha256": "0" * 64 if corrupt else checksum,
            }],
        }))
        return root, raw

    def test_pinned_override_preserves_exact_bytes_and_provenance(self):
        root, raw = self.override()
        with patch.object(collector, "OVERRIDES_ROOT", root):
            result = self.run_collection(strict=True)
        dependency = next(p for p in result["packages"] if p["name"] == "dependency")
        notice = dependency["notices"][0]
        self.assertEqual((self.root / "output" / notice["path"]).read_bytes(), raw)
        self.assertTrue((self.root / "output" / notice["provenance"]).is_file())
        self.assertEqual(result["warnings"], [])

    def test_corrupt_override_fails_instead_of_claiming_text_was_collected(self):
        root, _ = self.override(corrupt=True)
        with patch.object(collector, "OVERRIDES_ROOT", root):
            with self.assertRaisesRegex(ValueError, "upstream notice hash mismatch"):
                self.run_collection()

    def standard_fixture(self, *, conflicting=False, header=True):
        name, version = "convert_case", "0.4.0"
        registry = self.root / "registry"
        source = registry / "src/test-index" / f"{name}-{version}"
        source.mkdir(parents=True)
        manifest = (
            '[package]\nname="convert_case"\nversion="0.4.0"\nlicense="MIT"\n'
            'authors=["Fixture Author"]\n'
        ).encode()
        (source / "Cargo.toml").write_bytes(manifest)
        members = {
            "Cargo.toml": manifest,
            "Cargo.toml.orig": manifest.replace(b'license="MIT"', b'license="NOASSERTION"') if conflicting else manifest,
            "README.md": b"Synthetic published-package test fixture.\n",
            "src/lib.rs": ((b"// Copyright Fixture Author\n// Licensed under MIT.\n" if header else b"")
                           + b"pub fn fixture() {}\n"),
        }
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            for path, contents in members.items():
                info = tarfile.TarInfo(f"{name}-{version}/{path}")
                info.size = len(contents)
                archive.addfile(info, io.BytesIO(contents))
        raw = data.getvalue()
        archive = registry / "cache/test-index" / f"{name}-{version}.crate"
        archive.parent.mkdir(parents=True)
        archive.write_bytes(raw)
        checksum = hashlib.sha256(raw).hexdigest()
        self.packages[1].update(name=name, version=version, manifest_path=str(source / "Cargo.toml"))
        self.root.joinpath("Cargo.lock").write_text(
            'version=4\n[[package]]\nname="convert_case"\nversion="0.4.0"\n'
            f'source="{self.packages[1]["source"]}"\nchecksum="{checksum}"\n'
        )
        overrides = self.root / "standards"
        (overrides / "convert_case-0.4.0").mkdir(parents=True)
        (overrides / "convert_case-0.4.0/source.json").write_text(json.dumps({
            "record_type": "declared-spdx", "name": name, "version": version,
            "license_expression": "MIT", "crate_sha256": checksum,
            "canonical_license_ids": ["MIT"],
            "archive_url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
        }))
        (overrides / "spdx").mkdir()
        terms = b"MIT License\nCopyright (c) <year> <copyright holders>\nPermission fixture terms.\n"
        (overrides / "spdx/MIT.txt").write_bytes(terms)
        commit = "a" * 40
        (overrides / "spdx/source.json").write_text(json.dumps({
            "repository": "https://github.com/spdx/license-list-data", "commit": commit,
            "files": [{
                "license_id": "MIT", "path": "spdx/MIT.txt",
                "url": f"https://raw.githubusercontent.com/spdx/license-list-data/{commit}/text/MIT.txt",
                "sha256": hashlib.sha256(terms).hexdigest(),
                "template_copyright_line": "Copyright (c) <year> <copyright holders>",
            }],
        }))
        return overrides, archive

    def test_declared_standard_grant_authenticates_archive_and_retains_notices(self):
        overrides, _ = self.standard_fixture()
        with patch.object(collector, "OVERRIDES_ROOT", overrides):
            result = self.run_collection(strict=True)
        package = next(p for p in result["packages"] if p["name"] == "convert_case")
        self.assertEqual(package["license_text_status"], "declared-standard-text")
        self.assertEqual(result["declared_standard_package_count"], 1)
        self.assertEqual(result["warnings"], [])
        terms = next(n for n in package["notices"] if n.get("kind") == "declared-standard-text")
        text = (self.root / "output" / terms["path"]).read_text()
        self.assertNotIn("<copyright holders>", text)
        header = next(n for n in package["notices"] if n.get("kind") == "source-header-notice")
        self.assertEqual(
            (self.root / "output" / header["path"]).read_bytes(),
            b"// Copyright Fixture Author\n// Licensed under MIT.",
        )

    def test_declared_standard_rejects_corrupt_archive(self):
        overrides, archive = self.standard_fixture()
        archive.write_bytes(b"not the authenticated archive")
        with patch.object(collector, "OVERRIDES_ROOT", overrides):
            with self.assertRaisesRegex(ValueError, "published crate checksum mismatch"):
                self.run_collection(strict=True)

    def test_declared_standard_rejects_conflicting_published_declaration(self):
        overrides, _ = self.standard_fixture(conflicting=True)
        with patch.object(collector, "OVERRIDES_ROOT", overrides):
            with self.assertRaisesRegex(ValueError, "published license declaration conflict"):
                self.run_collection(strict=True)

    def test_declared_standard_rejects_noassertion(self):
        overrides, _ = self.standard_fixture()
        self.packages[1]["license"] = "NOASSERTION"
        with patch.object(collector, "OVERRIDES_ROOT", overrides):
            with self.assertRaisesRegex(ValueError, "unrecognized/conflicting declared-SPDX"):
                self.run_collection(strict=True)

    def test_declared_standard_is_not_an_automatic_unknown_package_fallback(self):
        with self.assertRaisesRegex(ValueError, "unrecognized/conflicting declared-SPDX"):
            collector.declared_standard_notices(
                {"name": "unreviewed", "version": "1.0.0"},
                self.root, self.root / "unused", self.root, self.root, {},
            )

    def test_declared_standard_rejects_mismatched_lock_checksum(self):
        overrides, _ = self.standard_fixture()
        lock = self.root / "Cargo.lock"
        lock.write_text(lock.read_text().replace('checksum="', 'checksum="bad'))
        with patch.object(collector, "OVERRIDES_ROOT", overrides):
            with self.assertRaisesRegex(ValueError, "declared-SPDX lock checksum mismatch"):
                self.run_collection(strict=True)

    def test_declared_standard_does_not_invent_missing_copyright(self):
        overrides, _ = self.standard_fixture(header=False)
        with patch.object(collector, "OVERRIDES_ROOT", overrides):
            result = self.run_collection(strict=True)
        package = next(p for p in result["packages"] if p["name"] == "convert_case")
        first = self.root / "output" / package["notices"][0]["path"]
        provenance = json.loads((first.parent.parent / "provenance.json").read_text())
        self.assertEqual(provenance["actual_notice_count"], 0)
        self.assertIn("none invented", provenance["copyright_note"])


if __name__ == "__main__":
    unittest.main()
