#!/usr/bin/env python3
"""Observe one owned native .app window and request its orderly exit in CI.

This records native visibility and process exit only. It does not capture images,
change focus, inspect the clipboard, or prove React, game, AI or Gatekeeper use.
"""
from __future__ import annotations

import argparse
import ctypes as c
import importlib.util
import json
import os
from pathlib import Path
import platform
import stat
import subprocess
import sys
import time

SPEC = importlib.util.spec_from_file_location("pcl_macos_window_observer", Path(__file__).parent / "game-acceptance/native_window.py")
WINDOW = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WINDOW)
LOG_LIMIT = 8 * 1024**2


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def require_ci() -> None:
    require(os.name == "posix" and sys.platform == "darwin" and platform.system() == "Darwin"
            and os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and os.environ.get("RUNNER_OS") == "macOS"
            and os.getuid() == os.geteuid() != 0 and os.getgid() == os.getegid(),
            "Native window verification requires an unprivileged disposable GitHub-hosted macOS runner")


def owned_directory(path: Path, *, private: bool = False) -> None:
    metadata = path.lstat()
    require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == os.geteuid(),
            "Expected a literal runner-owned directory")
    require(not private or stat.S_IMODE(metadata.st_mode) & 0o077 == 0,
            "GUI evidence and data directories must be private")


def checked_paths(bundle: Path, data: Path, output: Path) -> tuple[Path, Path, Path]:
    raw_temp = os.environ.get("RUNNER_TEMP", "")
    temporary = Path(raw_temp)
    require(bool(raw_temp) and temporary.is_absolute(), "RUNNER_TEMP must be an existing absolute directory")
    temporary = temporary.resolve(strict=True)
    owned_directory(temporary)
    require(all(path.is_absolute() and ".." not in path.parts for path in (bundle, data, output)),
            "Bundle, new data root and evidence output must be explicit absolute paths")
    owned_directory(bundle)
    bundle = bundle.resolve(strict=True)
    require(bundle.suffix == ".app" and bundle != temporary and bundle.is_relative_to(temporary),
            "Execute only a runner-owned temporary .app copy inside RUNNER_TEMP")
    for directory in (bundle / "Contents", bundle / "Contents/MacOS"):
        owned_directory(directory)
        require(directory.resolve(strict=True) == directory, "Application directory links are not allowed")
    binary = bundle / "Contents/MacOS/pcl-desktop"
    metadata = binary.lstat()
    require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.geteuid()
            and metadata.st_nlink == 1 and metadata.st_mode & 0o111
            and binary.resolve(strict=True) == binary,
            "Application executable must be its literal runner-owned executable file")
    require(not os.path.lexists(data), "GUI smoke requires a new data root without existing files or links")
    data = data.parent.resolve(strict=True) / data.name
    output = output.parent.resolve(strict=True) / output.name
    for path in (data, output):
        require(path != temporary and path.is_relative_to(temporary), "Data and evidence must be children of RUNNER_TEMP")
        owned_directory(path.parent)
    require(all(not left.is_relative_to(right) and not right.is_relative_to(left)
                for left, right in ((bundle, data), (bundle, output), (data, output))),
            "Bundle, data and evidence roots must be separate")
    if os.path.lexists(output):
        owned_directory(output, private=True)
    return bundle, data, output


def private_file(path: Path):
    return os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb")


def environment_for(data: Path, output: Path) -> dict:
    data.mkdir(mode=0o700)
    (data / "Minecraft").mkdir(mode=0o700)
    (data / "Minecraft/.minecraft").mkdir(mode=0o700)
    app = data / ".pcl-rust"
    code = app / "rh-code"
    app.mkdir(mode=0o700)
    code.mkdir(mode=0o700)
    # The product's own plugin configuration disables clipboard discovery before
    # first startup. No existing profile, account or clipboard value is opened.
    with private_file(code / "workspace.json") as target:
        target.write(json.dumps({"schemaVersion": 1, "projects": [],
                                 "disabledPlugins": ["launcher.discovery.clipboard"],
                                 "layoutPresets": []}).encode("utf-8"))
    home = output / "isolated-home"
    home.mkdir(mode=0o700)
    directories = {"HOME": home, "CFFIXED_USER_HOME": home, "TMPDIR": output / "tmp",
                   "XDG_CONFIG_HOME": home / "config", "XDG_CACHE_HOME": home / "cache",
                   "XDG_DATA_HOME": home / "data", "XDG_STATE_HOME": home / "state"}
    for directory in set(directories.values()) - {home}:
        directory.mkdir(mode=0o700)
    environment = os.environ.copy()
    for key in ("PCL_RH_HOME", "PCL_LINUX_HOME", "PCL_RH_RESOURCE_ROOT", "PCL_RH_DATA_ROOT"):
        environment.pop(key, None)
    environment.update({key: str(value) for key, value in directories.items()})
    environment["PCL_RH_DATA_ROOT"] = str(data)
    return environment


