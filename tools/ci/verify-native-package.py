#!/usr/bin/env python3
"""Smoke-check a CI portable archive in a separate extracted tree.

This is a packaging check, not a desktop/game or author-code execution test.
Only the desktop's build-info path, CLI and trusted declarative Node host run,
against new data. The desktop report exits before any GUI/state initialization.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tempfile
import zipfile


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def relative_name(name: str) -> PurePosixPath:
    value = PurePosixPath(name)
    require(bool(name) and not value.is_absolute() and "\\" not in name
            and all(part not in ("", ".", "..") for part in name.rstrip("/").split("/"))
            and value.parts[0] == "PCL-RH", "Invalid archive member: " + name)
    return value


def unpack(archive: Path, destination: Path) -> Path:
    # The assembler emits only regular files/directories. Reject links rather
    # than letting platform-specific extractors silently change their meaning.
    names: set[str] = set()
    total = 0
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as source:
            entries = source.infolist()
            for entry in entries:
                relative_name(entry.filename)
                mode = entry.external_attr >> 16
                require(not stat.S_ISLNK(mode), "Archive contains a symbolic link")
                require(entry.filename not in names, "Duplicate archive member")
                names.add(entry.filename)
                total += entry.file_size
            require(len(entries) <= 100_000 and total <= 2 * 1024**3,
                    "Archive exceeds smoke-check bounds")
            source.extractall(destination)
    else:
        with tarfile.open(archive, "r:gz") as source:
            entries = source.getmembers()
            for entry in entries:
                relative_name(entry.name)
                require(entry.isfile() or entry.isdir(), "Archive contains a link or special file")
                require(entry.name not in names, "Duplicate archive member")
                names.add(entry.name)
                total += entry.size
            require(len(entries) <= 100_000 and total <= 2 * 1024**3,
                    "Archive exceeds smoke-check bounds")
            source.extractall(destination, filter="data")
    return destination / "PCL-RH"


def native_binary(path: Path) -> None:
    with path.open("rb") as source:
        header = source.read(1024 * 1024)
    arm = platform.machine().lower() in ("arm64", "aarch64")
    if platform.system() == "Windows":
        require(header[:2] == b"MZ" and len(header) >= 64, "Missing PE executable header")
        offset = struct.unpack_from("<I", header, 60)[0]
        require(offset + 6 <= len(header) and header[offset:offset + 4] == b"PE\0\0",
                "Invalid PE header offset")
        require(struct.unpack_from("<H", header, offset + 4)[0] == (0xAA64 if arm else 0x8664),
                "Executable architecture differs from native runner")
    elif platform.system() == "Darwin":
        require(header[:4] == b"\xcf\xfa\xed\xfe", "Missing native 64-bit Mach-O header")
        require(struct.unpack_from("<I", header, 4)[0] == (0x100000C if arm else 0x1000007),
                "Executable architecture differs from native runner")
    else:
        require(header[:6] == b"\x7fELF\x02\x01", "Missing native 64-bit little-endian ELF header")
        require(struct.unpack_from("<H", header, 18)[0] == (183 if arm else 62),
                "Executable architecture differs from native runner")
    if platform.system() != "Windows":
        require(bool(path.stat().st_mode & stat.S_IXUSR), "Executable permission was lost")


def verify_manifest(root: Path, external: Path) -> dict:
    raw = (root / "package-manifest.json").read_bytes()
    require(raw == external.read_bytes(), "Internal and external manifests differ")
    manifest = json.loads(raw)
    require(manifest["schema_version"] == 1
            and manifest["product_id"] == "dev.pcl-rh.portable"
            and manifest["layout"] == "flat" and manifest["signed"] is False,
            "Unsupported portable preview manifest")
    require(manifest["profile"] in ("debug", "release"), "Unknown Cargo profile")
    require(isinstance(manifest.get("version"), str) and isinstance(manifest.get("cli_version"), str)
            and type(manifest.get("curseforge_embedded")) is bool, "Missing package version or service declaration")
    expected_system = {"Linux": "linux", "Windows": "windows", "Darwin": "macos"}[platform.system()]
    expected_arch = "aarch64" if platform.machine().lower() in ("aarch64", "arm64") else "x86_64"
    require(manifest["platform"] == expected_system and manifest["arch"] == expected_arch,
            "Manifest does not describe this native runner")
    if os.environ.get("GITHUB_SHA"):
        require(manifest["commit"] == os.environ["GITHUB_SHA"], "Package belongs to another commit")
    expected: set[str] = set()
    for entry in manifest["files"]:
        name = entry["path"]
        relative_name("PCL-RH/" + name)
        require(name not in expected and name != "package-manifest.json", "Duplicate manifest member")
        expected.add(name)
        path = root / name
        require(path.is_file() and path.stat().st_size == entry["bytes"], "Missing or truncated member: " + name)
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        require(digest == entry["sha256"], "Member hash mismatch: " + name)
        if platform.system() != "Windows":
            require(stat.S_IMODE(path.stat().st_mode) == entry["mode"], "Member permissions changed: " + name)
    actual = {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()}
    require(actual == expected | {"package-manifest.json"}, "Unrecorded or missing packaged files")
    return manifest


def verify_desktop_build(raw: str, manifest: dict) -> dict:
    require(len(raw.encode("utf-8")) <= 16 * 1024, "Desktop build information exceeds its output bound")
    info = json.loads(raw)
    fields = {"schema_version", "product_id", "version", "commit", "profile", "target",
              "target_os", "target_arch", "debug_assertions", "custom_protocol", "curseforge_embedded"}
    require(isinstance(info, dict) and set(info) == fields
            and type(info["schema_version"]) is int and info["schema_version"] == 1
            and info["product_id"] == "dev.pcl-rh.desktop", "Unsupported desktop build information")
    require(info["version"] == manifest["version"], "Desktop version differs from the package")
    require(isinstance(info["commit"], str) and re.fullmatch(r"[0-9a-fA-F]{40}", info["commit"]) is not None
            and info["commit"] == manifest["commit"], "Desktop commit differs from the package or is unknown")
    if os.environ.get("GITHUB_SHA"):
        require(info["commit"] == os.environ["GITHUB_SHA"], "Desktop belongs to another CI commit")
    require(info["profile"] == manifest["profile"] and info["profile"] in ("debug", "release"),
            "Desktop profile differs from the package")
    require(info["target_os"] == manifest["platform"] and info["target_arch"] == manifest["arch"],
            "Desktop target differs from the package")
    require(isinstance(info["target"], str) and len(info["target"]) <= 128
            and re.fullmatch(r"[a-zA-Z0-9_-]+", info["target"]) is not None
            and info["target"] != "unknown", "Desktop Cargo target is unavailable")
    require(type(info["debug_assertions"]) is bool, "Desktop debug_assertions must be boolean")
    require(info["custom_protocol"] is True, "Desktop does not embed the production WebUI")
    require(type(info["curseforge_embedded"]) is bool
            and info["curseforge_embedded"] == manifest["curseforge_embedded"],
            "Desktop embedded service declaration differs from the package")
    return info


def verify_cli_version(raw: str, expected: str) -> None:
    # subprocess text mode normalizes the native Windows CRLF to one newline.
    require(raw == f"pcl-cli {expected}\n", "Packaged CLI version differs from its declared version")


def pe_subsystem(path: Path) -> int:
    with path.open("rb") as source:
        header = source.read(1024 * 1024)
    require(len(header) >= 64 and header[:2] == b"MZ", "Missing DOS header")
    offset = struct.unpack_from("<I", header, 60)[0]
    require(offset + 94 <= len(header) and header[offset:offset + 4] == b"PE\0\0"
            and struct.unpack_from("<H", header, offset + 24)[0] == 0x20B,
            "Missing PE32+ optional header")
    return struct.unpack_from("<H", header, offset + 24 + 68)[0]


def verify_windows_entry(root: Path, scratch: Path, build: dict) -> None:
    if platform.system() != "Windows":
        return
    # The production attribute follows assertions, not the Cargo profile label.
    require(pe_subsystem(root / "pcl-desktop.exe") == (3 if build["debug_assertions"] else 2),
            "Desktop PE subsystem differs from the native GUI build contract")
    require(pe_subsystem(root / "pcl-cli.exe") == 3, "CLI lost its console subsystem")
    # This executes the actual GUI PE with explicit inherited pipes. Invalid
    # input must fail through NDJSON, before any window, project or compiler.
    result = subprocess.run([str(root / "pcl-desktop.exe"), "--rh-code-build-runner"],
                            input="{}\n", capture_output=True, text=True,
                            encoding="utf-8", cwd=scratch, timeout=15)
    require(result.returncode == 1 and len(result.stdout) <= 16 * 1024
            and len(result.stderr) <= 16 * 1024, "GUI build helper failed its bounded pipe protocol")
    frames = [json.loads(line) for line in result.stdout.splitlines()]
    require(len(frames) == 1 and frames[0].get("type") == "finished"
            and frames[0].get("error", {}).get("code") == "INVALID_BUILD_REQUEST",
            "GUI PE did not return the actual invalid-request frame")


def run(command: list[str], cwd: Path, *, input_text: str | None = None) -> str:
    result = subprocess.run(command, cwd=cwd, input=input_text, capture_output=True,
                            text=True, encoding="utf-8", timeout=20, check=False)
    require(result.returncode == 0, f"Packaged command failed ({result.returncode}): {result.stderr[-4000:]}")
    require(len(result.stdout) <= 1024 * 1024 and len(result.stderr) <= 1024 * 1024,
            "Packaged command exceeded smoke-check output bounds")
    return result.stdout


def smoke(root: Path, scratch: Path, manifest: dict) -> dict:
    require(not (root / ".pcl-rust").exists(), "Package must not precreate user state")
    marker = json.loads((root / "pcl-rh.json").read_text(encoding="utf-8"))
    require(marker == {"schemaVersion": 1, "productId": "dev.pcl-rh.portable"},
            "Missing portable layout identity")
    suffix = ".exe" if platform.system() == "Windows" else ""
    for name in ("pcl-desktop", "pcl-cli"):
        native_binary(root / (name + suffix))
    desktop = str(root / ("pcl-desktop" + suffix))
    build = verify_desktop_build(run([desktop, "--build-info"], scratch), manifest)
    require(not (root / ".pcl-rust").exists() and not (scratch / ".pcl-rust").exists(),
            "Desktop build report initialized launcher state")
    cli = str(root / ("pcl-cli" + suffix))
    verify_cli_version(run([cli, "--version"], scratch), manifest["cli_version"])
    game = scratch / "empty synthetic game"
    game.mkdir()
    listed = run([cli, "--project", str(root), "--root", str(game), "list"], scratch)
    require(json.loads(listed) == [], "Packaged CLI could not scan an empty relocated root")
    node = shutil.which("node")
    require(node is not None, "CI Node interpreter is missing")
    data = scratch / "private plugin data"
    requests = [{"operation": "extensions_list"}, {"operation": "idle_prepare"},
                {"operation": "shutdown"}]
    output = run([node, str(root / "experimental/host.mjs"), str(data)], scratch,
                 input_text="".join(json.dumps(request) + "\n" for request in requests))
    replies = [json.loads(line) for line in output.splitlines()]
    require(len(replies) == 3 and all(reply.get("ok") is True for reply in replies),
            "Actual packaged plugin host did not complete its protocol")
    require(replies[0]["result"]["entries"] == [] and replies[1]["result"]["busy"] is False
            and replies[2]["result"] is True, "Unexpected packaged plugin state")
    # Resolution checks that the assembled host includes its installed engine
    # tree. It does not initialize a model, use a key, or execute domain tools.
    resolver = ("import {existsSync} from 'node:fs';"
                "import {fileURLToPath} from 'node:url';"
                "import path from 'node:path';"
                "const entry=fileURLToPath(import.meta.resolve('@earendil-works/pi-coding-agent'));"
                "if (!entry.startsWith(path.resolve('node_modules')+path.sep) || "
                "!existsSync(entry)) process.exit(1);")
    run([node, "--input-type=module", "--eval", resolver], root / "experimental/runtime")
    for entry in ("experimental/agent-host.mjs", "experimental/runtime/src/index.mjs",
                  "code/agent-adapter.mjs"):
        run([node, "--check", str(root / entry)], scratch)
    require(not (root / ".pcl-rust").exists() and not (scratch / ".pcl-rust").exists(),
            "Smoke check accidentally initialized launcher state")
    return build


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    archives = sorted([*args.directory.glob("*.tar.gz"), *args.directory.glob("*.zip")])
    require(len(archives) == 1, "Expected exactly one native archive")
    archive = archives[0].resolve()
    sidecar = archive.with_name(archive.name + ".sha256")
    with archive.open("rb") as source:
        actual = hashlib.file_digest(source, "sha256").hexdigest()
    expected = sidecar.read_text(encoding="utf-8").strip().split()
    require(len(expected) == 2 and expected[0] == actual and expected[1] == archive.name,
            "Archive SHA256 sidecar does not match")
    # Keep all synthetic runtime data in the CI output folder, never a user's
    # game/config tree. TemporaryDirectory removes only this owned generation.
    with tempfile.TemporaryDirectory(prefix="relocated space & 项目-", dir=args.directory) as temporary:
        scratch = Path(temporary).resolve()
        root = unpack(archive, scratch)
        manifest = verify_manifest(root, args.directory / "package-manifest.json")
        desktop_build = smoke(root, scratch, manifest)
        verify_windows_entry(root, scratch, desktop_build)
        if platform.system() == "Darwin":
            entry = root / "Start PCL RH.command"
            require(entry.read_bytes() == (root / "start-native.sh").read_bytes()
                    and entry.stat().st_mode & stat.S_IXUSR, "Finder launch entry differs from the portable script")
            require(verify_desktop_build(run([str(entry), "--build-info"], scratch), manifest) == desktop_build,
                    "Finder entry did not execute the packaged desktop")
    print(json.dumps({"archive": archive.name, "sha256": actual,
                      "relocated_cli": True, "declarative_host_rpc": True,
                      "desktop_build": desktop_build,
                      "desktop_or_game_started": False}))


if __name__ == "__main__":
    main()
