#!/usr/bin/env python3
"""Regression tests for the workspace dependency-direction gate."""

import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location(
    "architecture", Path(__file__).with_name("check-architecture.py")
)
architecture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(architecture)


def package(name, *dependencies):
    return {
        "name": name,
        "dependencies": [
            {"name": dependency, "kind": None} for dependency in dependencies
        ],
    }


class ArchitectureTests(unittest.TestCase):
    def metadata(self):
        return {
            "packages": [
                package("kadr-app", "kadr-engine", "kadr-ui", "gpui"),
                package("kadr-ui", "gpui"),
                package("kadr-engine", "kadr-storage"),
                package("kadr-storage", "kadr-core"),
                package("kadr-core"),
            ]
        }

    def test_separate_layers_are_allowed(self):
        self.assertEqual(architecture.check(self.metadata()), [])

    def test_desktop_cannot_bypass_engine(self):
        metadata = self.metadata()
        metadata["packages"][0]["dependencies"].append(
            {"name": "kadr-storage", "kind": None}
        )
        self.assertTrue(architecture.check(metadata))

    def test_indirect_engine_dependency_cannot_import_gpui(self):
        metadata = self.metadata()
        metadata["packages"][-1]["dependencies"].append(
            {"name": "gpui", "kind": None}
        )
        self.assertTrue(architecture.check(metadata))

    def test_ui_cannot_import_engine(self):
        metadata = self.metadata()
        metadata["packages"][1]["dependencies"].append(
            {"name": "kadr-engine", "kind": None}
        )
        self.assertTrue(architecture.check(metadata))

    def test_dev_dependencies_do_not_change_runtime_layers(self):
        metadata = self.metadata()
        metadata["packages"][0]["dependencies"].append(
            {"name": "kadr-storage", "kind": "dev"}
        )
        self.assertEqual(architecture.check(metadata), [])

    def test_engine_tests_cannot_import_presentation(self):
        for dependency in ("gpui", "kadr-ui", "kadr-app"):
            with self.subTest(dependency=dependency):
                metadata = self.metadata()
                metadata["packages"][2]["dependencies"].append(
                    {"name": dependency, "kind": "dev"}
                )
                self.assertTrue(architecture.check(metadata))

    def test_engine_test_helpers_cannot_import_presentation(self):
        metadata = self.metadata()
        metadata["packages"].append(package("engine-test-helper", "gpui"))
        metadata["packages"][2]["dependencies"].append(
            {"name": "engine-test-helper", "kind": "dev"}
        )
        self.assertTrue(architecture.check(metadata))

    def test_lower_level_library_dev_dependencies_are_not_built_with_engine(self):
        metadata = self.metadata()
        metadata["packages"][-1]["dependencies"].append(
            {"name": "gpui", "kind": "dev"}
        )
        self.assertEqual(architecture.check(metadata), [])


if __name__ == "__main__":
    unittest.main()
