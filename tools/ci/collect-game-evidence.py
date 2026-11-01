#!/usr/bin/env python3
"""Collect bounded logs only from the diagnostic's newly allocated workspace.

The game protocol never uses a user's account or game directory. This collector
still checks the selected private parent and owner marker before reading any
path from its report. It does not upload runtimes, assets, worlds or screenshots.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import stat


def collect(report: Path, parent: Path, output: Path) -> dict:
    parent = parent.resolve(strict=True)
    result = json.loads(report.read_bytes())
    workspace_name = result.get("client_attempt", {}).get("client", {}).get("workspace")
    if not isinstance(workspace_name, str):
        return {"collected": False, "reason": "No allocated game workspace was reported"}
    path = Path(workspace_name)
    if path.is_symlink() or (hasattr(path, "is_junction") and path.is_junction()):
        raise ValueError("Game workspace became a symbolic link")
    workspace = path.resolve(strict=True)
    # Rust's canonical Windows path keeps a verbatim prefix; Python may retain
    # or remove it depending on the caller's spelling. Compare the immediate
    # parent's real filesystem identity, not those two textual namespaces.
    if not workspace.parent.samefile(parent):
        raise ValueError("Game workspace is not a direct child of the private test parent")
    marker = workspace / "game-acceptance-owner.json"
    metadata = marker.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 1024:
        raise ValueError("Missing bounded game-workspace owner record")
    if json.loads(marker.read_bytes()) != {"schema_version": 1, "purpose": "disposable-game-acceptance"}:
        raise ValueError("Unexpected game-workspace owner record")
    output.mkdir(parents=True, exist_ok=True)
    files = []
    for source in workspace.rglob("*"):
        if source.suffix != ".log" and source.name != "acceptance-quickplay.json":
            continue
        metadata = source.lstat()
        if not stat.S_ISREG(metadata.st_mode) or source.is_symlink():
            continue
        if not source.resolve(strict=True).is_relative_to(workspace):
            raise ValueError("Game evidence escaped its private workspace")
        if len(files) >= 32:
            raise ValueError("Unexpected number of game log files")
        relative = source.relative_to(workspace)
        # Monitor logs live under .pcl-linux. Flatten the bounded selected files
        # so artifact upload's hidden-directory filter cannot silently omit the
        # native stderr needed to diagnose failures before a window is created.
        target = output / f"{len(files):02d}-{source.name.lstrip('.')}"
        with source.open("rb") as incoming, target.open("xb") as outgoing:
            incoming.seek(max(0, metadata.st_size - 2 * 1024**2))
            remaining = 2 * 1024**2
            while remaining:
                chunk = incoming.read(min(128 * 1024, remaining))
                if not chunk:
                    break
                outgoing.write(chunk)
                remaining -= len(chunk)
        files.append({"path": relative.as_posix(), "collected_path": target.name, "source_bytes": metadata.st_size,
                      "collected_bytes": target.stat().st_size})
    evidence = {"collected": True, "files": files, "workspace_retained_until_runner_teardown": True}
    (output / "collection.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--parent", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(collect(args.report, args.parent, args.output)))
