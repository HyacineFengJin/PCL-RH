#!/usr/bin/env python3
"""Mount only this CI job's DMG; verify and execute its private writable copy."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile

import importlib.util

SPEC = importlib.util.spec_from_file_location("pcl_dmg", Path(__file__).resolve().parents[1] / "dmg-native.py")
assert SPEC is not None and SPEC.loader is not None
DMG = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DMG)


def verify(directory: Path, portable: Path) -> dict:
    require = DMG.PACKAGE.require
    require(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_OS") == "macOS",
            "Disk-image mounting checks are restricted to disposable macOS CI")
    report = json.loads((directory / "dmg-manifest.json").read_bytes())
    name = report["image"]
    require(isinstance(name, str) and Path(name).name == name and name.endswith(".dmg"), "Invalid image name")
    image = directory / name
    require(DMG.sha256(image) == report["sha256"]
            and image.with_name(name + ".sha256").read_text().split() == [report["sha256"], name],
            "Disk-image checksum differs from its manifest")
    require(report["schema_version"] == 1 and report["distribution"] == "macos-portable-dmg"
            and report["signed"] is False and report["notarized"] is False
            and report["requires_writable_copy"] is True and report["user_data_included"] is False,
            "Unexpected disk-image distribution policy")
    archives = list(portable.glob("*.tar.gz"))
    require(len(archives) == 1 and DMG.sha256(archives[0]) == report["source_archive_sha256"],
            "Disk image was built from a different portable archive")
    with tempfile.TemporaryDirectory(prefix="dmg space & 项目-", dir=directory) as temporary:
        scratch = Path(temporary).resolve()
        mount = scratch / "mounted"
        mount.mkdir()
        try:
            # Explicit mountpoint and no Finder opening: never attach at or
            # detach a user-selected volume, and never run from a read-only image.
            result = subprocess.run(["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse",
                                     "-noautoopen", "-owners", "on", "-mountpoint", str(mount),
                                     "-plist", str(image.resolve())], check=True,
                                    capture_output=True, timeout=120)
            require(len(result.stdout) <= 256 * 1024, "Unexpected hdiutil report size")
            attachment = plistlib.loads(result.stdout)
            require(os.path.ismount(mount) and any(e.get("mount-point") == str(mount)
                    for e in attachment.get("system-entities", [])), "Image mounted at an unexpected path")
            require(DMG.sha256(mount / "README-install.txt") == report["instructions_sha256"],
                    "Disk-image installation instructions changed")
            root = mount / "PCL-RH"
            manifest = DMG.VERIFY.verify_manifest(root, portable / "package-manifest.json")
            require(DMG.sha256(root / "package-manifest.json") == report["portable_manifest_sha256"]
                    and manifest["commit"] == report["commit"] and manifest["arch"] == report["arch"],
                    "Disk image does not contain the verified producer")
            copied = scratch / "writable copy" / "PCL-RH"
            shutil.copytree(root, copied)
            DMG.VERIFY.verify_manifest(copied, portable / "package-manifest.json")
            build = DMG.VERIFY.smoke(copied, scratch, manifest)
            entry = copied / "Start PCL RH.command"
            entry_build = DMG.VERIFY.verify_desktop_build(
                DMG.VERIFY.run([str(entry), "--build-info"], scratch), manifest)
            require(entry_build == build, "Copied Finder entry ran a different executable")
            require(not (copied / ".pcl-rust").exists(), "Verification unexpectedly created user data")
        finally:
            if os.path.ismount(mount):
                subprocess.run(["/usr/bin/hdiutil", "detach", str(mount)], check=True, timeout=60)
        require(not os.path.ismount(mount), "Test image was not detached")
    result = {"image": name, "source_commit": report["commit"], "mounted_manifest_verified": True,
              "writable_copy_verified": True, "copied_finder_entry": True,
              "detached": True, "desktop_build": build, "desktop_or_game_started": False}
    (directory / "dmg-verification.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--portable", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(verify(args.directory.resolve(), args.portable.resolve())))
