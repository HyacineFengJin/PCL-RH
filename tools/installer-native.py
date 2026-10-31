#!/usr/bin/env python3
"""Build a per-user Windows installer from an already verified portable archive.

The compiler receives an exact, hash-bound file list, never the checkout or a
recursive glob. User data is absent from that list and from uninstall commands.
This tool only builds; actual installation is a separate, CI-only verification.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
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


VERIFY = load("pcl_installer_archive", ROOT / "ci/verify-native-package.py")
PACKAGE = load("pcl_installer_package", ROOT / "package-native.py")
LEGAL = load("pcl_installer_legal", ROOT / "licenses/release.py")
APP_ID = "dev.pcl-rh.desktop"
ROOT_FILES = {"pcl-rh.json", "pcl-desktop.exe", "pcl-cli.exe", "start-native.ps1",
              "README-native.txt", "package-manifest.json", "pcl-rh-license-notices.zip",
              "LICENSES/INNO-SETUP.txt"}


def language_inputs() -> tuple[bytes, bytes]:
    folder = ROOT / "installer-languages"
    metadata = json.loads((folder / "SOURCES.json").read_bytes())
    item, = metadata["files"]
    PACKAGE.require(item["path"] == "ChineseSimplified.isl", "Unknown installer language input")
    language = (folder / "ChineseSimplified.isl").read_bytes()
    notice = (folder / "NOTICE").read_bytes()
    PACKAGE.require(len(language) == item["bytes"]
                    and hashlib.sha256(language).hexdigest() == item["sha256"],
                    "Pinned installer translation differs from its source record")
    license = metadata["license"]
    PACKAGE.require(license["local_path"] == "NOTICE"
                    and hashlib.sha256(notice[-license["bytes"]:]).hexdigest() == license["sha256"],
                    "Verbatim installer license differs from its source record")
    return language, notice


def member(name: str) -> str:
    """Windows destination semantics, stricter than the portable POSIX spelling."""
    PACKAGE.relative_name(name)
    for part in name.split("/"):
        stem = part.split(".", 1)[0].upper()
        PACKAGE.require(not any(c in part for c in ':*?<>|"{};')
                        and not part.endswith((".", " "))
                        and stem not in {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"}
                        and re.fullmatch(r"(?:COM|LPT)[1-9¹²³]", stem) is None,
                        "Unsafe Windows installer member: " + repr(name))
    PACKAGE.require(name in ROOT_FILES or name in LEGAL.DOCUMENTS
                    or name.startswith(("assets/", "code/", "experimental/")),
                    "Not a program resource: " + repr(name))
    return name.replace("/", "\\")


def inno_script(root: Path, manifest: dict, notice: bytes | None = None) -> str:
    PACKAGE.require(manifest["platform"] == "windows" and manifest["arch"] == "x86_64",
                    "Only native Windows x64 installers are currently supported")
    version = PACKAGE.checked_version(manifest["version"])
    PACKAGE.require(manifest["profile"] in ("debug", "release"), "Unknown profile")
    entries = list(manifest["files"])
    if notice is None:
        _, notice = language_inputs()
    # Inno does not restore every overwritten old file on failure. Replace the
    # desktop first so a running image rejects the upgrade before JS/resources
    # change. Other late failures still require unlocking and rerunning setup.
    entries.sort(key=lambda e: (0 if e["path"] == "pcl-desktop.exe" else
                                1 if e["path"] == "pcl-cli.exe" else 2, e["path"]))
    raw = (root / "package-manifest.json").read_bytes()
    entries.append({"path": "package-manifest.json", "sha256": hashlib.sha256(raw).hexdigest()})
    seen: set[str] = set()
    files = []
    for entry in entries:
        name = member(entry["path"])
        key = name.casefold()
        PACKAGE.require(key not in seen, "Windows installer name collision: " + name)
        seen.add(key)
        digest = entry["sha256"]
        PACKAGE.require(isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest),
                        "Invalid installer member hash")
        parent = str(PurePosixPath(entry["path"]).parent).replace("/", "\\")
        destination = "{app}" + ("\\" + parent if parent != "." else "")
        # Hash is checked by Inno while it reads the source, after our archive
        # validation. No external/download flag or executable script is emitted.
        files.append(f'Source: "PCL-RH\\{name}"; DestDir: "{destination}"; '
                     f'Flags: ignoreversion; Hash: "{digest}"')
    files.append('Source: "installer-languages\\NOTICE"; DestDir: "{app}\\LICENSES"; '
                 'DestName: "INNO-SETUP.txt"; Flags: ignoreversion; Hash: "'
                 + hashlib.sha256(notice).hexdigest() + '"')
    return f'''[Setup]
AppId={APP_ID}
AppName=PCL RH
AppVersion={version}
AppPublisher=PCL RH contributors
DefaultDirName={{localappdata}}\\Programs\\PCL RH
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UsePreviousAppDir=yes
UninstallLogMode=append
CloseApplications=no
RestartApplications=no
AlwaysRestart=no
UninstallRestartComputer=no
SetupIconFile=PCL-RH\\assets\\pcl-rh.ico
UninstallDisplayIcon={{app}}\\pcl-desktop.exe
LicenseFile=PCL-RH\\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
DisableProgramGroupPage=yes
DisableWelcomePage=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimp"; MessagesFile: "installer-languages\\ChineseSimplified.isl"

[Files]
''' + "\n".join(files) + '''

[Icons]
Name: "{autoprograms}\\PCL RH"; Filename: "{app}\\pcl-desktop.exe"; WorkingDir: "{app}"
'''


def compiler() -> Path:
    found = shutil.which("ISCC.exe")
    if found:
        return Path(found)
    for key in ("ProgramFiles(x86)", "ProgramFiles"):
        if os.environ.get(key):
            candidate = Path(os.environ[key]) / "Inno Setup 6/ISCC.exe"
            if candidate.is_file():
                return candidate
    raise ValueError("Install Inno Setup 6.4+ to build the Windows installer")


def build(directory: Path, output: Path) -> dict:
    PACKAGE.require(os.name == "nt", "Build the installer on a native Windows host")
    archives = list(directory.glob("*.zip"))
    PACKAGE.require(len(archives) == 1, "Expected one verified Windows portable ZIP")
    archive = archives[0].resolve()
    with archive.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    expected = archive.with_name(archive.name + ".sha256").read_text().split()
    PACKAGE.require(expected == [digest, archive.name], "Portable ZIP hash mismatch")
    output.mkdir(parents=True, exist_ok=True)
    PACKAGE.regular_directory(output)
    with tempfile.TemporaryDirectory(prefix=".pcl-installer-", dir=output) as temporary:
        scratch = Path(temporary).resolve()
        root = VERIFY.unpack(archive, scratch)
        manifest = VERIFY.verify_manifest(root, directory / "package-manifest.json")
        language, notice = language_inputs()
        inputs = scratch / "installer-languages"
        inputs.mkdir()
        (inputs / "ChineseSimplified.isl").write_bytes(language)
        (inputs / "NOTICE").write_bytes(notice)
        script = inno_script(root, manifest, notice)
        base = f'pcl-rh-{manifest["version"]}-windows-x86_64-{manifest["profile"]}-setup'
        target = output.resolve() / (base + ".exe")
        PACKAGE.require(not any(os.path.lexists(path) for path in
                                (target, target.with_suffix(".exe.sha256"), output / "installer-manifest.json")),
                        "Installer output already exists")
        (scratch / "installer.iss").write_text(script, encoding="utf-8-sig")
        subprocess.run([str(compiler()), "/Qp", "/O" + str(output.resolve()),
                        "/F" + base, "installer.iss"], cwd=scratch, check=True, timeout=600)
        with target.open("rb") as source:
            installer_hash = hashlib.file_digest(source, "sha256").hexdigest()
        target.with_suffix(".exe.sha256").write_text(installer_hash + "  " + target.name + "\n")
        report = {"schema_version": 1, "installer": target.name, "sha256": installer_hash,
                  "source_archive_sha256": digest, "commit": manifest["commit"],
                  "installer_tool_commit": os.environ.get("GITHUB_SHA"),
                  "per_user": True, "signed": False, "user_data_included": False,
                  "installer_resources": [{"path": "LICENSES/INNO-SETUP.txt", "bytes": len(notice),
                                            "sha256": hashlib.sha256(notice).hexdigest()}]}
        (output / "installer-manifest.json").write_text(json.dumps(report, indent=2) + "\n")
        return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(build(args.directory.resolve(), args.output.resolve())))
