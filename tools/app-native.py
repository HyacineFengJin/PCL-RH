#!/usr/bin/env python3
"""Repackage a verified macOS portable candidate as a standalone .app archive.

Native executables move to Contents/MacOS; production resources stay in
Contents/Resources/PCL-RH. Neither bundle creation nor its metadata initializes
user data. The unmodified executables must advertise the installed split-v1
layout before receiving any new diagnostic argument. No signing is performed.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DMG = load("pcl_app_source", ROOT / "dmg-native.py")
PACKAGE, VERIFY, LEGAL = DMG.PACKAGE, DMG.VERIFY, DMG.LEGAL
require, sha256 = PACKAGE.require, DMG.sha256
BUNDLE = "PCL RH.app"
RESOURCE = "Contents/Resources/PCL-RH"
SOURCE_MANIFEST = "Contents/Resources/source-package-manifest.json"
ICON = "Contents/Resources/pcl-rh.icns"
EXCLUDED = {"start-native.sh", "Start PCL RH.command", "README-native.txt"}
BINARIES = {"pcl-desktop", "pcl-cli"}


def destination(name: str) -> str | None:
    PACKAGE.relative_name(name)
    if name in EXCLUDED:
        return None
    return "Contents/MacOS/" + name if name in BINARIES else RESOURCE + "/" + name


def icon_bytes(png: bytes) -> bytes:
    require(len(png) >= 33 and png[:8] == b"\x89PNG\r\n\x1a\n"
            and png[8:16] == b"\0\0\0\rIHDR" and struct.unpack_from(">II", png, 16) == (256, 256),
            "The application icon must be the original 256x256 PNG")
    # ic08 is the 256px PNG representation. Preserve the approved image's exact
    # bytes; generating other artwork or resampling is unnecessary for this icon.
    chunk = b"ic08" + struct.pack(">I", len(png) + 8) + png
    return b"icns" + struct.pack(">I", len(chunk) + 8) + chunk


def os_version(value: str) -> tuple[int, int, int]:
    require(re.fullmatch(r"[0-9]{1,4}(?:\.[0-9]{1,4}){1,2}", value) is not None,
            "Invalid macOS deployment version")
    fields = [int(part) for part in value.split(".")]
    return tuple(fields + [0] * (3 - len(fields)))


def deployment_version(raw: str) -> str:
    require(len(raw.encode("utf-8")) <= 1024 * 1024, "otool load commands exceed their output bound")
    versions = []
    for block in re.split(r"(?m)^Load command [0-9]+\s*$", raw):
        match = re.search(r"(?m)^\s*cmd (LC_BUILD_VERSION|LC_VERSION_MIN_MACOSX)\s*$", block)
        if match is None:
            continue
        if match[1] == "LC_BUILD_VERSION":
            require(re.search(r"(?m)^\s*platform (?:1|MACOS)\s*$", block) is not None,
                    "Mach-O deployment target is not macOS")
            field = "minos"
        else:
            field = "version"
        values = re.findall(r"(?m)^\s*" + field + r" ([0-9.]+)\s*$", block)
        require(len(values) == 1, "Invalid Mach-O deployment load command")
        versions.append(values[0])
    require(bool(versions), "Mach-O executable has no macOS deployment target")
    return max(versions, key=os_version)


def native_deployment(path: Path) -> str:
    result = subprocess.run(["/usr/bin/otool", "-l", str(path)], capture_output=True,
                            text=True, encoding="utf-8", check=True, timeout=30)
    return deployment_version(result.stdout)


def info_plist(version: str, minimum: str) -> bytes:
    PACKAGE.checked_version(version)
    os_version(minimum)
    # Apple bundle version fields contain the numeric release, while the
    # external app manifest retains any complete SemVer prerelease/build label.
    numeric = re.match(r"[0-9]+\.[0-9]+\.[0-9]+", version)[0]
    values = {"CFBundleIdentifier": "dev.pcl-rh.desktop", "CFBundleExecutable": "pcl-desktop",
              "CFBundleName": "PCL RH", "CFBundleDisplayName": "PCL RH", "CFBundlePackageType": "APPL",
              "CFBundleVersion": numeric, "CFBundleShortVersionString": numeric,
              "CFBundleInfoDictionaryVersion": "6.0", "CFBundleIconFile": "pcl-rh.icns",
              "LSMinimumSystemVersion": minimum, "NSHighResolutionCapable": True}
    return plistlib.dumps(values, fmt=plistlib.FMT_XML, sort_keys=True)


def required_source_manifest(manifest: dict) -> None:
    names = {entry["path"] for entry in manifest["files"]}
    require(manifest["platform"] == "macos" and manifest["profile"] == "release",
            "Application bundles require a native macOS optimized release candidate")
    require(len(names) == len(manifest["files"]), "Duplicate application source resource")
    require(set(PACKAGE.REQUIRED) | set(LEGAL.DOCUMENTS) | EXCLUDED | BINARIES
            | {"pcl-rh-license-notices.zip"} <= names, "Portable candidate lacks required production resources")
    require(all(type(entry["mode"]) is int and entry["mode"] in (0o644, 0o755)
                for entry in manifest["files"]), "Unsafe application resource permissions")
    extra = BINARIES | EXCLUDED | {"pcl-rh-license-notices.zip"} | PACKAGE.NATIVE_ATTRIBUTION_FILES
    for name in names:
        PACKAGE.relative_name(name)
        require(name.split("/", 1)[0] not in (".pcl-rust", ".pcl-linux", "Minecraft"), "Application includes user state")
        require(name in extra or name in LEGAL.DOCUMENTS or PACKAGE.selected_resource(name, "start-native.sh")
                or name.startswith("experimental/runtime/node_modules/"), "Application includes a non-production resource")


def validated_source(root: Path, manifest: dict) -> None:
    required_source_manifest(manifest)
    require(json.loads((root / "pcl-rh.json").read_bytes())
            == {"schemaVersion": 1, "productId": "dev.pcl-rh.portable"}, "Invalid application resource marker")
    runtime = json.loads((root / "experimental/runtime/package.json").read_bytes())
    lock = json.loads((root / "experimental/runtime/package-lock.json").read_bytes())
    engine = root / "experimental/runtime/node_modules/@earendil-works/pi-coding-agent/package.json"
    require(lock.get("packages", {}).get("", {}).get("dependencies") == runtime.get("dependencies")
            and engine.is_file() and json.loads(engine.read_bytes()).get("version")
            == runtime.get("dependencies", {}).get("@earendil-works/pi-coding-agent")
            and bool(runtime.get("dependencies", {}).get("@earendil-works/pi-coding-agent")),
            "Missing or mismatched production Node runtime dependencies")


def split_build_info(root: Path, manifest: dict, scratch: Path) -> dict:
    raw = VERIFY.run([str(root / "pcl-desktop"), "--build-info"], scratch)
    info = json.loads(raw)
    require(info.get("schema_version") == 2 and info.get("runtime_layout") == "split-v1",
            "Desktop does not support the split-v1 installed resource/data layout")
    return VERIFY.verify_desktop_build(raw, manifest)


def assemble(root: Path, manifest: dict, bundle: Path, minimum: str) -> tuple[list[dict], list[dict]]:
    records, mappings = [], []

    def add(name: str, content: bytes | Path, mode: int):
        target = bundle / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            target.write_bytes(content)
        else:
            shutil.copyfile(content, target)
        target.chmod(mode)
        records.append({"path": name, "bytes": target.stat().st_size, "sha256": sha256(target), "mode": mode})

    for entry in manifest["files"]:
        target = destination(entry["path"])
        if target is None:
            continue
        add(target, root / entry["path"], entry["mode"])
        require(records[-1]["sha256"] == entry["sha256"] and records[-1]["bytes"] == entry["bytes"],
                "Source resource changed while assembling the application")
        mappings.append({"source": entry["path"], "destination": target,
                         "bytes": entry["bytes"], "sha256": entry["sha256"], "mode": entry["mode"]})
    add(SOURCE_MANIFEST, root / "package-manifest.json", 0o644)
    add("Contents/Info.plist", info_plist(manifest["version"], minimum), 0o644)
    add(ICON, icon_bytes((root / "assets/pcl-rh.png").read_bytes()), 0o644)
    bundle.chmod(0o755)
    for path in bundle.rglob("*"):
        if path.is_dir():
            path.chmod(0o755)
    return sorted(records, key=lambda entry: entry["path"]), sorted(mappings, key=lambda entry: entry["source"])


def write_archive(archive: Path, bundle: Path, records: list[dict]) -> None:
    modes = {entry["path"]: entry["mode"] for entry in records}
    with archive.open("xb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as target:
            for path in [bundle, *sorted(bundle.rglob("*"))]:
                relative = path.relative_to(bundle).as_posix()
                entry = tarfile.TarInfo(BUNDLE + ("/" + relative if relative != "." else ""))
                if path.is_dir():
                    entry.type, entry.mode = tarfile.DIRTYPE, 0o755
                    target.addfile(entry)
                else:
                    entry.size, entry.mode = path.stat().st_size, modes[relative]
                    with path.open("rb") as incoming:
                        target.addfile(entry, incoming)


def build(directory: Path, output: Path) -> dict:
    require(platform.system() == "Darwin", "Build application bundles on a native macOS host")
    output.mkdir(parents=True, exist_ok=True)
    PACKAGE.regular_directory(output)
    names = ("mac-app.tar.gz", "mac-app.tar.gz.sha256", "app-manifest.json")
    require(not any(os.path.lexists(output / name) for name in names), "Refusing to replace an existing application output")
    with tempfile.TemporaryDirectory(prefix=".pcl-app-", dir=output) as temporary:
        scratch = Path(temporary).resolve()
        root, manifest, source_hash = DMG.verified_portable(directory, scratch)
        validated_source(root, manifest)
        build_info = split_build_info(root, manifest, scratch)
        deployments = {name: native_deployment(root / name) for name in sorted(BINARIES)}
        minimum = max(["11.0", *deployments.values()], key=os_version)
        bundle = scratch / BUNDLE
        bundle.mkdir()
        records, mappings = assemble(root, manifest, bundle, minimum)
        require(len(records) <= PACKAGE.MAX_FILES and sum(entry["bytes"] for entry in records) <= PACKAGE.MAX_BYTES,
                "Application exceeds the native archive budget")
        archive = scratch / names[0]
        write_archive(archive, bundle, records)
        report = {"schema_version": 1, "distribution": "macos-app-bundle", "archive": names[0],
                  "sha256": sha256(archive), "bundle": BUNDLE, "runtime_layout": "split-v1",
                  "commit": manifest["commit"], "version": manifest["version"], "profile": manifest["profile"],
                  "platform": "macos", "arch": manifest["arch"], "desktop_build": build_info,
                  "distribution_tool_commit": os.environ.get("GITHUB_SHA"),
                  "source_archive_sha256": source_hash, "portable_manifest_sha256": sha256(root / "package-manifest.json"),
                  "minimum_macos_version": minimum, "binary_deployment_targets": deployments,
                  "source_mapping": mappings, "excluded_source_paths": sorted(EXCLUDED),
                  "generated_files": [entry for entry in records
                                      if entry["path"] in (SOURCE_MANIFEST, "Contents/Info.plist", ICON)],
                  "files": records, "signed": False, "notarized": False, "user_data_included": False}
        checksum = scratch / names[1]
        checksum.write_text(report["sha256"] + "  " + names[0] + "\n", encoding="ascii")
        metadata = scratch / names[2]
        metadata.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        created = []
        try:
            for source, name in zip((archive, checksum, metadata), names):
                with (output / name).open("xb") as target:
                    created.append(output / name)
                    with source.open("rb") as incoming:
                        shutil.copyfileobj(incoming, target, 128 * 1024)
        except BaseException:
            for path in created:
                path.unlink()
            raise
        return report


def unpack(archive: Path, destination_root: Path) -> Path:
    seen, total = set(), 0
    with tarfile.open(archive, "r:gz") as source:
        entries = source.getmembers()
        require(len(entries) <= PACKAGE.MAX_FILES * 2, "Application archive exceeds entry budget")
        for entry in entries:
            name = entry.name.rstrip("/")
            PACKAGE.relative_name(name)
            require(name == BUNDLE or name.startswith(BUNDLE + "/"), "Unexpected application archive root")
            require(name not in seen and (entry.isdir() or entry.isfile()), "Duplicate, linked or special application member")
            require(entry.uid == 0 and entry.gid == 0
                    and entry.mode in ((0o755,) if entry.isdir() else (0o644, 0o755)), "Unsafe application archive permissions")
            seen.add(name)
            total += entry.size
        require(total <= PACKAGE.MAX_BYTES, "Application archive exceeds byte budget")
        source.extractall(destination_root, filter="data")
    return destination_root / BUNDLE


def verify_bundle(bundle: Path, report: dict, external_source: Path | None = None) -> dict:
    metadata = bundle.lstat()
    require(stat.S_ISDIR(metadata.st_mode) and stat.S_IMODE(metadata.st_mode) == 0o755,
            "Application root is linked or has unsafe permissions")
    require(report.get("schema_version") == 1 and report.get("distribution") == "macos-app-bundle"
            and report.get("runtime_layout") == "split-v1" and report.get("bundle") == BUNDLE
            and report.get("platform") == "macos" and report.get("profile") == "release"
            and report.get("signed") is False and report.get("notarized") is False
            and report.get("user_data_included") is False, "Unsupported application distribution")
    raw = (bundle / SOURCE_MANIFEST).read_bytes()
    require(hashlib.sha256(raw).hexdigest() == report["portable_manifest_sha256"]
            and (external_source is None or raw == external_source.read_bytes()), "Source package manifest differs")
    manifest = json.loads(raw)
    required_source_manifest(manifest)
    require(manifest["schema_version"] == 1 and manifest["product_id"] == "dev.pcl-rh.portable"
            and manifest["layout"] == "flat" and manifest["signed"] is False
            and all(manifest[key] == report[key] for key in ("commit", "version", "profile", "platform", "arch")),
            "Application source identity differs")
    mappings = [{"source": entry["path"], "destination": destination(entry["path"]),
                 "bytes": entry["bytes"], "sha256": entry["sha256"], "mode": entry["mode"]}
                for entry in manifest["files"] if destination(entry["path"]) is not None]
    require(sorted(mappings, key=lambda entry: entry["source"]) == report["source_mapping"]
            and report["excluded_source_paths"] == sorted(EXCLUDED), "Application source mapping differs")
    expected = {entry["destination"]: {key: entry[key] for key in ("bytes", "sha256", "mode")}
                for entry in mappings}
    generated = {SOURCE_MANIFEST: raw, "Contents/Info.plist": info_plist(manifest["version"], report["minimum_macos_version"]),
                 ICON: icon_bytes((bundle / RESOURCE / "assets/pcl-rh.png").read_bytes())}
    for name, content in generated.items():
        expected[name] = {"bytes": len(content), "sha256": hashlib.sha256(content).hexdigest(), "mode": 0o644}
    records = [{"path": name, **value} for name, value in sorted(expected.items())]
    require(records == report["files"] and report["generated_files"]
            == [entry for entry in records if entry["path"] in generated],
            "Application file list differs from the source mapping")
    actual = set()
    for path in bundle.rglob("*"):
        metadata = path.lstat()
        require(stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode), "Application contains a link or special file")
        name = path.relative_to(bundle).as_posix()
        if stat.S_ISDIR(metadata.st_mode):
            require(stat.S_IMODE(metadata.st_mode) == 0o755
                    and any(value.startswith(name + "/") for value in expected), "Unexpected application directory")
            continue
        require(name in expected, "Application contains an unrecorded file")
        record = expected[name]
        require(metadata.st_size == record["bytes"] and stat.S_IMODE(metadata.st_mode) == record["mode"]
                and sha256(path) == record["sha256"], "Application member changed: " + name)
        actual.add(name)
    require(actual == set(expected), "Application is missing production files")
    for name, content in generated.items():
        require((bundle / name).read_bytes() == content, "Generated application metadata changed")
    return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(build(args.directory.resolve(), args.output.resolve())))
