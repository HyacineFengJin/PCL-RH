#!/usr/bin/env python3
"""Build a portable desktop checkout with the current host's Rust toolchain."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent


def npm_command() -> list[str]:
    if os.name != "nt":
        npm = shutil.which("npm")
        if npm is None:
            raise RuntimeError("Node.js 22.19+ and npm are required")
        return [npm]
    # Invoke npm's JavaScript entry directly: no cmd.exe/batch quoting of a
    # checkout path is needed, including paths containing spaces or '&'.
    node = shutil.which("node.exe")
    npm = shutil.which("npm.cmd")
    if node is None:
        raise RuntimeError("Install Node.js 22.19+ with npm first")
    directories = [Path(node).resolve().parent]
    if npm:
        directories.append(Path(npm).resolve().parent)
    for directory in directories:
        entry = directory / "node_modules" / "npm" / "bin" / "npm-cli.js"
        if entry.is_file():
            return [node, str(entry)]
    raise RuntimeError("npm-cli.js was not found beside Node/npm; use a standard Node.js installation")


def install_binary(
    source: Path, project_root: Path = ROOT, *, windows: bool | None = None
) -> Path:
    windows = os.name == "nt" if windows is None else windows
    if windows:
        # The runtime owns its private Windows state ACL. Python's 0700 uses
        # OWNER RIGHTS, which is not the retained filesystem's exact SID policy.
        # Keep executables beside the archive marker; never create/repair state.
        destination = project_root / source.name
    else:
        # Retain the existing Unix portable layout and permissions unchanged.
        state = project_root / ".pcl-rust"
        state.mkdir(mode=0o700, exist_ok=True)
        destination = state / "bin" / source.name
        destination.parent.mkdir(mode=0o700, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix=".pcl-build-", suffix=source.suffix, dir=destination.parent)
    os.close(fd)
    temporary = Path(name)
    try:
        shutil.copyfile(source, temporary)
        if not windows:
            temporary.chmod(0o755)
        # A running Windows executable may refuse replacement. Keep the old
        # binary intact and report the error; never stop a user's application.
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
    return destination


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=os.environ.get("PCL_BUILD_JOBS", "2"))
    parser.add_argument("--release", action="store_true", help="build an optimized executable")
    parser.add_argument("--build-only", action="store_true", help="keep outputs in target; do not replace the local launcher")
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    cargo = shutil.which("cargo")
    if cargo is None:
        raise RuntimeError("Rust/Cargo and the native C/C++ build tools are required")
    npm = npm_command()
    environment = os.environ.copy()
    credential = ROOT / ".pcl-rust" / "maintainer" / "curseforge-api-key"
    if not any(key in environment for key in ("PCL_RH_CURSEFORGE_API_KEY", "PCL_RH_CURSEFORGE_API_KEY_FILE")) and credential.is_file():
        environment["PCL_RH_CURSEFORGE_API_KEY_FILE"] = str(credential)

    def run(command: list[str]) -> None:
        subprocess.run(command, cwd=ROOT, env=environment, check=True)

    for prefix in ("apps/desktop", "experimental/runtime"):
        run(npm + ["ci", "--prefix", prefix, "--ignore-scripts", "--no-audit", "--no-fund"])
    run(npm + ["run", "build", "--prefix", "apps/desktop"])
    command = [cargo, "build", "--locked", "-p", "pcl-desktop", "-p", "pcl-cli", "--features", "pcl-desktop/custom-protocol", "-j", str(args.jobs)]
    if args.release:
        command.append("--release")
    run(command)
    if not args.build_only:
        suffix = ".exe" if os.name == "nt" else ""
        profile = "release" if args.release else "debug"
        target = Path(environment.get("CARGO_TARGET_DIR", str(ROOT / "target")))
        if not target.is_absolute():
            target = ROOT / target
        for binary in ("pcl-desktop", "pcl-cli"):
            install_binary(target / profile / (binary + suffix))
    print("Build complete. Run start-native.ps1 on Windows or start-native.sh on Linux/macOS.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Build failed: {error}", file=sys.stderr)
        sys.exit(1)
