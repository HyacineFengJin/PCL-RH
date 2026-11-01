#!/usr/bin/env python3
"""Create an unsigned macOS disk image from a verified portable archive.

The image contains the existing complete, writable-folder distribution. It is
not a standalone .app: no user state is placed inside a signed bundle, and the
user must copy PCL-RH out of the read-only volume before starting it. hdiutil
only receives a private extracted tree, never the checkout or user data.
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
             "pcl-rh-license-notices.zip"}
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


def build(directory: Path, output: Path) -> dict:
    PACKAGE.require(platform.system() == "Darwin", "Build disk images on a native macOS host")
    output.mkdir(parents=True, exist_ok=True)
    PACKAGE.regular_directory(output)
    with tempfile.TemporaryDirectory(prefix=".pcl-dmg-", dir=output) as temporary:
        scratch = Path(temporary).resolve()
        stage = scratch / "image-root"
        stage.mkdir()
        root, manifest, source_hash = verified_portable(directory, stage)
        (stage / "README-install.txt").write_bytes(INSTRUCTIONS)
        base = f'pcl-rh-{manifest["version"]}-macos-{manifest["arch"]}-{manifest["profile"]}'
        names = (base + ".dmg", base + ".dmg.sha256", "dmg-manifest.json")
        PACKAGE.require(not any(os.path.lexists(output / name) for name in names),
                        "Refusing to replace an existing disk image output")
        image = scratch / (base + ".dmg")
        # Full runtime trees contain many small files. Intel CI can spend more
        # than five minutes creating the filesystem; keep a finite ten-minute
        # bound and expose hdiutil's progress instead of an opaque silent wait.
        # Indexing this delivery-only volume is unnecessary. This flag affects
        # the new image, not the host's Spotlight service or user volumes.
        print("Creating read-only portable disk image...", flush=True)
        subprocess.run(["/usr/bin/hdiutil", "create", "-srcfolder", str(stage),
                        "-volname", "PCL RH " + manifest["version"], "-fs", "HFS+",
                        "-nospotlight", "-puppetstrings", "-format", "UDZO",
                        "-o", str(image)], check=True, timeout=600)
        print("Checking the disk-image checksum...", flush=True)
        subprocess.run(["/usr/bin/hdiutil", "verify", str(image)], check=True, timeout=120)
        report = {"schema_version": 1, "distribution": "macos-portable-dmg",
                  "image": names[0], "sha256": sha256(image), "format": "UDZO",
                  "commit": manifest["commit"], "platform": "macos", "arch": manifest["arch"],
                  "profile": manifest["profile"], "version": manifest["version"],
                  "distribution_tool_commit": os.environ.get("GITHUB_SHA"),
                  "source_archive_sha256": source_hash,
                  "portable_manifest_sha256": sha256(root / "package-manifest.json"),
                  "instructions_sha256": hashlib.sha256(INSTRUCTIONS).hexdigest(),
                  "signed": False, "notarized": False,
                  "requires_writable_copy": True, "user_data_included": False}
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
    args = parser.parse_args()
    print(json.dumps(build(args.directory.resolve(), args.output.resolve())))
