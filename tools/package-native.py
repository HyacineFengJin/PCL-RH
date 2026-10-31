#!/usr/bin/env python3
"""Assemble an unsigned native portable archive; never build, install or run it.

Source resources come from committed HEAD blobs, not the checkout. The only
working-tree inputs are the explicit Cargo binary directory and the host's
experimental/runtime/node_modules installation (npm ci is a separate step).
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import platform
import re
import shutil
import stat
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import types
import zipfile

ROOT = Path(__file__).resolve().parents[1]
MAX_FILES = 100_000
MAX_BYTES = 2 * 1024**3
SEMVER_NUMBER = r"(?:0|[1-9][0-9]*)"
SEMVER_IDENTIFIER = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
SEMVER = re.compile(SEMVER_NUMBER + r"\." + SEMVER_NUMBER + r"\." + SEMVER_NUMBER
                    + r"(?:-" + SEMVER_IDENTIFIER + r"(?:\." + SEMVER_IDENTIFIER + r")*)?"
                    + r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?")
REQUIRED = (
    "pcl-rh.json", "code/plugins.json", "code/agent-adapter.mjs",
    "experimental/host.mjs", "experimental/agent-host.mjs", "experimental/python-host.py",
    "experimental/extensions/src/host.mjs", "experimental/extensions/schema/manifest.schema.json",
    "experimental/extensions/schema/runtime-summary.schema.json",
    "experimental/runtime/package.json", "experimental/runtime/package-lock.json",
    "experimental/runtime/src/index.mjs", "assets/pcl-rh.png", "assets/pcl-rh.ico",
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def relative_name(name: str) -> str:
    require(bool(name) and not PurePosixPath(name).is_absolute()
            and not PureWindowsPath(name).drive and "\\" not in name
            and all(part not in ("", ".", "..") for part in name.split("/"))
            and not any(ord(character) < 32 for character in name),
            "Unsafe resource name: " + repr(name))
    return name


def checked_version(value: object) -> str:
    require(isinstance(value, str) and len(value) <= 128 and SEMVER.fullmatch(value) is not None,
            "Expected a filename-safe SemVer version")
    return value


def native_host() -> tuple[str, str]:
    system = {"Linux": "linux", "Windows": "windows", "Darwin": "macos"}.get(platform.system())
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64",
            "aarch64": "aarch64"}.get(platform.machine().lower())
    require((system, arch) in (("linux", "x86_64"), ("windows", "x86_64"),
                              ("macos", "x86_64"), ("macos", "aarch64")),
            "Unsupported native packaging host")
    return system, arch


def validate_binary(header: bytes, system: str, arch: str) -> None:
    arm = arch == "aarch64"
    if system == "linux":
        require(len(header) >= 64 and header[:7] == b"\x7fELF\x02\x01\x01"
                and struct.unpack_from("<H", header, 16)[0] in (2, 3)
                and struct.unpack_from("<H", header, 18)[0] == (183 if arm else 62),
                "Binary is not a native 64-bit ELF executable")
    elif system == "windows":
        require(len(header) >= 64 and header[:2] == b"MZ", "Missing PE executable header")
        offset = struct.unpack_from("<I", header, 60)[0]
        require(offset + 26 <= len(header) and header[offset:offset + 4] == b"PE\0\0"
                and struct.unpack_from("<H", header, offset + 4)[0] == (0xAA64 if arm else 0x8664)
                and struct.unpack_from("<H", header, offset + 22)[0] & 2 != 0
                and struct.unpack_from("<H", header, offset + 24)[0] == 0x20B,
                "Binary is not a native PE32+ executable")
    else:
        require(len(header) >= 32 and header[:4] == b"\xcf\xfa\xed\xfe"
                and struct.unpack_from("<I", header, 4)[0] == (0x100000C if arm else 0x1000007)
                and struct.unpack_from("<I", header, 12)[0] == 2,
                "Binary is not a native 64-bit Mach-O executable")


class Head:
    def __init__(self, root: Path):
        self.root = root
        self.commit = self.git("rev-parse", "HEAD").decode("ascii").strip()
        self.entries: dict[str, tuple[str, str]] = {}
        for entry in self.git("ls-tree", "-r", "-z", "HEAD").split(b"\0"):
            if not entry:
                continue
            metadata, raw_name = entry.split(b"\t", 1)
            mode, kind, oid = metadata.decode("ascii").split()
            name = relative_name(raw_name.decode("utf-8"))
            if kind == "blob":
                self.entries[name] = (mode, oid)

    def git(self, *args: str) -> bytes:
        result = subprocess.run(["git", "-C", str(self.root), *args], check=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        return result.stdout

    def read(self, name: str) -> bytes:
        name = relative_name(name)
        require(name in self.entries, "Missing tracked HEAD resource: " + name)
        mode, oid = self.entries[name]
        require(mode in ("100644", "100755"), "Linked or special HEAD resource: " + name)
        return self.git("cat-file", "blob", oid)


def package_versions(head: Head) -> tuple[str, str]:
    desktop = tomllib.loads(head.read("apps/desktop/src-tauri/Cargo.toml").decode("utf-8"))["package"]
    cli = tomllib.loads(head.read("crates/cli/Cargo.toml").decode("utf-8"))["package"]
    tauri = json.loads(head.read("apps/desktop/src-tauri/tauri.conf.json"))
    require(desktop.get("name") == "pcl-desktop" and cli.get("name") == "pcl-cli",
            "Unexpected Cargo package identity")
    version, cli_version = checked_version(desktop.get("version")), checked_version(cli.get("version"))
    require(checked_version(tauri.get("version")) == version, "Desktop Cargo and Tauri versions differ")
    return version, cli_version


def selected_resource(name: str, startup: str) -> bool:
    """Production source directories only; do not archive the checkout."""
    path = PurePosixPath(name)
    if name in REQUIRED or name in (startup, "experimental/SOURCES.json"):
        return True
    if path.parent == PurePosixPath("code"):
        return path.suffix in (".mjs", ".json", ".mts")
    if path.parent == PurePosixPath("experimental"):
        return path.suffix in (".mjs", ".py")
    roots = ("experimental/extensions/src/", "experimental/extensions/schema/",
             "experimental/runtime/src/", "experimental/runtime/vendor/maker/",
             "experimental/runtime/vendor/porter/")
    return (name.startswith(roots)
            and not any(part in ("tests", "test", "compat", ".github", "__pycache__")
                        for part in path.parts)
            and path.suffix in (".mjs", ".py", ".json"))


def license_inputs(head: Head) -> tuple[types.ModuleType, dict[str, bytes], dict]:
    # Reuse the committed release checker without reading dirty/untracked input
    # files. Its source_bytes seam provides every input from the same HEAD.
    module = types.ModuleType("pcl_tracked_license_release")
    module.__file__ = str(head.root / "tools/licenses/release.py")
    exec(compile(head.read("tools/licenses/release.py"), module.__file__, "exec"), module.__dict__)
    module.source_bytes = head.read
    contents, issues = module.checked_documents()
    return module, contents, issues


def reparse(metadata: os.stat_result) -> bool:
    return bool(getattr(metadata, "st_file_attributes", 0) & 0x400)


def common_stamp(metadata: os.stat_result) -> tuple:
    mode = metadata.st_mode
    if os.name == "nt":
        # CPython's path stat adds 0111 for .exe/.bat/.cmd/.com; fstat has
        # no filename and does not. These are suffix-derived bits, not an
        # NTFS access check. Keep kind/readonly permissions and all identity,
        # time and native attributes, rather than dropping the mode field.
        # https://github.com/python/cpython/blob/v3.13.7/Modules/posixmodule.c
        # update_st_mode_from_path; Python/fileutils.c _Py_fstat_noraise.
        mode &= ~0o111
    return (metadata.st_dev, metadata.st_ino, mode, metadata.st_size,
            metadata.st_mtime_ns, metadata.st_nlink,
            getattr(metadata, "st_file_attributes", None),
            getattr(metadata, "st_reparse_tag", None),
            getattr(metadata, "st_birthtime_ns", None))


def stamp(metadata: os.stat_result) -> tuple:
    # Compare ctime only within one query role. Windows CPython 3.11 uses
    # creation time; in 3.13 win32_xstat_implementation preserves that legacy
    # path result while _Py_fstat_noraise may expose the real ChangeTime.
    # Every role still checks its complete snapshot before/after the copy.
    return (*common_stamp(metadata), metadata.st_ctime_ns)


def regular_directory(path: Path) -> None:
    metadata = path.lstat()
    require(stat.S_ISDIR(metadata.st_mode) and not reparse(metadata),
            "Directory must not be linked or a reparse point: " + str(path))


class Stage:
    def __init__(self, root: Path):
        self.root = root
        self.files: list[dict] = []
        self.total = 0
        self.names: set[str] = set()

    def destination(self, name: str, size: int) -> Path:
        relative_name(name)
        require(name not in self.names, "Duplicate staged resource: " + name)
        require(len(self.names) < MAX_FILES and size >= 0 and self.total + size <= MAX_BYTES,
                "Portable package exceeds file/byte budget")
        destination = self.root / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        return destination

    def record(self, name: str, size: int, digest: str, mode: int) -> None:
        self.names.add(name)
        self.total += size
        self.files.append({"path": name, "bytes": size, "sha256": digest, "mode": mode})

    def add_bytes(self, name: str, data: bytes, mode: int = 0o644) -> None:
        destination = self.destination(name, len(data))
        with destination.open("xb") as output:
            output.write(data)
        destination.chmod(mode)
        self.record(name, len(data), hashlib.sha256(data).hexdigest(), mode)

    def add_file(self, name: str, source: Path, mode: int, *, binary: tuple | None = None) -> None:
        before = source.lstat()
        require(stat.S_ISREG(before.st_mode) and not reparse(before),
                "Input must be a regular, unlinked file: " + str(source))
        destination = self.destination(name, before.st_size)
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
        digest = hashlib.sha256()
        count = 0
        with os.fdopen(os.open(source, flags), "rb") as incoming, destination.open("xb") as outgoing:
            held_before = os.fstat(incoming.fileno())
            require(common_stamp(before) == common_stamp(held_before), "Input changed before copy")
            if binary:
                header = incoming.read(1024 * 1024)
                validate_binary(header, *binary)
                incoming.seek(0)
            while chunk := incoming.read(128 * 1024):
                count += len(chunk)
                require(count <= before.st_size, "Input grew during copy")
                digest.update(chunk)
                outgoing.write(chunk)
            held_after = os.fstat(incoming.fileno())
            path_after = source.lstat()
            require(count == before.st_size and stamp(held_before) == stamp(held_after)
                    and stamp(before) == stamp(path_after)
                    and common_stamp(held_after) == common_stamp(path_after), "Input changed during copy")
        destination.chmod(mode)
        self.record(name, count, digest.hexdigest(), mode)


def node_target(root: Path, path: Path) -> Path:
    """Resolve only relative in-tree file links; never traverse linked dirs."""
    current = path
    for _ in range(16):
        require(current.is_relative_to(root), "Node file link escapes its installation")
        for parent in current.relative_to(root).parents:
            regular_directory(root / parent)
        metadata = current.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            target = os.readlink(current)
            require(not os.path.isabs(target) and not PureWindowsPath(target).drive,
                    "Absolute Node file link is forbidden")
            # abspath normalizes .. without following any directory symlink.
            current = Path(os.path.abspath(current.parent / target))
            continue
        require(stat.S_ISREG(metadata.st_mode) and not reparse(metadata),
                "Node entry is not a regular file")
        require(metadata.st_nlink == 1, "Hardlinked Node input is forbidden")
        return current
    raise ValueError("Too many Node file links")


def add_node_modules(stage: Stage, root: Path) -> None:
    regular_directory(root)
    required = root / "@earendil-works/pi-coding-agent/package.json"
    require(required.is_file(), "Missing installed Pi runtime; run host npm ci separately")
    pending = [root]
    visited = 0
    while pending:
        directory = pending.pop()
        regular_directory(directory)
        with os.scandir(directory) as iterator:
            entries = sorted(iterator, key=lambda entry: entry.name)
        for entry in entries:
            visited += 1
            require(visited <= MAX_FILES, "Node installation exceeds entry budget")
            name = entry.name
            if (name in (".bin", ".cache", ".github", "__pycache__", ".package-lock.json", ".npmrc")
                    or name.startswith(".env") or name.endswith((".pyc", ".log"))):
                continue
            source = Path(entry.path)
            metadata = entry.stat(follow_symlinks=False)
            if stat.S_ISDIR(metadata.st_mode):
                require(not reparse(metadata), "Linked Node directory is forbidden")
                pending.append(source)
            else:
                target = node_target(root, source)
                mode = 0o755 if target.stat().st_mode & 0o111 else 0o644
                stage.add_file("experimental/runtime/node_modules/" + source.relative_to(root).as_posix(),
                               target, mode)


def write_archive(destination: Path, stage: Stage, system: str) -> None:
    # Manual file entries avoid tar's inode-based hardlink optimization. Safe
    # Node file links have already become independent ordinary staged files.
    paths = sorted(path for path in stage.root.rglob("*") if path.is_file())
    # Windows stat cannot express Unix executable bits. Archive the same
    # intended portable mode recorded in the manifest on every build host.
    modes = {entry["path"]: entry["mode"] for entry in stage.files}
    if system == "windows":
        with zipfile.ZipFile(destination, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            for path in paths:
                info = zipfile.ZipInfo("PCL-RH/" + path.relative_to(stage.root).as_posix(),
                                       date_time=(1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                mode = modes[path.relative_to(stage.root).as_posix()]
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                with path.open("rb") as source, archive.open(info, "w", force_zip64=True) as output:
                    shutil.copyfileobj(source, output, 128 * 1024)
    else:
        with destination.open("xb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as archive:
                for path in paths:
                    info = tarfile.TarInfo("PCL-RH/" + path.relative_to(stage.root).as_posix())
                    info.size = path.stat().st_size
                    info.mode = modes[path.relative_to(stage.root).as_posix()]
                    with path.open("rb") as source:
                        archive.addfile(info, source)


def package(root: Path, binaries: Path, output: Path, profile: str, *,
            curseforge_embedded: bool = False) -> dict[str, str]:
    system, arch = native_host()
    require(profile in ("debug", "release"), "Unsupported Cargo profile")
    require(not output.is_relative_to(root / "experimental/runtime/node_modules"),
            "Package output must be outside the installed runtime tree")
    regular_directory(binaries)
    head = Head(root)
    version, cli_version = package_versions(head)
    require(type(curseforge_embedded) is bool, "CurseForge embedded declaration must be boolean")
    startup = "start-native.ps1" if system == "windows" else "start-native.sh"
    for name in (*REQUIRED, startup):
        head.read(name)
    require(json.loads(head.read("pcl-rh.json")) == {
        "schemaVersion": 1, "productId": "dev.pcl-rh.portable"}, "Invalid portable root marker")
    runtime = json.loads(head.read("experimental/runtime/package.json"))
    lock = json.loads(head.read("experimental/runtime/package-lock.json"))
    require(lock.get("packages", {}).get("", {}).get("dependencies") == runtime.get("dependencies")
            and bool(runtime.get("dependencies", {}).get("@earendil-works/pi-coding-agent")),
            "Runtime package lock does not describe the committed runtime dependencies")
    module, documents, issues = license_inputs(head)
    suffix = ".zip" if system == "windows" else ".tar.gz"
    archive_name = f"pcl-rh-{version}-{system}-{arch}-{profile}" + suffix
    names = {"archive": archive_name, "sha256": archive_name + ".sha256",
             "manifest": "package-manifest.json", "license": "licenses/pcl-rh-license-notices.zip"}
    output.mkdir(parents=True, exist_ok=True)
    regular_directory(output)
    for name in names.values():
        require(not os.path.lexists(output / name), "Refusing to replace an existing package output: " + name)
    with tempfile.TemporaryDirectory(prefix=".pcl-native-package-", dir=output) as temporary:
        scratch = Path(temporary)
        stage = Stage(scratch / "PCL-RH")
        stage.root.mkdir()
        for name in sorted(head.entries):
            if selected_resource(name, startup):
                stage.add_bytes(name, head.read(name), 0o755 if name == "start-native.sh" else 0o644)
        if system == "macos":
            # Finder recognizes .command as a Terminal launch entry. It is the
            # same script, with the same quoted root/argv handling, not an .app
            # bundle with writable state hidden inside its signed resources.
            stage.add_bytes("Start PCL RH.command", head.read("start-native.sh"), 0o755)
        for name, data in documents.items():
            if name not in stage.names:
                stage.add_bytes(name, data)
        extension = ".exe" if system == "windows" else ""
        for executable in ("pcl-desktop", "pcl-cli"):
            name = executable + extension
            stage.add_file(name, binaries / name, 0o755, binary=(system, arch))
        add_node_modules(stage, root / "experimental/runtime/node_modules")
        require(json.loads((stage.root / "experimental/runtime/node_modules/"
                            "@earendil-works/pi-coding-agent/package.json").read_bytes()).get("version")
                == runtime["dependencies"]["@earendil-works/pi-coding-agent"],
                "Installed Pi version differs from the committed runtime dependency")
        prerequisites = {
            "linux": "Linux: WebKitGTK 4.1 and its normal desktop runtime libraries are required.",
            "windows": "Windows: Microsoft Edge WebView2 Runtime must already be installed.",
            "macos": "macOS: uses the system WebKit; use the archive for your native CPU architecture.\nOpen Start PCL RH.command in Finder, or run ./start-native.sh from Terminal.",
        }
        credential_notice = (
            "CurseForge service credentials are declared embedded in this build.\n"
            if curseforge_embedded else
            "No CurseForge service credential is declared for this package.\n")
        readme = (f"PCL RH {version} portable {system}/{arch} ({profile})\n\n"
                  + ("Double-click pcl-desktop.exe. Keep the extracted resources together.\n"
                   if system == "windows" else
                   f"Start with {startup} from this extracted folder. Keep its resources together.\n")
                  + prerequisites[system] + "\n"
                  "RH Code and community declarative plugin services need external Node.js >=22.19\n"
                  "at a supported standard installation location.\n"
                  "The archive includes runtime dependencies, not a Node.js or Java installation.\n"
                  "Game Java can be downloaded by the launcher; author builds need an external supported JDK.\n"
                  "Native author-code builds and private-pack Java processors remain platform restricted.\n"
                  "This unsigned development/test archive uses manual replacement for updates.\n"
                  + credential_notice)
        stage.add_bytes("README-native.txt", readme.encode("utf-8"))
        license_path = scratch / "license-notices.zip"
        module.write_bundle(license_path, documents, issues)
        stage.add_file("pcl-rh-license-notices.zip", license_path, 0o644)
        manifest = {"schema_version": 1, "product_id": "dev.pcl-rh.portable", "commit": head.commit,
                    "version": version, "cli_version": cli_version,
                    "curseforge_embedded": curseforge_embedded,
                    "commit_scope": "tracked_resources", "binary_source_commit": None,
                    "binary_provenance": "explicit_native_inputs",
                    "platform": system, "arch": arch, "profile": profile, "layout": "flat",
                    "signed": False, "files": sorted(stage.files, key=lambda entry: entry["path"])}
        manifest_bytes = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
        # The manifest's own bytes are identical outside/inside the archive and
        # intentionally omitted from its contents list (no self-referential hash).
        stage.add_bytes("package-manifest.json", manifest_bytes)
        archive = scratch / archive_name
        write_archive(archive, stage, system)
        with archive.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        checksum = scratch / "archive.sha256"
        checksum.write_text(digest + "  " + archive_name + "\n", encoding="ascii")
        sources = {"archive": archive, "sha256": checksum,
                   "manifest": stage.root / "package-manifest.json", "license": license_path}
        created: list[Path] = []
        try:
            for key, name in names.items():
                destination = output / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                regular_directory(destination.parent)
                with destination.open("xb") as outgoing:
                    created.append(destination)
                    with sources[key].open("rb") as incoming:
                        shutil.copyfileobj(incoming, outgoing, 128 * 1024)
        except BaseException:
            for destination in created:
                destination.unlink()
            raise
    return names


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True, help="explicit native Cargo output directory")
    parser.add_argument("--profile", choices=("debug", "release"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--curseforge-embedded", action="store_true",
                        help="declare an embedded service credential; archive verification must confirm it")
    args = parser.parse_args()
    try:
        print(json.dumps(package(ROOT, args.binaries.absolute(), args.output.absolute(), args.profile,
                                 curseforge_embedded=args.curseforge_embedded)))
    except (ValueError, OSError, KeyError, UnicodeError, subprocess.CalledProcessError) as error:
        parser.exit(1, "Native packaging failed: " + str(error) + "\n")


if __name__ == "__main__":
    main()
