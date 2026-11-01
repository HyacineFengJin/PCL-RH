#!/usr/bin/env python3
"""Put a verified native candidate in its actual installed layout for client QA.

Only disposable GitHub-hosted runners may use this tool. It prepares a Debian
installation, a read-only Mac app, or a per-user Windows setup installation;
game tests therefore exercise the shipped executable's real location too.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def guard(root: Path) -> None:
    if (os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or not root.resolve().is_relative_to(Path(os.environ["RUNNER_TEMP"]).resolve())
            or root.resolve() == Path(os.environ["RUNNER_TEMP"]).resolve()):
        raise ValueError("Prepare game installations only in a disposable hosted runner's private temporary child")


def prepare(portable: Path, root: Path, report: Path) -> dict:
    guard(root)
    root.mkdir(mode=0o700)
    packages = root / "packages"
    system = platform.system()
    if system == "Linux":
        package = load("pcl_game_deb", ROOT / "linux-package-native.py")
        verification = load("pcl_game_deb_check", ROOT / "ci/verify-linux-package.py")
        verification.disposable_runner(packages)
        metadata = package.build(portable, packages)
        # The exact package was just built from the checked source. Inspect the
        # real Debian data/control streams before any privileged installation.
        verification.checked_archive(packages, metadata)
        subprocess.run(["sudo", "-n", "dpkg", "--install", str(packages / metadata["package"])],
                       check=True, timeout=180)
        executable = Path("/usr/lib/pcl-rh/pcl-desktop")
        kind = "debian"
    elif system == "Darwin":
        package = load("pcl_game_app", ROOT / "app-native.py")
        verification = load("pcl_game_app_check", ROOT / "ci/verify-native-app.py")
        metadata = package.build(portable, packages)
        relocated = root / "installed app"
        relocated.mkdir()
        bundle = package.unpack(packages / metadata["archive"], relocated)
        package.verify_bundle(bundle, metadata)
        verification.readonly(bundle)
        executable = bundle / "Contents/MacOS/pcl-desktop"
        kind = "macos-app"
    elif system == "Windows":
        package = load("pcl_game_setup", ROOT / "installer-native.py")
        verification = load("pcl_game_setup_check", ROOT / "ci/verify-native-installer.py")
        if verification.registered():
            raise ValueError("An existing Windows registration must not be changed")
        metadata = package.build(portable, packages)
        install_root = root / "installed space & 项目" / "PCL RH"
        subprocess.run([str(packages / metadata["installer"]), "/VERYSILENT", "/SUPPRESSMSGBOXES",
                        "/NORESTART", "/SP-", "/DIR=" + str(install_root),
                        "/LOG=" + str(root / "install.log")], check=True, timeout=180)
        executable = install_root / "pcl-desktop.exe"
        kind = "windows-setup"
    else:
        raise ValueError("Unsupported native game installation host")
    source = json.loads((portable / "package-manifest.json").read_bytes())
    verify = load("pcl_game_package_identity", ROOT / "ci/verify-native-package.py")
    raw = verify.run([str(executable), "--build-info"], root)
    info = verify.verify_desktop_build(raw, source)
    if info.get("schema_version") != 2 or info.get("runtime_layout") != "split-v1":
        raise ValueError("This source predates installed client acceptance")
    result = {"schema_version": 1, "kind": kind, "executable": str(executable),
              "root": str(root), "source_commit": source["commit"], "desktop_build": info,
              "package_version": metadata.get("debian_version")}
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as environment:
        environment.write("PCL_ACCEPTANCE_DESKTOP=" + str(executable) + "\n")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--portable", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(prepare(args.portable.resolve(), args.root.resolve(), args.report.resolve())))
