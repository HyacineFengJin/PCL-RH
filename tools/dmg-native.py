#!/usr/bin/env python3
"""Create an unsigned macOS portable-folder or standalone-app disk image.

The default image contains the existing complete, writable-folder distribution.
In that mode it is not a standalone .app, and the user must copy PCL-RH out of
the read-only volume before starting it. hdiutil
only receives a private extracted tree, never the checkout or user data. With
--app-directory, the image contains an already verified split-v1 .app instead;
its application resources remain separate from the user's writable data root.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
from typing import NamedTuple


ROOT = Path(__file__).resolve().parent


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


VERIFY = load("pcl_dmg_verify", ROOT / "ci/verify-native-package.py")
PACKAGE = load("pcl_dmg_package", ROOT / "package-native.py")
LEGAL = load("pcl_dmg_legal", ROOT / "licenses/release.py")
INSTRUCTIONS = (
    "PCL RH\n\n"
    "将整个 PCL-RH 文件夹复制到你可写的位置，再打开其中的 Start PCL RH.command。\n"
    "请勿直接从此只读磁盘映像启动，也不要只复制一个程序文件。\n"
    "更新前退出启动器，保留原目录的 .pcl-rust、Minecraft 与自建工程。\n"
    "这是未签名、未经公证的便携候选；它不是独立 .app。\n\n"
    "Copy the entire PCL-RH folder to a writable location, then open\n"
    "Start PCL RH.command inside that copy. Do not run from this read-only\n"
    "image or copy only the executable. For updates, close the launcher and\n"
    "preserve .pcl-rust, Minecraft and your projects in the original folder.\n"
    "This is an unsigned, unnotarized portable candidate, not a standalone .app.\n"
).encode("utf-8")
APP_INSTRUCTIONS = (
    "PCL RH\n\n"
    "将 PCL RH.app 拖到 Applications，或复制到你的个人应用目录，再打开复制后的应用。\n"
    "程序资源保留在应用内；用户数据默认位于 ~/Library/Application Support/PCL RH。\n"
    "更新前退出启动器并替换应用，保留外部用户数据、Minecraft 存档与自建工程。\n"
    "这是未签名、未经公证的独立应用候选。\n\n"
    "Drag PCL RH.app to Applications, or copy it to your personal applications\n"
    "folder, then open the copied app. Program resources stay inside the app;\n"
    "user data defaults to ~/Library/Application Support/PCL RH.\n"
    "For updates, close the launcher and replace the app, preserving the separate\n"
    "user data, Minecraft worlds, and projects. This candidate is unsigned and\n"
    "has not been notarized.\n"
).encode("utf-8")


class AppInputs(NamedTuple):
    module: object
    bundle: Path
    manifest: dict
    report: dict


def sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def verified_portable(directory: Path, scratch: Path) -> tuple[Path, dict, str]:
    archives = list(directory.glob("*.tar.gz"))
    PACKAGE.require(len(archives) == 1, "Expected one macOS portable archive")
    archive = archives[0].resolve()
    digest = sha256(archive)
    PACKAGE.require(archive.with_name(archive.name + ".sha256").read_text().split()
                    == [digest, archive.name], "Portable archive hash mismatch")
    root = VERIFY.unpack(archive, scratch)
    manifest = VERIFY.verify_manifest(root, directory / "package-manifest.json")
    PACKAGE.require(manifest["platform"] == "macos", "Expected a macOS portable candidate")
    PACKAGE.checked_version(manifest["version"])
    # The portable assembler never emits user state. Keep that boundary even
    # when a manually supplied archive has an internally consistent manifest.
    PACKAGE.require(not any(entry["path"].split("/", 1)[0] in (".pcl-rust", ".pcl-linux", "Minecraft")
                            for entry in manifest["files"]),
                    "A distribution image must not contain user state")
    extra = {"pcl-desktop", "pcl-cli", "Start PCL RH.command", "README-native.txt",
             "pcl-rh-license-notices.zip"} | PACKAGE.NATIVE_ATTRIBUTION_FILES
    PACKAGE.require(all(entry["path"] in extra or entry["path"] in LEGAL.DOCUMENTS
                        or PACKAGE.selected_resource(entry["path"], "start-native.sh")
                        or entry["path"].startswith("experimental/runtime/node_modules/")
                        for entry in manifest["files"]), "Image contains a non-production resource")
    entry = root / "Start PCL RH.command"
    PACKAGE.require(entry.read_bytes() == (root / "start-native.sh").read_bytes()
                    and entry.stat().st_mode & 0o100, "Invalid Finder portable entry")
    for binary in ("pcl-desktop", "pcl-cli"):
        VERIFY.native_binary(root / binary)
    return root, manifest, digest


def verified_app(directory: Path, app_directory: Path, stage: Path) -> AppInputs:
    # app-native itself loads this module for portable validation. Loading it
    # only inside the app branch keeps both tools' ordinary imports acyclic.
    app = load("pcl_dmg_app_source", ROOT / "app-native.py")
    report = json.loads((app_directory / "app-manifest.json").read_bytes())
    PACKAGE.require(report["archive"] == "mac-app.tar.gz", "Unknown application archive name")
    archive = app_directory / report["archive"]
    digest = sha256(archive)
    PACKAGE.require(digest == report["sha256"]
                    and archive.with_name(archive.name + ".sha256").read_text().split() == [digest, archive.name],
                    "Application archive checksum mismatch")
    sources = list(directory.glob("*.tar.gz"))
    PACKAGE.require(len(sources) == 1 and sha256(sources[0]) == report["source_archive_sha256"]
                    and sources[0].with_name(sources[0].name + ".sha256").read_text().split()
                    == [report["source_archive_sha256"], sources[0].name],
                    "Application was built from a different portable archive")
    bundle = app.unpack(archive, stage)
    manifest = app.verify_bundle(bundle, report, directory / "package-manifest.json")
    expected = VERIFY.expected_source_commit()
    PACKAGE.require(expected is None or manifest["commit"] == expected, "Application belongs to a different producer commit")
    for name in sorted(app.BINARIES):
        VERIFY.native_binary(bundle / "Contents/MacOS" / name)
    return AppInputs(app, bundle, manifest, report)


def build(directory: Path, output: Path, app_directory: Path | None = None) -> dict:
    PACKAGE.require(platform.system() == "Darwin", "Build disk images on a native macOS host")
    output.mkdir(parents=True, exist_ok=True)
    PACKAGE.regular_directory(output)
    with tempfile.TemporaryDirectory(prefix=".pcl-dmg-", dir=output) as temporary:
        scratch = Path(temporary).resolve()
        stage = scratch / "image-root"
        stage.mkdir()
        if app_directory is None:
            root, manifest, source_hash = verified_portable(directory, stage)
            instructions = INSTRUCTIONS
            portable_manifest_hash = sha256(root / "package-manifest.json")
        else:
            app = verified_app(directory, app_directory, stage)
            manifest, source_hash = app.manifest, app.report["source_archive_sha256"]
            instructions = APP_INSTRUCTIONS
            portable_manifest_hash = app.report["portable_manifest_sha256"]
            (stage / "Applications").symlink_to("/Applications", target_is_directory=True)
            stage.chmod(0o755)
        readme = stage / "README-install.txt"
        readme.write_bytes(instructions)
        readme.chmod(0o644)
        base = f'pcl-rh-{manifest["version"]}-macos-{manifest["arch"]}-{manifest["profile"]}'
        if app_directory is not None:
            base += "-app"
        names = (base + ".dmg", base + ".dmg.sha256", "dmg-manifest.json")
        PACKAGE.require(not any(os.path.lexists(output / name) for name in names),
                        "Refusing to replace an existing disk image output")
        image = scratch / (base + ".dmg")
        # Full runtime trees contain many small files. Intel CI can spend more
        # than five minutes creating the filesystem; keep a finite ten-minute
        # bound and expose hdiutil's progress instead of an opaque silent wait.
        # Indexing this delivery-only volume is unnecessary. This flag affects
        # the new image, not the host's Spotlight service or user volumes.
        print("Creating read-only " + ("application" if app_directory is not None else "portable") + " disk image...", flush=True)
        subprocess.run(["/usr/bin/hdiutil", "create", "-srcfolder", str(stage),
                        "-volname", "PCL RH " + manifest["version"], "-fs", "HFS+",
                        "-nospotlight", "-puppetstrings", "-format", "UDZO",
                        "-o", str(image)], check=True, timeout=600)
        print("Checking the disk-image checksum...", flush=True)
        subprocess.run(["/usr/bin/hdiutil", "verify", str(image)], check=True, timeout=120)
        report = {"schema_version": 1, "distribution": "macos-app-dmg" if app_directory is not None else "macos-portable-dmg",
                  "image": names[0], "sha256": sha256(image), "format": "UDZO",
                  "commit": manifest["commit"], "platform": "macos", "arch": manifest["arch"],
                  "profile": manifest["profile"], "version": manifest["version"],
                  "distribution_tool_commit": os.environ.get("GITHUB_SHA"),
                  "source_archive_sha256": source_hash,
                  "portable_manifest_sha256": portable_manifest_hash,
                  "instructions_sha256": hashlib.sha256(instructions).hexdigest(),
                  "signed": False, "notarized": False,
                  "requires_writable_copy": app_directory is None, "user_data_included": False}
        if app_directory is not None:
            report.update({"app_manifest_sha256": sha256(app_directory / "app-manifest.json"),
                           "app_archive_sha256": app.report["sha256"],
                           "application_tool_commit": app.report["distribution_tool_commit"],
                           "bundle": app.module.BUNDLE, "runtime_layout": "split-v1",
                           "applications_link": "/Applications", "volume_read_only": True,
                           "read_only_resources": True, "user_data_policy": "external-per-user"})
        checksum = scratch / names[1]
        checksum.write_text(report["sha256"] + "  " + names[0] + "\n", encoding="ascii")
        metadata = scratch / names[2]
        metadata.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        created = []
        try:
            for source, name in zip((image, checksum, metadata), names):
                with (output / name).open("xb") as target:
                    created.append(output / name)
                    with source.open("rb") as incoming:
                        shutil.copyfileobj(incoming, target, 128 * 1024)
        except BaseException:
            for path in created:
                path.unlink()
            raise
        return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--app-directory", type=Path)
    args = parser.parse_args()
    print(json.dumps(build(args.directory.resolve(), args.output.resolve(),
                           args.app_directory.resolve() if args.app_directory else None)))
