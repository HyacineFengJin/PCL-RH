#!/usr/bin/env python3
"""Mount only this CI job's DMG; verify it and execute its owned private copy."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import stat
import subprocess
import tempfile

import importlib.util

SPEC = importlib.util.spec_from_file_location("pcl_dmg", Path(__file__).resolve().parents[1] / "dmg-native.py")
assert SPEC is not None and SPEC.loader is not None
DMG = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DMG)


def verify_app(directory: Path, portable: Path, app_directory: Path) -> dict:
    require = DMG.PACKAGE.require
    # Keep app execution/loading lazy: app-native imports the portable DMG
    # validator, and ordinary portable-image checks do not need the app tools.
    execution = DMG.load("pcl_dmg_app_execution", DMG.ROOT / "ci/verify-native-app.py")
    execution.require_ci()
    report = json.loads((directory / "dmg-manifest.json").read_bytes())
    name = report["image"]
    require(isinstance(name, str) and Path(name).name == name and name.endswith("-app.dmg"), "Invalid application image name")
    image = directory / name
    require(DMG.sha256(image) == report["sha256"]
            and image.with_name(name + ".sha256").read_text().split() == [report["sha256"], name],
            "Disk-image checksum differs from its manifest")
    require(report["schema_version"] == 1 and report["distribution"] == "macos-app-dmg" and report["format"] == "UDZO"
            and report["signed"] is False and report["notarized"] is False
            and report["requires_writable_copy"] is False and report["user_data_included"] is False
            and report["volume_read_only"] is True and report["read_only_resources"] is True
            and report["runtime_layout"] == "split-v1" and report["user_data_policy"] == "external-per-user"
            and report["applications_link"] == "/Applications", "Unexpected application disk-image policy")
    require(DMG.sha256(app_directory / "app-manifest.json") == report["app_manifest_sha256"],
            "Disk image was built from a different application manifest")
    with tempfile.TemporaryDirectory(prefix="app dmg space & 项目-", dir=directory) as temporary:
        scratch = Path(temporary).resolve()
        inputs = scratch / "verified application input"
        inputs.mkdir()
        app = DMG.verified_app(portable, app_directory, inputs)
        require(report["bundle"] == app.module.BUNDLE and report["app_archive_sha256"] == app.report["sha256"]
                and report["source_archive_sha256"] == app.report["source_archive_sha256"]
                and report["portable_manifest_sha256"] == app.report["portable_manifest_sha256"]
                and report["application_tool_commit"] == app.report["distribution_tool_commit"]
                and all(report[key] == app.report[key] for key in ("commit", "version", "profile", "platform", "arch")),
                "Disk-image producer differs from the verified application")
        mount = scratch / "mounted"
        mount.mkdir()
        try:
            result = subprocess.run(["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse",
                                     "-noautoopen", "-owners", "on", "-mountpoint", str(mount),
                                     "-plist", str(image.resolve())], check=True, capture_output=True, timeout=120)
            require(len(result.stdout) <= 256 * 1024, "Unexpected hdiutil report size")
            attachment = plistlib.loads(result.stdout)
            require(os.path.ismount(mount) and any(entry.get("mount-point") == str(mount)
                    for entry in attachment.get("system-entities", [])), "Image mounted at an unexpected path")
            require(os.statvfs(mount).f_flag & os.ST_RDONLY, "Application image was not mounted read-only")
            readme = mount / "README-install.txt"
            require(stat.S_ISREG(readme.lstat().st_mode) and DMG.sha256(readme) == report["instructions_sha256"]
                    and readme.read_bytes() == DMG.APP_INSTRUCTIONS, "Application installation instructions changed")
            link = mount / "Applications"
            require(stat.S_ISLNK(link.lstat().st_mode) and os.readlink(link) == "/Applications",
                    "Application image has an unexpected Applications link")
            root = mount / app.module.BUNDLE
            manifest = app.module.verify_bundle(root, app.report, portable / "package-manifest.json")
            # Unlike the historical portable image, a standalone app must also
            # run directly from read-only delivery media. This exercises two
            # real filesystems, not just two folders on one writable volume.
            mounted_home = scratch / "mounted-app-home"
            mounted_home.mkdir(mode=0o700)
            mounted_data = scratch / "mounted-app-data"
            environment = {**os.environ, "HOME": str(mounted_home),
                           "PCL_RH_DATA_ROOT": str(mounted_data)}
            for variable in ("PCL_RH_RESOURCE_ROOT", "PCL_RH_HOME", "PCL_LINUX_HOME"):
                environment.pop(variable, None)
            mounted_check = execution.installed(root, scratch, environment, mounted_data)
            app.module.verify_bundle(root, app.report, portable / "package-manifest.json")
            copied = scratch / "copied application" / app.module.BUNDLE
            # The only delivery symlink lives beside the app and is never
            # traversed or copied. The verified bundle itself contains no links.
            shutil.copytree(root, copied, symlinks=True)
            app.module.verify_bundle(copied, app.report, portable / "package-manifest.json")
            checked = execution.verify_execution(copied, scratch, app.report, manifest)
            require(checked.get("launch_services_entry_checked") is True
                    and checked.get("read_only_bundle_unchanged") is True
                    and checked.get("existing_user_data_preserved") is True,
                    "Copied application did not prove LaunchServices, read-only resources and preserved user data")
        finally:
            if os.path.ismount(mount):
                subprocess.run(["/usr/bin/hdiutil", "detach", str(mount)], check=True, timeout=60)
        require(not os.path.ismount(mount), "Test image was not detached")
    result = {"image": name, "source_commit": report["commit"], "mounted_manifest_verified": True,
              "copied_app_verified": True, "known_applications_link_verified": True,
              "app_launch_services": True, "read_only_bundle": True, "data_preserved": True,
              "mounted_app_data_check": mounted_check,
              "detached": True, "application_verification": checked, "desktop_or_game_started": False,
              "finder_or_gui_started": False, "gatekeeper_assessed": False}
    (directory / "dmg-verification.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return result


def verify(directory: Path, portable: Path, app_directory: Path | None = None) -> dict:
    if app_directory is not None:
        return verify_app(directory, portable, app_directory)
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
    parser.add_argument("--app-directory", type=Path)
    args = parser.parse_args()
    print(json.dumps(verify(args.directory.resolve(), args.portable.resolve(),
                            args.app_directory.resolve() if args.app_directory else None)))
