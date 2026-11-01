#!/usr/bin/env python3
"""Verify a standalone .app in private macOS CI storage without Finder or GUI.

The same relocated executable resolves its bundled resources and initializes
separate default/explicit data roots through --installation-check. Making every
bundle file and directory read-only proves that no portable-data copy is used.
LaunchServices also opens this exact copy with the headless diagnostic, without
requesting foreground activation. No DMG is mounted, no app is installed, and
no signing assessment is claimed.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import stat
import subprocess
import tempfile

SPEC = importlib.util.spec_from_file_location("pcl_app_verification", Path(__file__).resolve().parents[1] / "app-native.py")
assert SPEC is not None and SPEC.loader is not None
APP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(APP)
require = APP.require


def require_ci() -> None:
    require(platform.system() == "Darwin" and os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_OS") == "macOS"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted" and os.geteuid() != 0,
            "Application execution verification is restricted to disposable GitHub-hosted macOS CI")


def run(command: list[str], scratch: Path, environment: dict) -> str:
    result = subprocess.run(command, cwd=scratch, env=environment, capture_output=True,
                            text=True, encoding="utf-8", timeout=45)
    require(len(result.stdout) <= 1024 * 1024 and len(result.stderr) <= 1024 * 1024,
            "Application diagnostic exceeded its output budget")
    require(result.returncode == 0, "Application diagnostic failed: "
            + result.stderr[-4000:] + "\n" + result.stdout[-4000:])
    return result.stdout


def snapshot(bundle: Path) -> dict:
    records = {}
    for path in [bundle, *sorted(bundle.rglob("*"))]:
        metadata = path.lstat()
        require(stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode), "Application contains a link or special file")
        records[path.relative_to(bundle).as_posix()] = {
            "mode": stat.S_IMODE(metadata.st_mode),
            "sha256": APP.sha256(path) if path.is_file() else None}
    return records


def readonly(bundle: Path) -> None:
    for path in bundle.rglob("*"):
        if path.is_file():
            path.chmod(0o555 if path.stat().st_mode & 0o111 else 0o444)
    for path in sorted([bundle, *[path for path in bundle.rglob("*") if path.is_dir()]],
                       key=lambda path: len(path.parts), reverse=True):
        path.chmod(0o555)


def restore_permissions(bundle: Path, report: dict) -> None:
    # Restore only this owned extracted copy so TemporaryDirectory can remove
    # it, including on diagnostic failure. Mounted/user bundles are never passed.
    bundle.chmod(0o755)
    for path in bundle.rglob("*"):
        if stat.S_ISDIR(path.lstat().st_mode):
            path.chmod(0o755)
    for entry in report["files"]:
        path = bundle / entry["path"]
        if stat.S_ISREG(path.lstat().st_mode):
            path.chmod(entry["mode"])


def no_resource_copy(data: Path) -> None:
    require(data.is_dir() and data.stat().st_uid == os.geteuid(), "User data was not initialized by the runner user")
    for name in ("pcl-rh.json", "pcl-desktop", "pcl-cli", "assets", "code", "experimental/runtime/node_modules"):
        require(not os.path.lexists(data / name), "App copied production resources into user data: " + name)


def checked_installation_result(bundle: Path, data: Path, result: dict) -> dict:
    require(isinstance(result, dict), "Application installation diagnostic must be a JSON object")
    require(result.get("schema_version") == 1 and result.get("ok") is True
            and result.get("installed") is True and result.get("plugin_host_checked") is True
            and result.get("plugin_host_retired") is True
            and result.get("desktop_or_game_started") is False,
            "App did not check and retire its actual installed plugin host: " + str(result.get("error", ""))[-4000:])
    resource = (bundle / APP.RESOURCE).resolve()
    reported_data = Path(result.get("data_root", ""))
    game = Path(result.get("game_root", ""))
    require(Path(result.get("resource_root", "")).resolve() == resource
            and reported_data.is_absolute() and reported_data.resolve() == data.resolve()
            and not data.resolve().is_relative_to(bundle.resolve())
            and game.is_absolute() and game.resolve().is_relative_to(data.resolve()),
            "App resource/data/game roots are not separated")
    no_resource_copy(data)
    return result


def installed(bundle: Path, scratch: Path, environment: dict, data: Path) -> dict:
    binary = bundle / "Contents/MacOS/pcl-desktop"
    result = json.loads(run([str(binary), "--installation-check"], scratch, environment))
    return checked_installation_result(bundle, data, result)


def launch_services(bundle: Path, scratch: Path, environment: dict, data: Path, expected: dict) -> dict:
    output, errors = scratch / "launch-services-output.json", scratch / "launch-services-stderr.txt"
    for path in (output, errors):
        with path.open("xb"):
            pass
        path.chmod(0o600)
    # Apple's open(1) explicitly supports per-launch environment/stdout/stderr.
    # -n binds the wait to a new instance; -g/-j avoid foreground activation.
    # Full bundle path plus the returned resource root prevent a cached bundle
    # identifier from silently selecting a different installed/candidate app.
    command = ["/usr/bin/open", "-n", "-W", "-g", "-j", "--stdout", str(output),
               "--stderr", str(errors), "--stdin", "/dev/null", "-a", str(bundle)]
    isolated = {**environment}
    isolated.pop("PCL_RH_RESOURCE_ROOT", None)
    isolated["PCL_RH_DATA_ROOT"] = str(data)
    for key in ("HOME", "PCL_RH_DATA_ROOT", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME"):
        if key in isolated:
            command.extend(["--env", key + "=" + isolated[key]])
    command.extend(["--args", "--installation-check"])
    result = subprocess.run(command, cwd=scratch, env=isolated, capture_output=True,
                            text=True, encoding="utf-8", timeout=60)
    require(len(result.stdout) <= 64 * 1024 and len(result.stderr) <= 64 * 1024
            and output.stat().st_size <= 1024 * 1024 and errors.stat().st_size <= 1024 * 1024,
            "LaunchServices diagnostic exceeded its output budget")
    raw, stderr = output.read_text(encoding="utf-8"), errors.read_text(encoding="utf-8")
    require(result.returncode == 0, "LaunchServices could not open the owned app: "
            + (result.stderr[-4000:] or stderr[-4000:] or raw[-4000:]))
    require(bool(raw.strip()), "LaunchServices returned without an actual application diagnostic: " + stderr[-4000:])
    check = checked_installation_result(bundle, data, json.loads(raw))
    require(check == expected, "LaunchServices diagnostic differs from the direct installed entry")
    return check


def verify_execution(bundle: Path, scratch: Path, report: dict, manifest: dict) -> dict:
    require_ci()
    require(bundle.resolve().is_relative_to(scratch.resolve()), "Execute only an owned temporary app copy")
    for name in sorted(APP.BINARIES):
        APP.VERIFY.native_binary(bundle / "Contents/MacOS" / name)
    actual_deployments = {name: APP.native_deployment(bundle / "Contents/MacOS" / name)
                          for name in sorted(APP.BINARIES)}
    require(actual_deployments == report["binary_deployment_targets"]
            and APP.os_version(report["minimum_macos_version"])
            >= max([APP.os_version("11.0"), *[APP.os_version(value) for value in actual_deployments.values()]]),
            "Info.plist understates the actual binary deployment target")
    home = scratch / "isolated home 项目 & space"
    home.mkdir(mode=0o700)
    environment = {**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(scratch / "xdg config"),
                   "XDG_CACHE_HOME": str(scratch / "xdg cache"), "XDG_DATA_HOME": str(scratch / "xdg data")}
    environment.pop("PCL_RH_RESOURCE_ROOT", None)
    environment.pop("PCL_RH_DATA_ROOT", None)
    default_data = home / "Library/Application Support/PCL RH"
    explicit_data = scratch / "separate data 项目 & space"
    readonly(bundle)
    before = snapshot(bundle)
    try:
        binary = bundle / "Contents/MacOS/pcl-desktop"
        raw = run([str(binary), "--build-info"], scratch, environment)
        info = json.loads(raw)
        require(info.get("schema_version") == 2 and info.get("runtime_layout") == "split-v1",
                "App executable has no installed-layout capability")
        build = APP.VERIFY.verify_desktop_build(raw, manifest)
        require(build == report["desktop_build"] and not default_data.exists(),
                "App build identity changed or initialized user data")
        default_check = installed(bundle, scratch, environment, default_data)
        explicit_environment = {**environment, "PCL_RH_DATA_ROOT": str(explicit_data)}
        explicit_check = installed(bundle, scratch, explicit_environment, explicit_data)
        canaries = {"my-notes.txt": b"private fixture data\n",
                    ".pcl-rust/synthetic-settings.json": b"private fixture settings\n",
                    "Minecraft/.minecraft/saves/synthetic/level.dat": b"private fixture world\n"}
        for name, contents in canaries.items():
            path = explicit_data / name
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            path.write_bytes(contents)
        repeated = installed(bundle, scratch, explicit_environment, explicit_data)
        launch_check = launch_services(bundle, scratch, explicit_environment, explicit_data, repeated)
        require(all((explicit_data / name).read_bytes() == contents for name, contents in canaries.items()),
                "App reinitialization changed existing user data")
        cli = str(bundle / "Contents/MacOS/pcl-cli")
        APP.VERIFY.verify_cli_version(run([cli, "--version"], scratch, explicit_environment), manifest["cli_version"])
        game = scratch / "empty synthetic game"
        game.mkdir()
        listed = run([cli, "--project", str(explicit_data), "--root", str(game), "list"], scratch, explicit_environment)
        require(json.loads(listed) == [], "Bundled CLI could not inspect a separate empty game root")
        require(snapshot(bundle) == before, "Installed application diagnostics changed the read-only bundle")
        return {"desktop_build": build, "default_data_check": default_check, "explicit_data_check": explicit_check,
                "repeated_data_check": repeated, "binary_deployment_targets": actual_deployments,
                "launch_services_entry_checked": True, "launch_services_check": launch_check,
                "read_only_bundle_unchanged": True, "production_resources_copied_to_data": False,
                "existing_user_data_preserved": True, "cli_empty_root": True,
                "finder_or_gui_started": False, "gatekeeper_assessed": False,
                "desktop_or_game_started": False}
    finally:
        restore_permissions(bundle, report)


def verify(directory: Path, portable: Path) -> dict:
    require_ci()
    report = json.loads((directory / "app-manifest.json").read_bytes())
    name = report["archive"]
    require(name == "mac-app.tar.gz", "Unknown application archive name")
    archive = directory / name
    require(APP.sha256(archive) == report["sha256"]
            and archive.with_name(name + ".sha256").read_text().split() == [report["sha256"], name],
            "Application archive checksum mismatch")
    with tempfile.TemporaryDirectory(prefix="app space & 项目-", dir=directory) as temporary:
        scratch = Path(temporary).resolve()
        original = scratch / "original portable"
        original.mkdir()
        root, source, source_hash = APP.DMG.verified_portable(portable, original)
        APP.validated_source(root, source)
        require(source_hash == report["source_archive_sha256"], "App was built from a different portable archive")
        copied = scratch / "relocated copy"
        copied.mkdir()
        bundle = APP.unpack(archive, copied)
        manifest = APP.verify_bundle(bundle, report, root / "package-manifest.json")
        result = verify_execution(bundle, scratch, report, manifest)
    result.update({"archive": name, "sha256": report["sha256"], "source_commit": report["commit"],
                   "mapped_source_hashes_verified": True, "signed": False, "notarized": False})
    (directory / "app-verification.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--portable", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(verify(args.directory.resolve(), args.portable.resolve())))
