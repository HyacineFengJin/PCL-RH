#!/usr/bin/env python3
"""Install, upgrade, remove and purge a candidate on a disposable Ubuntu runner.

System package changes are deliberately unavailable outside GitHub-hosted CI.
The app runs as the unprivileged runner user with private HOME/data directories;
only apt/dpkg receives sudo. No GUI, account, game, or AI session is started.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile

import importlib.util

SPEC = importlib.util.spec_from_file_location("pcl_linux_package", Path(__file__).parents[1] / "linux-package-native.py")
assert SPEC is not None and SPEC.loader is not None
LINUX = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LINUX)
require = LINUX.require
RESOURCE = Path(LINUX.RESOURCE_ROOT)


def disposable_runner(directory: Path) -> None:
    require(platform.system() == "Linux" and platform.machine().lower() == "x86_64"
            and os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and os.environ.get("RUNNER_OS") == "Linux" and os.geteuid() != 0,
            "System-package verification requires an unprivileged disposable GitHub-hosted Linux runner")
    release = dict(line.split("=", 1) for line in Path("/etc/os-release").read_text().splitlines()
                   if "=" in line and not line.startswith("#"))
    require(release.get("ID", "").strip('"') == "ubuntu", "Use a disposable Ubuntu runner")
    temporary = Path(os.environ.get("RUNNER_TEMP", "/nonexistent-runner-temp"))
    require(temporary.is_absolute() and temporary.is_dir() and directory != temporary.resolve()
            and directory.is_relative_to(temporary.resolve()), "Verification output must be a child of RUNNER_TEMP")
    require(not os.path.lexists(RESOURCE) and not os.path.lexists("/usr/bin/pcl-rh")
            and not os.path.lexists("/usr/share/applications/pcl-rh.desktop")
            and not os.path.lexists("/usr/share/doc/pcl-rh")
            and not os.path.lexists("/usr/share/icons/hicolor/256x256/apps/pcl-rh.png"),
            "An existing application or registration must not be touched")
    status = subprocess.run(["dpkg-query", "-W", "-f=${db:Status-Abbrev}", "pcl-rh"],
                            capture_output=True, text=True, timeout=15)
    require(status.returncode == 1 and status.stdout == "", "An existing package registration must not be touched")
    require(all(shutil.which(name) for name in ("sudo", "apt-get", "dpkg-deb", "desktop-file-validate")),
            "Missing system-package verification tools")


def safe_tar_name(name: str) -> str:
    while name.startswith("./"):
        name = name[2:]
    name = name.rstrip("/")
    if name in ("", "."):
        return ""
    LINUX.PACKAGE.relative_name(name)
    return name


def checked_archive(directory: Path, report: dict) -> Path:
    require(report.get("schema_version") == 1 and report.get("distribution") == "linux-deb"
            and report.get("runtime_layout") == "split-v1" and report.get("package_name") == "pcl-rh"
            and report.get("architecture") == "amd64" and report.get("profile") == "release"
            and report.get("resource_root") == str(RESOURCE)
            and report.get("user_data_included") is False and report.get("signed") is False,
            "Unsupported Linux package report")
    name = report["package"]
    require(isinstance(name, str) and Path(name).name == name and name.endswith(".deb"), "Invalid package name")
    archive = directory / name
    require(LINUX.sha256(archive) == report["sha256"], "Debian package hash mismatch")
    require(archive.with_name(name + ".sha256").read_text().split() == [report["sha256"], name],
            "Debian package checksum sidecar mismatch")
    expected = {}
    for entry in report["files"]:
        member = entry["path"]
        LINUX.PACKAGE.relative_name(member)
        require(member not in expected and (member.startswith("usr/lib/pcl-rh/")
                or member in {"usr/bin/pcl-rh", "usr/share/applications/pcl-rh.desktop",
                              "usr/share/icons/hicolor/256x256/apps/pcl-rh.png",
                              "usr/share/doc/pcl-rh/copyright", "usr/share/doc/pcl-rh/README.Debian"}),
                "Unexpected package destination")
        require(entry["mode"] in (0o644, 0o755), "Unsafe packaged permissions")
        expected[member] = entry
    # Inspect real Debian data/control streams before any privileged action.
    # Reject scripts and special files even if a report was edited to match.
    actual = set()
    result = subprocess.run(["dpkg-deb", "--fsys-tarfile", str(archive)], capture_output=True, check=True, timeout=120)
    require(len(result.stdout) <= LINUX.PACKAGE.MAX_BYTES + 256 * 1024**2, "Debian payload exceeds bounds")
    with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:") as source:
        entries = source.getmembers()
        require(len(entries) <= LINUX.PACKAGE.MAX_FILES * 2, "Too many Debian package members")
        seen = set()
        for item in entries:
            name = safe_tar_name(item.name)
            require(name not in seen and item.uid == 0 and item.gid == 0,
                    "Duplicate member or non-root package ownership")
            seen.add(name)
            require(item.isdir() or item.isfile(), "Debian package contains a link or special file")
            if item.isdir():
                require(item.mode == 0o755, "Unsafe package directory permissions")
                require(not name or any(value.startswith(name + "/") for value in expected),
                        "Unexpected package directory")
                continue
            require(name in expected, "Unexpected Debian package member: " + name)
            record = expected[name]
            require(item.size == record["bytes"] and item.mode == record["mode"], "Debian member metadata mismatch")
            with source.extractfile(item) as incoming:
                require(hashlib.file_digest(incoming, "sha256").hexdigest() == record["sha256"],
                        "Debian member hash mismatch: " + name)
            actual.add(name)
    require(actual == set(expected), "Debian package members differ from the report")
    result = subprocess.run(["dpkg-deb", "--ctrl-tarfile", str(archive)], capture_output=True, check=True, timeout=30)
    require(len(result.stdout) <= 32 * 1024**2, "Debian control stream exceeds bounds")
    with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:") as source:
        entries = source.getmembers()
        controls = [entry for entry in entries if entry.isfile()]
        names = [safe_tar_name(entry.name) for entry in entries]
        require({safe_tar_name(entry.name) for entry in controls} == {"control", "md5sums"}
                and len(names) == len(set(names))
                and all(entry.isdir() or entry.isfile() for entry in entries),
                "Debian package must have no maintainer scripts or special control files")
    fields = subprocess.check_output(["dpkg-deb", "-f", str(archive), "Package", "Version", "Architecture"],
                                      text=True, timeout=15)
    require(fields == f'Package: pcl-rh\nVersion: {report["debian_version"]}\nArchitecture: amd64\n',
            "Debian control identity differs from the report")
    return archive


def installed_hashes(report: dict) -> dict[str, str]:
    hashes = {}
    for entry in report["files"]:
        path = Path("/") / entry["path"]
        metadata = path.lstat()
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and metadata.st_gid == 0
                and stat.S_IMODE(metadata.st_mode) == entry["mode"]
                and metadata.st_size == entry["bytes"], "Installed metadata differs: " + entry["path"])
        digest = LINUX.sha256(path)
        require(digest == entry["sha256"], "Installed hash differs: " + entry["path"])
        hashes[entry["path"]] = digest
    actual = {"usr/lib/pcl-rh/" + path.relative_to(RESOURCE).as_posix()
              for path in RESOURCE.rglob("*") if path.is_file() or path.is_symlink()}
    expected = {entry["path"] for entry in report["files"] if entry["path"].startswith("usr/lib/pcl-rh/")}
    require(actual == expected, "Installed resource directory has unexpected files")
    for path in [RESOURCE, *[item for item in RESOURCE.rglob("*") if item.is_dir()]]:
        metadata = path.lstat()
        require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == 0 and metadata.st_gid == 0
                and stat.S_IMODE(metadata.st_mode) == 0o755, "Installed directory is writable by the app user")
    return hashes


def previous_revision(archive: Path, report: dict, scratch: Path) -> Path:
    stage = scratch / "previous-package"
    subprocess.run(["dpkg-deb", "--raw-extract", str(archive), str(stage)], check=True, timeout=120)
    version = report["debian_version"].rsplit("-", 1)[0] + "-0.ci"
    require(subprocess.run(["dpkg", "--compare-versions", version, "lt", report["debian_version"]],
                           timeout=15).returncode == 0, "Candidate must upgrade the CI previous revision")
    control = stage / "DEBIAN/control"
    raw = control.read_text()
    raw, count = re.subn(r"(?m)^Version: .+$", "Version: " + version, raw)
    require(count == 1, "Missing previous package version")
    control.write_text(raw)
    (stage / "usr/lib/pcl-rh/previous-package-fixture.txt").write_text("CI-owned obsolete package member\n")
    (stage / "usr/lib/pcl-rh/README-native.txt").write_text("CI-owned previous resource bytes\n")
    sums = []
    for path in sorted((stage / "usr").rglob("*")):
        if path.is_file():
            path.chmod(0o755 if path.relative_to(stage).as_posix() == "usr/bin/pcl-rh"
                       or path.stat().st_mode & 0o111 else 0o644)
            with path.open("rb") as incoming:
                sums.append(hashlib.file_digest(incoming, "md5").hexdigest() + "  "
                            + path.relative_to(stage).as_posix() + "\n")
    (stage / "DEBIAN/md5sums").write_text("".join(sums))
    image = scratch / "previous-ci-revision.deb"
    subprocess.run(["dpkg-deb", "--build", "--root-owner-group", "-Zxz", str(stage), str(image)],
                   check=True, timeout=300)
    return image


def app(command: list[str], cwd: Path, environment: dict) -> str:
    result = subprocess.run(command, cwd=cwd, env=environment, capture_output=True,
                            text=True, encoding="utf-8", timeout=45)
    require(len(result.stdout) <= 1024 * 1024 and len(result.stderr) <= 1024 * 1024,
            "Installed app command exceeded output bounds")
    require(result.returncode == 0, "Installed app command failed: "
            + result.stderr[-4000:] + "\n" + result.stdout[-4000:])
    return result.stdout


def installation_check(data: Path, scratch: Path, manifest: dict, *, default: bool = False) -> dict:
    home = scratch / "private home 项目 & space"
    home.mkdir(mode=0o700, exist_ok=True)
    environment = {**os.environ, "HOME": str(home), "XDG_DATA_HOME": str(scratch / "xdg data 项目 & space"),
                   "XDG_CONFIG_HOME": str(scratch / "xdg config"), "XDG_CACHE_HOME": str(scratch / "xdg cache"),
                   "PCL_RH_DATA_ROOT": str(data), "PCL_RH_RESOURCE_ROOT": "/wrong-inherited-resource-root"}
    if default:
        environment.pop("PCL_RH_DATA_ROOT")
    raw = app(["/usr/bin/pcl-rh", "--build-info"], scratch, environment)
    LINUX.VERIFY.verify_desktop_build(raw, manifest)
    require(json.loads(raw).get("runtime_layout") == "split-v1", "Installed entry lost its layout capability")
    result = json.loads(app(["/usr/bin/pcl-rh", "--installation-check"], scratch, environment))
    require(result.get("schema_version") == 1 and result.get("installed") is True
            and Path(result.get("resource_root", "")).resolve() == RESOURCE
            and Path(result.get("data_root", "")).resolve() == data.resolve()
            and result.get("plugin_host_checked") is True and result.get("plugin_host_retired") is True,
            "Installed entry did not initialize split resources/data and the actual plugin host")
    require(data.is_dir() and data.stat().st_uid == os.geteuid(), "App data is not owned by the runner user")
    return result


def verify_canaries(data: Path, canaries: dict) -> None:
    for name, contents in canaries.items():
        require((data / name).read_bytes() == contents, "Package operation changed user data: " + name)


def run(directory: Path, portable: Path) -> dict:
    disposable_runner(directory)
    report = json.loads((directory / "linux-package-manifest.json").read_bytes())
    archive = checked_archive(directory, report)
    with tempfile.TemporaryDirectory(prefix="installed space & 项目-", dir=directory) as temporary:
        scratch = Path(temporary).resolve()
        source = scratch / "portable-source"
        source.mkdir()
        root, manifest, source_hash = LINUX.verified_portable(portable, source)
        require(source_hash == report["source_archive_sha256"]
                and LINUX.sha256(root / "package-manifest.json") == report["portable_manifest_sha256"]
                and manifest["commit"] == report["commit"], "Portable source differs from the package")
        require(LINUX.stage_payload(root, manifest, scratch / "expected-payload") == report["files"],
                "Debian payload differs from the verified production portable candidate")
        previous = previous_revision(archive, report, scratch)
        data = scratch / "user data 项目 & space"
        data.mkdir(mode=0o700)
        canaries = {".pcl-rust/synthetic-settings.json": b"private fixture settings\n",
                    "Minecraft/.minecraft/saves/synthetic/level.dat": b"private fixture world\n",
                    "custom-project/hand-written.java": b"private handwritten fixture\n",
                    "my-notes.txt": b"private user file\n"}
        for name, contents in canaries.items():
            path = data / name
            path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
            path.write_bytes(contents)
        changed_runner = False
        try:
            changed_runner = True
            subprocess.run(["sudo", "-n", "apt-get", "-y", "--no-install-recommends", "install", str(previous)],
                           check=True, timeout=300)
            require((RESOURCE / "previous-package-fixture.txt").is_file(), "Previous revision did not install")
            verify_canaries(data, canaries)
            subprocess.run(["sudo", "-n", "apt-get", "-y", "--no-install-recommends", "install", str(archive)],
                           check=True, timeout=300)
            require(not (RESOURCE / "previous-package-fixture.txt").exists(), "Upgrade retained an obsolete package member")
            before = installed_hashes(report)
            subprocess.run(["desktop-file-validate", "/usr/share/applications/pcl-rh.desktop"], check=True, timeout=15)
            check = installation_check(data, scratch, manifest)
            default_data = scratch / "xdg data 项目 & space/pcl-rh"
            default_check = installation_check(default_data, scratch, manifest, default=True)
            game = scratch / "empty synthetic game"
            game.mkdir()
            environment = {**os.environ, "PCL_RH_RESOURCE_ROOT": str(RESOURCE), "PCL_RH_DATA_ROOT": str(data)}
            cli = str(RESOURCE / "pcl-cli")
            LINUX.VERIFY.verify_cli_version(app([cli, "--version"], scratch, environment), manifest["cli_version"])
            listed = app([cli, "--project", str(data), "--root", str(game), "list"], scratch, environment)
            require(json.loads(listed) == [], "Installed CLI could not inspect a separate empty game root")
            require(installed_hashes(report) == before, "App initialization changed package resources")
            verify_canaries(data, canaries)
            subprocess.run(["sudo", "-n", "dpkg", "--remove", "pcl-rh"], check=True, timeout=120)
            require(not RESOURCE.exists() and all(not os.path.lexists(Path("/") / entry["path"])
                    for entry in report["files"]), "Package removal retained owned application files")
            verify_canaries(data, canaries)
            require(default_data.is_dir(), "Package removal deleted default per-user data")
            subprocess.run(["sudo", "-n", "apt-get", "-y", "--no-install-recommends", "install", str(archive)],
                           check=True, timeout=300)
            reinstall_check = installation_check(data, scratch, manifest)
            require(installed_hashes(report) == before, "Reinstallation did not restore exact package resources")
            verify_canaries(data, canaries)
            subprocess.run(["sudo", "-n", "dpkg", "--purge", "pcl-rh"], check=True, timeout=120)
            require(not RESOURCE.exists() and not Path("/usr/bin/pcl-rh").exists(), "Purge retained application files")
            verify_canaries(data, canaries)
            require(default_data.is_dir(), "Package purge deleted default per-user data")
        finally:
            if changed_runner:
                subprocess.run(["sudo", "-n", "dpkg", "--purge", "pcl-rh"], timeout=120, check=False)
        return {"package": archive.name, "sha256": report["sha256"], "commit": report["commit"],
                "debian_version": report["debian_version"], "upgrade_from_synthetic_previous_revision": True,
                "obsolete_package_member_removed": True, "installed_resources_unchanged": True,
                "installation_check": check, "default_data_check": default_check,
                "reinstallation_check": reinstall_check, "cli_empty_root": True,
                "remove_preserved_user_data": True, "purge_preserved_user_data": True,
                "desktop_or_game_started": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--portable", type=Path, required=True)
    args = parser.parse_args()
    result = run(args.directory.resolve(), args.portable.resolve())
    (args.directory / "linux-package-verification.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