class MacApplication:
    """Typed Objective-C messages for one unreaped Popen child's application."""
    def __init__(self) -> None:
        self.appkit = c.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
        self.objc = c.CDLL("/usr/lib/libobjc.A.dylib")
        ptr = c.c_void_p
        self.objc.objc_getClass.argtypes, self.objc.objc_getClass.restype = [c.c_char_p], ptr
        self.objc.sel_registerName.argtypes, self.objc.sel_registerName.restype = [c.c_char_p], ptr
        # objc_msgSend has several distinct ABIs. Never overwrite one global
        # argtypes declaration and then reuse it for PID, object and BOOL calls.
        self.object_message = c.CFUNCTYPE(ptr, ptr, ptr)(("objc_msgSend", self.objc))
        self.application_for_pid = c.CFUNCTYPE(ptr, ptr, ptr, c.c_int)(("objc_msgSend", self.objc))
        self.pid_message = c.CFUNCTYPE(c.c_int, ptr, ptr)(("objc_msgSend", self.objc))
        objc_bool = c.c_bool if platform.machine().lower() in ("arm64", "aarch64") else c.c_byte
        self.bool_message = c.CFUNCTYPE(objc_bool, ptr, ptr)(("objc_msgSend", self.objc))
        self.void_message = c.CFUNCTYPE(None, ptr, ptr)(("objc_msgSend", self.objc))
        self.application = self.objc.objc_getClass(b"NSRunningApplication")
        self.pool_class = self.objc.objc_getClass(b"NSAutoreleasePool")
        require(bool(self.application) and bool(self.pool_class), "AppKit application classes are unavailable")
        self.selectors = {name: self.objc.sel_registerName(name.encode("ascii")) for name in (
            "alloc", "init", "drain", "runningApplicationWithProcessIdentifier:", "processIdentifier", "terminate")}
        require(all(self.selectors.values()), "AppKit selectors are unavailable")

    def terminate(self, process: subprocess.Popen) -> bool:
        require(type(process.pid) is int and 0 < process.pid < 2**31, "Expected the actual owned child PID")
        require(process.poll() is None, "Owned desktop exited before the normal termination request")
        pool = self.object_message(self.pool_class, self.selectors["alloc"])
        pool = self.object_message(pool, self.selectors["init"])
        require(bool(pool), "Cannot create an Objective-C autorelease pool")
        try:
            application = self.application_for_pid(
                self.application, self.selectors["runningApplicationWithProcessIdentifier:"], process.pid)
            require(bool(application) and self.pid_message(application, self.selectors["processIdentifier"]) == process.pid,
                    "NSRunningApplication no longer identifies the owned desktop child")
            # A dead unreaped child keeps its PID reserved. poll() refuses the
            # request if it reaps that child; no PID/bundle-ID search is used.
            require(process.poll() is None, "Owned desktop exited before its AppKit termination request")
            return bool(self.bool_message(application, self.selectors["terminate"]))
        finally:
            self.void_message(pool, self.selectors["drain"])


def cleanup(process: subprocess.Popen, api: MacApplication, deadline: float) -> dict:
    result = {"normal_termination_requested": False, "forced": False, "kill_sent": False,
              "exit_code": process.poll()}
    if result["exit_code"] is not None:
        return result
    try:
        result["normal_termination_requested"] = api.terminate(process)
        require(result["normal_termination_requested"], "NSRunningApplication declined normal termination")
        process.wait(timeout=max(0.0, min(10.0, deadline - time.monotonic() - 3.0)))
    except Exception as error:
        result["normal_termination_error"] = str(error)[:1024]
    if process.poll() is None:
        # Failure cleanup addresses only this Popen child, never a bundle ID,
        # process-name match, user application or unrelated descendant.
        result["forced"] = True
        try:
            process.terminate()
        except OSError as error:
            result["termination_error"] = str(error)[:1024]
        try:
            process.wait(timeout=max(0.0, min(1.0, deadline - time.monotonic() - 1.0)))
        except (OSError, subprocess.TimeoutExpired):
            if process.poll() is None:
                try:
                    process.kill()
                    result["kill_sent"] = True
                except OSError as error:
                    result["kill_error"] = str(error)[:1024]
                try:
                    process.wait(timeout=max(0.0, deadline - time.monotonic()))
                except (OSError, subprocess.TimeoutExpired):
                    result["cleanup_error"] = "Owned child exit was not confirmed before the total deadline"
    result["exit_code"] = process.poll()
    return result


