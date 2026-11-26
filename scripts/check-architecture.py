#!/usr/bin/env python3
"""Check the engine/presentation dependency boundary using Cargo metadata."""

import json
from pathlib import Path
import subprocess
import sys


def check(metadata):
    packages = {package["name"]: package for package in metadata["packages"]}
    workspace = set(packages)

    def dependencies(name, include_dev=False):
        return {
            dependency["name"]
            for dependency in packages[name]["dependencies"]
            if include_dev or dependency["kind"] != "dev"
        }

    errors = []
    for name, allowed in (
        ("kadr-app", {"kadr-engine", "kadr-ui"}),
        ("kadr-ui", set()),
    ):
        unexpected = (dependencies(name) & workspace) - allowed
        if unexpected:
            errors.append(f"{name} has forbidden workspace dependencies: {sorted(unexpected)}")

    visited = set()
    pending = ["kadr-engine"]
    while pending:
        name = pending.pop()
        if name in visited:
            continue
        visited.add(name)
        # `cargo test -p kadr-engine` also builds the engine's own dev
        # dependencies, but not the dev dependencies of its libraries.
        for dependency in dependencies(name, include_dev=name == "kadr-engine"):
            if dependency in {"gpui", "kadr-ui", "kadr-app"}:
                errors.append(f"engine dependency {name} depends on presentation: {dependency}")
            elif dependency in workspace:
                pending.append(dependency)
    return errors


def main():
    root = Path(__file__).resolve().parent.parent
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    errors = check(json.loads(result.stdout))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("PASS: engine is independent of presentation; desktop uses engine and UI only")
    return 0


if __name__ == "__main__":
    sys.exit(main())
