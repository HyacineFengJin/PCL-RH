#!/usr/bin/env python3
"""Install/upgrade/uninstall the real candidate on a disposable Windows runner.

This intentionally changes the runner's per-user app registration/shortcut.
It refuses local invocation and an existing registration; never use it to
replace a person's installation. Only build-info and blocked helper modes run.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

SPEC = importlib.util.spec_from_file_location("pcl_install_build", Path(__file__).parents[1] / "installer-native.py")
assert SPEC is not None and SPEC.loader is not None
INSTALL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALL)
VERIFY = INSTALL.VERIFY
require = INSTALL.PACKAGE.require
REGISTRY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.pcl-rh.desktop_is1"


def registered() -> bool:
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, REGISTRY):
            return True
    except FileNotFoundError:
        return False


def setup(executable: Path, destination: Path, log: Path, *, success: bool = True) -> None:
    result = subprocess.run([str(executable), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                             "/NOCLOSEAPPLICATIONS", "/NORESTARTAPPLICATIONS", "/SP-",
                             "/DIR=" + str(destination), "/LOG=" + str(log)], timeout=180)
    require((result.returncode == 0) == success, "Unexpected installer result: " + str(result.returncode))


def hashes(root: Path, manifest: dict) -> None:
    for entry in manifest["files"]:
        with (root / entry["path"]).open("rb") as source:
            require(hashlib.file_digest(source, "sha256").hexdigest() == entry["sha256"],
                    "Installed file differs: " + entry["path"])


@contextmanager
def locked_file(path: Path):
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                  ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    handle = kernel.CreateFileW(str(path), 0x80000000, 1, None, 3, 0x80, None)
    require(handle != wintypes.HANDLE(-1).value, "Could not hold the synthetic file lock")
    try:
        yield
    finally:
        kernel.CloseHandle(handle)


def run(directory: Path, portable: Path) -> dict:
    require(os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true",
            "Installer verification is restricted to a disposable Windows Actions runner")
    require(not registered(), "An existing PCL RH installation must not be touched")
    programs = subprocess.check_output(["pwsh", "-NoProfile", "-NonInteractive", "-Command",
                                       "[Environment]::GetFolderPath('Programs')"], text=True).strip()
    shortcut = Path(programs) / "PCL RH.lnk"
    require(not shortcut.exists(), "An existing shortcut must not be touched")
    report = json.loads((directory / "installer-manifest.json").read_text())
    executable = directory / report["installer"]
    require(executable.name == report["installer"] and executable.suffix == ".exe", "Invalid installer name")
    with executable.open("rb") as source:
        require(hashlib.file_digest(source, "sha256").hexdigest() == report["sha256"], "Installer hash mismatch")
    manifest = json.loads((portable / "package-manifest.json").read_text())
    with tempfile.TemporaryDirectory(prefix="installed space & 项目-", dir=directory) as temporary:
        scratch = Path(temporary).resolve()
        root = scratch / "PCL RH"
        setup(executable, root, directory / "install.log")
        require(registered() and shortcut.is_file(), "Installer did not register the per-user app and shortcut")
        env = {**os.environ, "PCL_SETUP_ROOT": str(root), "PCL_SETUP_LINK": str(shortcut)}
        subprocess.run(["pwsh", "-NoProfile", "-NonInteractive", "-Command",
                        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:PCL_SETUP_LINK); "
                        "if ($s.TargetPath -ne (Join-Path $env:PCL_SETUP_ROOT 'pcl-desktop.exe') "
                        "-or $s.WorkingDirectory -ne $env:PCL_SETUP_ROOT) { exit 1 }"], env=env, check=True)
        hashes(root, manifest)
        build = VERIFY.smoke(root, scratch, manifest)
        VERIFY.verify_windows_entry(root, scratch, build)
        canaries = {".pcl-rust/synthetic-settings.json": b"private fixture settings\n",
                    "Minecraft/.minecraft/saves/synthetic/level.dat": b"private fixture world\n",
                    "custom-project/hand-written.java": b"private handwritten fixture\n",
                    "my-notes.txt": b"private user file\n"}
        for name, data in canaries.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

        # A denied-write/share-delete EXE simulates a running installation. Do
        # not launch it or close an application. The desktop-first recipe must
        # reject before replacing the adapter, rather than promising rollback.
        early = root / "code/agent-adapter.mjs"
        early.write_bytes(b"previous synthetic adapter\n")
        image = root / "pcl-desktop.exe"
        previous = image.read_bytes() + b"old fixture overlay\n"
        image.write_bytes(previous)
        with locked_file(image):
            setup(executable, root, directory / "locked-upgrade.log", success=False)
            require(image.read_bytes() == previous and early.read_bytes() == b"previous synthetic adapter\n",
                    "Locked desktop did not reject before changing program resources")
        setup(executable, root, directory / "upgrade.log")
        hashes(root, manifest)
        # A late resource lock is deliberately distinct: this is not an atomic
        # generation upgrade. Verify explicit failure and successful repair,
        # without ever starting the potentially mixed intermediate application.
        late = root / "experimental/host.mjs"
        late.write_bytes(b"previous synthetic host\n")
        with locked_file(late):
            setup(executable, root, directory / "late-resource-failure.log", success=False)
            for name, data in canaries.items():
                require((root / name).read_bytes() == data, "Failed upgrade changed user data")
        setup(executable, root, directory / "repaired-upgrade.log")
        hashes(root, manifest)
        for name, data in canaries.items():
            require((root / name).read_bytes() == data, "Upgrade changed user data")
        uninstaller = root / "unins000.exe"
        require(uninstaller.is_file(), "Missing actual uninstaller")
        subprocess.run([str(uninstaller), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                        "/LOG=" + str(directory / "uninstall.log")], check=True, timeout=180)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and (uninstaller.exists() or registered() or shortcut.exists()):
            time.sleep(0.1)
        require(not registered() and not shortcut.exists() and not uninstaller.exists(),
                "Uninstaller did not retire its registration and shortcut")
        for entry in manifest["files"]:
            require(not (root / entry["path"]).exists(), "Uninstall retained a registered program file")
        require(not (root / "package-manifest.json").exists(), "Uninstall retained its package manifest")
        for name, data in canaries.items():
            require((root / name).read_bytes() == data, "Uninstall changed user data")
    return {"installed": True, "shortcut_target_verified": True, "installed_build": build,
            "locked_exe_rejected_before_resources_changed": True, "upgrade_verified": True,
            "late_resource_failure_reported_and_repaired": True,
            "uninstalled": True, "user_data_preserved": True, "desktop_or_game_started": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--portable", type=Path, required=True)
    args = parser.parse_args()
    result = run(args.directory.resolve(), args.portable.resolve())
    (args.directory / "verification.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