def log_bound(stdout: Path, stderr: Path) -> None:
    require(stdout.stat().st_size <= LOG_LIMIT and stderr.stat().st_size <= LOG_LIMIT,
            "Desktop log exceeded the GUI smoke bound")


def smoke(bundle: Path, data: Path, output: Path, report: dict) -> None:
    require_ci()
    started = time.monotonic()
    deadline = started + 45.0
    bundle, data, output = checked_paths(bundle, data, output)
    output.mkdir(mode=0o700, exist_ok=True)
    owned_directory(output, private=True)
    report.update({"bundle": str(bundle), "data_root": str(data), "output": str(output)})
    environment = environment_for(data, output)
    cwd = output / "cwd"
    cwd.mkdir(mode=0o700)
    stdout, stderr = output / "desktop-stdout.log", output / "desktop-stderr.log"
    executable = bundle / "Contents/MacOS/pcl-desktop"
    report.update({"executable": str(executable), "stdout_log": str(stdout), "stderr_log": str(stderr),
                   "clipboard_discovery_disabled": True})
    api, process, failure = MacApplication(), None, None
    try:
        with private_file(stdout) as stdout_file, private_file(stderr) as stderr_file:
            process = subprocess.Popen([str(executable)], cwd=cwd, env=environment,
                                       stdin=subprocess.DEVNULL, stdout=stdout_file, stderr=stderr_file,
                                       close_fds=True, shell=False)
            report["pid"] = process.pid
            stable, stable_since = None, None
            while time.monotonic() < started + 30.0:
                require(process.poll() is None, "Desktop exited before owned native visibility was confirmed")
                log_bound(stdout, stderr)
                observation = WINDOW.observe(process.pid)
                windows = observation.get("windows", [])
                require(isinstance(windows, list) and len(windows) <= 32, "Owned window inventory exceeded its bound")
                report["observer"] = observation.get("method")
                if observation.get("error"):
                    report["observation_error"] = str(observation["error"])[:512]
                # A second owner filter keeps even malformed observer output
                # from authorizing visibility or a close target.
                owned = [window for window in windows if window.get("owner_pid") == process.pid]
                report["observed_owned_windows"] = owned
                candidates = [window for window in owned if window.get("visible") is True
                              and window.get("title") == "PCL RH"
                              and type(window.get("width")) is int and window["width"] >= 810
                              and type(window.get("height")) is int and window["height"] >= 500
                              and isinstance(window.get("id"), str) and window["id"].isdigit()
                              and 0 < int(window["id"]) < 2**32]
                candidate = next((window for window in candidates if stable and window["id"] == stable[0]),
                                 candidates[0] if candidates else None)
                if candidate is None:
                    stable, stable_since = None, None
                else:
                    key = (candidate["id"], candidate["width"], candidate["height"])
                    now = time.monotonic()
                    if key != stable:
                        stable, stable_since = key, now
                    elif now - stable_since >= 2.0:
                        report.update({"window": candidate, "stable_seconds": round(now - stable_since, 3),
                                       "native_window_visible": True})
                        break
                time.sleep(0.1)
            else:
                raise RuntimeError("No owned PCL RH window remained visible at >=810x500 for two seconds within 30 seconds")
    except Exception as error:
        failure = error
    finally:
        if process is not None:
            try:
                report["cleanup"] = cleanup(process, api, deadline)
            except Exception as error:
                report["cleanup"] = {"cleanup_error": str(error), "exit_code": process.poll()}
                failure = failure or error
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
    if failure is not None:
        raise failure
    require(report["cleanup"]["normal_termination_requested"] and not report["cleanup"]["forced"]
            and report["cleanup"]["exit_code"] == 0,
            "Desktop did not exit successfully after its owned NSRunningApplication termination request")
    log_bound(stdout, stderr)
    require(time.monotonic() <= deadline, "GUI smoke exceeded its 45-second total deadline")


def new_report() -> dict:
    return {"schema_version": 1, "ok": False, "native_window_visible": False,
            "proves": "Owned native window visibility and orderly NSRunningApplication termination only",
            "does_not_prove": ["React rendering", "account functionality", "game launch", "AI functionality",
                               "Gatekeeper acceptance", "code signing or notarization"],
            "screenshot": None, "focus_control_used": False, "clipboard_read": False}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--data", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = new_report()
    try:
        smoke(args.bundle, args.data, args.output, report)
        report["ok"] = True
    except Exception as error:
        report["error"] = str(error)
    if "output" in report:
        try:
            with private_file(Path(report["output"]) / "macos-window-smoke.json") as target:
                target.write((json.dumps(report, indent=2) + "\n").encode("utf-8"))
        except OSError as error:
            report["ok"] = False
            report["evidence_error"] = str(error)
    print(json.dumps(report))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
