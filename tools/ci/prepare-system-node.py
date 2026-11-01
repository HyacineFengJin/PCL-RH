#!/usr/bin/env python3
"""Copy setup-node's checked executable into the product's fixed CI prefix.

This is restricted to disposable GitHub-hosted Unix runners. Product discovery
does not accept toolcache/PATH overrides, so package verification uses the same
system Node location as an installed launcher. No npm tree or service is moved.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import uuid

MAX_NODE_BYTES = 512 * 1024**2
COMMAND_ENV = {"PATH": "/usr/bin:/bin", "LANG": "C"}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def destination() -> Path:
    """This guard must precede every executable, including Node --version."""
    system, machine = platform.system(), platform.machine().lower()
    require(os.name == "posix" and system in ("Linux", "Darwin")
            and os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and os.environ.get("RUNNER_OS") == {"Linux": "Linux", "Darwin": "macOS"}.get(system)
            and os.getuid() == os.geteuid() != 0 and os.getgid() == os.getegid(),
            "System Node preparation requires an unprivileged disposable GitHub-hosted Unix runner")
    require(machine in ("x86_64", "amd64", "aarch64", "arm64"), "Unsupported native runner architecture")
    if system == "Linux":
        return Path("/usr/bin/node")
    return Path("/opt/homebrew/bin/node" if machine in ("aarch64", "arm64") else "/usr/local/bin/node")


def regular(metadata: os.stat_result) -> bool:
    return stat.S_ISREG(metadata.st_mode) and not getattr(metadata, "st_file_attributes", 0) & 0x400


def identity(metadata: os.stat_result) -> tuple:
    return (metadata.st_dev, metadata.st_ino)


def stamp(metadata: os.stat_result) -> tuple:
    return (metadata.st_dev, metadata.st_ino, metadata.st_mode, metadata.st_uid, metadata.st_gid,
            metadata.st_nlink, metadata.st_size, metadata.st_mtime_ns, metadata.st_ctime_ns,
            getattr(metadata, "st_file_attributes", 0))


def snapshot(path: Path) -> tuple[tuple, str]:
    require(path.is_absolute() and path.resolve(strict=True) == path,
            "Node executable path contains a changed link")
    before = path.lstat()
    require(regular(before) and 0 < before.st_size <= MAX_NODE_BYTES and before.st_mode & 0o111,
            "Node executable must be a bounded executable regular file, without a link or reparse point")
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    with os.fdopen(os.open(path, flags), "rb") as incoming:
        held = os.fstat(incoming.fileno())
        require(stamp(held) == stamp(before), "Node executable changed before hashing")
        digest, size = hashlib.sha256(), 0
        while chunk := incoming.read(128 * 1024):
            size += len(chunk)
            require(size <= MAX_NODE_BYTES, "Node executable exceeds the hash budget")
            digest.update(chunk)
        require(size == before.st_size and stamp(os.fstat(incoming.fileno())) == stamp(before),
                "Node executable changed while hashing")
    require(path.resolve(strict=True) == path and stamp(path.lstat()) == stamp(before),
            "Node executable identity changed while hashing")
    return stamp(before), digest.hexdigest()


def selected_source() -> tuple[Path, Path, tuple]:
    raw_cache = os.environ.get("RUNNER_TOOL_CACHE", "")
    cache = Path(raw_cache)
    require(bool(raw_cache) and cache.is_absolute() and cache.is_dir(),
            "RUNNER_TOOL_CACHE must be an existing absolute directory")
    cache = cache.resolve(strict=True)
    cache_id = identity(cache.lstat())
    chosen = shutil.which("node")
    require(chosen is not None and Path(chosen).is_absolute(), "setup-node did not provide an absolute Node path")
    source = Path(chosen).resolve(strict=True)
    require(source != cache and source.is_relative_to(cache), "Selected Node is outside RUNNER_TOOL_CACHE")
    require(regular(source.lstat()), "Selected Node is not a regular non-reparse toolcache file")
    return source, cache, cache_id


def checked_version(path: Path) -> str:
    result = subprocess.run([str(path), "--version"], capture_output=True, timeout=15,
                            env=COMMAND_ENV, shell=False, check=True)
    require(len(result.stdout) <= 128 and len(result.stderr) <= 4096 and not result.stderr,
            "Node version probe exceeded its output budget or wrote diagnostics")
    match = re.fullmatch(rb"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\n?", result.stdout)
    require(match is not None, "Node returned an invalid stable version")
    version = tuple(int(part) for part in match.groups())
    require(version >= (22, 19, 0), "System Node preparation requires Node >=22.19")
    return ".".join(str(part) for part in version)


def recheck_source(source: Path, cache: Path, cache_id: tuple, expected: tuple) -> None:
    require(cache.resolve(strict=True) == cache and identity(cache.lstat()) == cache_id
            and source.is_relative_to(cache), "Node toolcache root changed")
    require(snapshot(source) == expected, "Node toolcache source hash or identity changed")


def sudo(arguments: list[str]) -> None:
    subprocess.run(["/usr/bin/sudo", "-n", *arguments], check=True, timeout=45,
                   env=COMMAND_ENV, shell=False)


def parent_identity(parent: Path) -> tuple:
    require(parent.is_absolute() and parent.resolve(strict=True) == parent,
            "System Node prefix contains a link or reparse point")
    metadata = parent.lstat()
    require(stat.S_ISDIR(metadata.st_mode) and not getattr(metadata, "st_file_attributes", 0) & 0x400,
            "System Node prefix is not a literal directory")
    return identity(metadata)


def prepare_parent(parent: Path) -> tuple:
    # All operands originate from destination(), never user-supplied paths.
    for ancestor in reversed([parent, *parent.parents]):
        if os.path.lexists(ancestor):
            parent_identity(ancestor)
    if not os.path.lexists(parent):
        sudo(["/bin/mkdir", "-p", str(parent)])
    return parent_identity(parent)


def check_destination(path: Path) -> None:
    if os.path.lexists(path):
        metadata = path.lstat()
        # mv replaces an old file symlink itself; install never opens it.
        require(regular(metadata) or stat.S_ISLNK(metadata.st_mode),
                "Existing system Node destination is a directory or special file")


def prepare() -> dict:
    target = destination()
    source, cache, cache_id = selected_source()
    expected = snapshot(source)
    version = checked_version(source)
    recheck_source(source, cache, cache_id, expected)
    parent_id = prepare_parent(target.parent)
    check_destination(target)
    temporary = target.with_name(".pcl-rh-node-" + uuid.uuid4().hex + ".tmp")
    require(not os.path.lexists(temporary), "Refusing to replace an existing Node staging file")
    owned_temporary = None
    moved = False
    try:
        sudo(["/usr/bin/install", "-m", "0755", str(source), str(temporary)])
        entry = temporary.lstat()
        require(regular(entry), "Installed Node staging entry is not a regular file")
        owned_temporary = identity(entry)
        staged = snapshot(temporary)
        require(staged[1] == expected[1] and stat.S_IMODE(entry.st_mode) == 0o755,
                "Installed Node staging bytes or permissions differ from setup-node")
        require(checked_version(temporary) == version, "Staged Node version differs from setup-node")
        require(snapshot(temporary) == staged, "Staged Node changed after its version probe")
        recheck_source(source, cache, cache_id, expected)
        require(parent_identity(target.parent) == parent_id, "System Node prefix changed")
        check_destination(target)
        # GNU -T and BSD -h both prevent a destination symlink-to-directory
        # from redirecting this rename into another directory.
        option = "-T" if platform.system() == "Linux" else "-h"
        sudo(["/bin/mv", "-f", option, str(temporary), str(target)])
        moved = True
        installed = snapshot(target)
        require(installed[1] == expected[1] and stat.S_IMODE(target.lstat().st_mode) == 0o755,
                "System Node hash or permissions differ from setup-node")
        require(checked_version(target) == version, "System Node version differs from setup-node")
        require(snapshot(target) == installed and parent_identity(target.parent) == parent_id,
                "System Node changed after installation")
        recheck_source(source, cache, cache_id, expected)
        return {"schema_version": 1, "source": str(source), "destination": str(target),
                "version": version, "sha256": expected[1], "npm_copied": False,
                "disposable_ci_only": True}
    finally:
        # Only the exact regular temporary inode created above is cleanup
        # authority. A replaced/uncertain entry is retained, never unlinked.
        if not moved and owned_temporary is not None and os.path.lexists(temporary):
            entry = temporary.lstat()
            if regular(entry) and identity(entry) == owned_temporary:
                sudo(["/bin/rm", "-f", str(temporary)])


def main() -> int:
    try:
        print(json.dumps(prepare(), sort_keys=True))
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
