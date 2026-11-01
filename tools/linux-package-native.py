#!/usr/bin/env python3
"""Build a Debian package from a hash-verified native portable candidate.

The private stage contains only the portable assembler's production allowlist.
Package files live below /usr; the launcher selects its own per-user data root.
There are no maintainer scripts, data directories, or recursive uninstall rules.
This command builds an archive and never installs a package on the build host.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


VERIFY = load("pcl_deb_verify", ROOT / "ci/verify-native-package.py")
PACKAGE = load("pcl_deb_portable", ROOT / "package-native.py")
LEGAL = load("pcl_deb_legal", ROOT / "licenses/release.py")
require = PACKAGE.require
RESOURCE_ROOT = "/usr/lib/pcl-rh"
ENTRY = b'''#!/bin/sh
# Production resources stay package-owned; Rust selects and initializes user data.
export PCL_RH_RESOURCE_ROOT=/usr/lib/pcl-rh
exec /usr/lib/pcl-rh/pcl-desktop "$@"
'''
DESKTOP = b'''[Desktop Entry]
Type=Application
Name=PCL RH
Comment=Minecraft launcher
Comment[zh_CN]=Minecraft \xe5\x90\xaf\xe5\x8a\xa8\xe5\x99\xa8
Exec=/usr/bin/pcl-rh
TryExec=/usr/bin/pcl-rh
Icon=pcl-rh
Terminal=false
Categories=Game;
StartupWMClass=pcl-desktop
'''
README = b'''PCL RH Debian package

Start PCL RH from the application menu or with /usr/bin/pcl-rh.
Program resources are package-owned in /usr/lib/pcl-rh. User data defaults to
$XDG_DATA_HOME/pcl-rh, or $HOME/.local/share/pcl-rh when XDG_DATA_HOME is unset.
PCL_RH_DATA_ROOT can select another absolute, writable data directory.
Upgrade and removal preserve this data, Minecraft worlds, and user projects.

The package does not install a Node.js or Java distribution. RH Code and
community declarative plugins need Node.js >=22.19 at /usr/bin/node. Game Java
can be downloaded by the launcher. Native author builds need a supported JDK,
bubblewrap >=0.8, a compatible kernel, and the documented isolation preflight.
File selection needs a working XDG Desktop Portal backend; remembered online
login needs an unlocked system Secret Service. These optional services are
not silently configured by this package.

This is an unsigned development candidate. It does not constitute signed
repository distribution, account login, GUI interaction, or game verification.
Offline license texts are in /usr/lib/pcl-rh/LICENSES and the notices ZIP.
'''


def sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def debian_version(version: str, revision: str) -> str:
    PACKAGE.checked_version(version)
    require(re.fullmatch(r"[0-9][0-9A-Za-z.+~]*", revision) is not None
            and len(revision) <= 64, "Invalid Debian package revision")
    # SemVer prereleases sort before a final release in Debian too. The first
    # hyphen separates the prerelease; later identifier hyphens stay intact.
    release, plus, metadata = version.partition("+")
    return release.replace("-", "~", 1) + plus + metadata + "-" + revision


def verified_portable(directory: Path, scratch: Path) -> tuple[Path, dict, str]:
    archives = list(directory.glob("*.tar.gz"))
    require(len(archives) == 1, "Expected one Linux portable archive")
    archive = archives[0].resolve()
    digest = sha256(archive)
    require(archive.with_name(archive.name + ".sha256").read_text().split()
            == [digest, archive.name], "Portable archive hash mismatch")
    root = VERIFY.unpack(archive, scratch)
    manifest = VERIFY.verify_manifest(root, directory / "package-manifest.json")
    require(manifest["platform"] == "linux" and manifest["arch"] == "x86_64",
            "Only a native Linux x86_64 candidate is currently supported")
    require(manifest["profile"] == "release", "System packages require an optimized release candidate")
    PACKAGE.checked_version(manifest["version"])
    require(re.fullmatch(r"[0-9a-f]{40}", manifest["commit"]) is not None,
            "Missing native source commit")
    allowed = {"pcl-desktop", "pcl-cli", "README-native.txt", "pcl-rh-license-notices.zip"}
    names = set()
    for entry in manifest["files"]:
        name = entry["path"]
        require(name.split("/", 1)[0] not in (".pcl-rust", ".pcl-linux", "Minecraft"),
                "System package contains user state")
        require(name in allowed or name in LEGAL.DOCUMENTS
                or PACKAGE.selected_resource(name, "start-native.sh")
                or name.startswith("experimental/runtime/node_modules/"),
                "System package contains a non-production resource: " + name)
        require(type(entry["mode"]) is int and entry["mode"] in (0o644, 0o755),
                "Resource has unsafe permissions: " + name)
        names.add(name)
    require(set(PACKAGE.REQUIRED) | set(LEGAL.DOCUMENTS) | {"pcl-desktop", "pcl-cli", "start-native.sh",
                                   "pcl-rh-license-notices.zip"} <= names,
            "Portable candidate lacks required production resources")
    require(json.loads((root / "pcl-rh.json").read_bytes())
            == {"schemaVersion": 1, "productId": "dev.pcl-rh.portable"},
            "Invalid resource root marker")
    runtime = json.loads((root / "experimental/runtime/package.json").read_bytes())
    lock = json.loads((root / "experimental/runtime/package-lock.json").read_bytes())
    engine = root / "experimental/runtime/node_modules/@earendil-works/pi-coding-agent/package.json"
    require(lock.get("packages", {}).get("", {}).get("dependencies") == runtime.get("dependencies")
            and engine.is_file() and json.loads(engine.read_bytes()).get("version")
            == runtime.get("dependencies", {}).get("@earendil-works/pi-coding-agent")
            and bool(runtime.get("dependencies", {}).get("@earendil-works/pi-coding-agent")),
            "Missing or mismatched production Node runtime dependencies")
    for name in ("pcl-desktop", "pcl-cli"):
        VERIFY.native_binary(root / name)
    return root, manifest, digest


def installed_build_info(root: Path, manifest: dict, scratch: Path) -> dict:
    # Old flat-only executables must be rejected before receiving a new layout
    # parameter or running initialization. --build-info is already state-free.
    raw = VERIFY.run([str(root / "pcl-desktop"), "--build-info"], scratch)
    info = json.loads(raw)
    require(info.get("schema_version") == 2 and info.get("runtime_layout") == "split-v1",
            "Desktop does not support the split-v1 installed resource/data layout")
    return VERIFY.verify_desktop_build(raw, manifest)


def shared_library_dependencies(root: Path, stage: Path) -> str:
    """Resolve ELF library versions from the build distro's actual symbols DB.

    Do not ignore missing metadata: that would publish an installable package
    whose runtime dependencies have never been determined. Foreign Node
    prebuilds and WASI .so modules are not host ELF inputs.
    https://manpages.debian.org/unstable/dpkg-dev/dpkg-shlibdeps.1.en.html
    """
    executables = [root / "pcl-desktop", root / "pcl-cli"]
    for candidate in sorted((root / "experimental/runtime/node_modules").rglob("*")):
        if candidate.is_file() and candidate.suffix in (".node", ".so"):
            with candidate.open("rb") as source:
                header = source.read(64)
            if (len(header) >= 64 and header[:7] == b"\x7fELF\x02\x01\x01"
                    and struct.unpack_from("<H", header, 18)[0] == 62):
                executables.append(candidate)
    metadata = stage / "debian"
    metadata.mkdir()
    (metadata / "control").write_text(
        "Source: pcl-rh\nSection: games\nPriority: optional\n"
        "Maintainer: PCL RH contributors <pcl-rh@users.noreply.github.com>\n\n"
        "Package: pcl-rh\nArchitecture: amd64\nDescription: PCL RH launcher\n", encoding="ascii")
    try:
        result = subprocess.run(["dpkg-shlibdeps", "-O", *["-e" + str(path) for path in executables]],
                                cwd=stage, capture_output=True, text=True, check=True, timeout=120)
    except subprocess.CalledProcessError as error:
        raise ValueError("Could not resolve Debian runtime dependencies: " + (error.stderr or "")[-8000:]) from None
    lines = result.stdout.splitlines()
    require(len(lines) == 1 and lines[0].startswith("shlibs:Depends="),
            "Unexpected shared-library dependency output")
    dependencies = lines[0].removeprefix("shlibs:Depends=")
    require(bool(dependencies) and re.fullmatch(r"[a-zA-Z0-9 ,().+:~<>=|\[\]-]+", dependencies)
            is not None, "Invalid shared-library dependencies")
    return dependencies


def control(version: str, dependencies: str, size: int) -> str:
    return f'''Package: pcl-rh
Version: {version}
Architecture: amd64
Section: games
Priority: optional
Maintainer: PCL RH contributors <pcl-rh@users.noreply.github.com>
Homepage: https://github.com/HyacineFengJin/PCL-RH
Installed-Size: {(size + 1023) // 1024}
Depends: {dependencies}
Recommends: nodejs (>= 22.19.0), bubblewrap (>= 0.8), xdg-desktop-portal
Suggests: default-jdk, gnome-keyring
Description: PCL RH Minecraft launcher
 Native desktop launcher with separate package-owned resources and user data.
 Includes the CLI and production plugin resources; Java and Node.js are external.
'''


def stage_payload(root: Path, manifest: dict, stage: Path) -> list[dict]:
    records = []

    def add(name: str, incoming: Path | bytes, mode: int = 0o644):
        PACKAGE.relative_name(name)
        target = stage / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(incoming, bytes):
            target.write_bytes(incoming)
        else:
            shutil.copyfile(incoming, target)
        target.chmod(mode)
        records.append({"path": name, "bytes": target.stat().st_size,
                        "sha256": sha256(target), "mode": mode})

    for entry in manifest["files"]:
        add("usr/lib/pcl-rh/" + entry["path"], root / entry["path"], entry["mode"])
        require(records[-1]["sha256"] == entry["sha256"] and records[-1]["bytes"] == entry["bytes"],
                "Resource changed while staging: " + entry["path"])
    add("usr/lib/pcl-rh/package-manifest.json", root / "package-manifest.json")
    add("usr/bin/pcl-rh", ENTRY, 0o755)
    add("usr/share/applications/pcl-rh.desktop", DESKTOP)
    add("usr/share/icons/hicolor/256x256/apps/pcl-rh.png", root / "assets/pcl-rh.png")
    # Debian readers expect bundled third-party terms in the package's standard
    # copyright location too, not only in a launcher-specific notices archive.
    copyright = b"PCL RH and bundled production resources\nhttps://github.com/HyacineFengJin/PCL-RH\n"
    for name in LEGAL.DOCUMENTS:
        if not name.endswith(".json"):
            copyright += b"\n===== " + name.encode("utf-8") + b" =====\n" + (root / name).read_bytes() + b"\n"
    add("usr/share/doc/pcl-rh/copyright", copyright)
    add("usr/share/doc/pcl-rh/README.Debian", README)
    # TemporaryDirectory honors a caller's restrictive umask. Directories in a
    # system package must still be traversable by the unprivileged app user.
    for path in stage.rglob("*"):
        if path.is_dir():
            path.chmod(0o755)
    stage.chmod(0o755)
    return sorted(records, key=lambda entry: entry["path"])


def build(directory: Path, output: Path, revision: str = "1") -> dict:
    require(platform.system() == "Linux" and platform.machine().lower() == "x86_64",
            "Build Debian packages on a native Linux x86_64 host")
    require(shutil.which("dpkg-deb") is not None and shutil.which("dpkg-shlibdeps") is not None,
            "Install dpkg-dev to build Debian packages")
    output.mkdir(parents=True, exist_ok=True)
    PACKAGE.regular_directory(output)
    with tempfile.TemporaryDirectory(prefix=".pcl-deb-", dir=output) as temporary:
        scratch = Path(temporary).resolve()
        root, manifest, source_hash = verified_portable(directory, scratch)
        version = debian_version(manifest["version"], revision)
        base = f"pcl-rh_{version}_amd64"
        names = (base + ".deb", base + ".deb.sha256", "linux-package-manifest.json")
        require(not any(os.path.lexists(output / name) for name in names),
                "Refusing to replace an existing Linux package output")
        build_info = installed_build_info(root, manifest, scratch)
        dependencies = shared_library_dependencies(root, scratch)
        stage = scratch / "package"
        stage.mkdir()
        records = stage_payload(root, manifest, stage)
        control_root = stage / "DEBIAN"
        control_root.mkdir(mode=0o755)
        control_root.chmod(0o755)
        (control_root / "control").write_text(control(version, dependencies, sum(e["bytes"] for e in records)),
                                                encoding="utf-8")
        sums = []
        for entry in records:
            with (stage / entry["path"]).open("rb") as source:
                sums.append(hashlib.file_digest(source, "md5").hexdigest() + "  " + entry["path"] + "\n")
        (control_root / "md5sums").write_text("".join(sums), encoding="utf-8")
        for path in control_root.iterdir():
            path.chmod(0o644)
        image = scratch / names[0]
        subprocess.run(["dpkg-deb", "--build", "--root-owner-group", "-Zxz", str(stage), str(image)],
                       check=True, timeout=300)
        report = {"schema_version": 1, "distribution": "linux-deb", "package": names[0],
                  "sha256": sha256(image), "package_name": "pcl-rh", "debian_version": version,
                  "architecture": "amd64", "commit": manifest["commit"], "version": manifest["version"],
                  "profile": manifest["profile"], "runtime_layout": "split-v1",
                  "distribution_tool_commit": os.environ.get("GITHUB_SHA"),
                  "source_archive_sha256": source_hash,
                  "portable_manifest_sha256": sha256(root / "package-manifest.json"),
                  "depends": dependencies, "desktop_build": build_info,
                  "resource_root": RESOURCE_ROOT, "user_data_included": False, "signed": False,
                  "files": records}
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
    parser.add_argument("--revision", default=os.environ.get("GITHUB_RUN_NUMBER", "1"),
                        help="Debian revision; defaults to this workflow's run number, or 1 outside Actions")
    args = parser.parse_args()
    print(json.dumps(build(args.directory.resolve(), args.output.resolve(), args.revision)))
