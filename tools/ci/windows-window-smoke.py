#!/usr/bin/env python3
"""Observe and close one owned desktop window on a disposable Windows CI host.

This proves native visibility only, not React rendering, accounts or game/AI
functionality. It never captures the desktop or interacts with another process.
"""
from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


class Windows:
    def __init__(self) -> None:
        self.user = ctypes.WinDLL("user32", use_last_error=True)
        self.callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
        self.user.EnumWindows.argtypes = [self.callback_type, wintypes.LPARAM]
        self.user.EnumWindows.restype = wintypes.BOOL
        self.user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
        self.user.GetWindowThreadProcessId.restype = wintypes.DWORD
        self.user.IsWindowVisible.argtypes = [wintypes.HWND]
        self.user.IsWindowVisible.restype = wintypes.BOOL
        self.user.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        self.user.GetWindowTextW.restype = ctypes.c_int
        self.user.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
        self.user.GetClientRect.restype = wintypes.BOOL
        self.user.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
        self.user.PostMessageW.restype = wintypes.BOOL
        self.user.SendMessageTimeoutW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM,
            wintypes.LPARAM, wintypes.UINT, wintypes.UINT, ctypes.POINTER(ctypes.c_size_t)]
        self.user.SendMessageTimeoutW.restype = wintypes.LPARAM

    def owned(self, hwnd: int, pid: int) -> bool:
        actual = wintypes.DWORD()
        return bool(self.user.GetWindowThreadProcessId(hwnd, ctypes.byref(actual))) and actual.value == pid

    def windows(self, pid: int) -> list[dict]:
        found: list[dict] = []
        errors: list[Exception] = []

        @self.callback_type
        def visit(hwnd: int, _parameter: int) -> bool:
            try:
                if self.owned(hwnd, pid) and self.user.IsWindowVisible(hwnd):
                    title = ctypes.create_unicode_buffer(257)
                    self.user.GetWindowTextW(hwnd, title, len(title))
                    rect = wintypes.RECT()
                    if not self.user.GetClientRect(hwnd, ctypes.byref(rect)):
                        raise ctypes.WinError(ctypes.get_last_error())
                    if len(found) >= 32:
                        raise RuntimeError("Owned process exceeded the window observation limit")
                    found.append({"hwnd": int(hwnd), "title": title.value,
                                  "client_width": rect.right - rect.left,
                                  "client_height": rect.bottom - rect.top})
                return True
            except Exception as error:
                errors.append(error)
                return False

        ctypes.set_last_error(0)
        enumerated = self.user.EnumWindows(visit, 0)
        if errors:
            raise errors[0]
        if not enumerated:
            raise ctypes.WinError(ctypes.get_last_error())
        return found

    def close(self, hwnd: int, pid: int) -> None:
        # Reconfirm ownership immediately before posting. A reused HWND owned
        # by any other process is never a close target.
        if not self.owned(hwnd, pid):
            raise RuntimeError("Observed window no longer belongs to the owned process")
        if not self.user.PostMessageW(hwnd, 0x0010, 0, 0):  # WM_CLOSE, never SendMessage
            raise ctypes.WinError(ctypes.get_last_error())

    def responsiveness(self, hwnd: int, pid: int, timeout_ms: int) -> dict:
        require(type(timeout_ms) is int and 1 <= timeout_ms <= 50, "Invalid bounded WM_NULL probe timeout")
        if not self.owned(hwnd, pid):
            raise RuntimeError("Observed window no longer belongs to the owned process")
        reply = ctypes.c_size_t()
        ctypes.set_last_error(0)
        # WM_NULL is observational. ABORTIFHUNG | ERRORONEXIT retains a finite
        # timeout; NOTIMEOUTIFNOTHUNG would make the declared budget unreliable.
        answered = bool(self.user.SendMessageTimeoutW(hwnd, 0, 0, 0, 0x0002 | 0x0020,
                                                     timeout_ms, ctypes.byref(reply)))
        return {"responsive": answered, "timeout_ms": timeout_ms,
                "error_code": 0 if answered else ctypes.get_last_error()}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def close_snapshot(process: subprocess.Popen, api: Windows, hwnd: int,
                   started: float, deadline: float) -> dict:
    windows = api.windows(process.pid)
    require(len(windows) <= 32, "Owned close snapshot exceeded its window bound")
    present = api.owned(hwnd, process.pid)
    main = next((window for window in windows if window["hwnd"] == hwnd), None)
    result = {"elapsed_seconds": round(time.monotonic() - started, 6),
              "main_hwnd_still_owned": present, "main_window_visible": main is not None,
              "visible_owned_hwnds": [window["hwnd"] for window in windows]}
    if main is not None:
        result["main_client_width"], result["main_client_height"] = main["client_width"], main["client_height"]
    # Never probe a replacement window, including a different owned HWND. A
    # direct child remains unreaped until poll; an exited/reaped PID is refused.
    if present and process.poll() is None and deadline - time.monotonic() >= 0.075:
        try:
            result["wm_null"] = api.responsiveness(hwnd, process.pid, 50)
        except (OSError, RuntimeError) as error:
            result["wm_null_error"] = str(error)[:512]
    return result


def close_observation(trace: list[dict], exited: bool) -> str:
    if exited:
        return "process_exited_after_wm_close"
    if not trace:
        return "no_close_window_observation"
    last = trace[-1]
    if last.get("main_hwnd_still_owned") is False:
        return "main_window_no_longer_owned_process_still_alive"
    if last.get("main_hwnd_still_owned") is True:
        # A final metadata sample can have too little time left for WM_NULL.
        # Preserve the last actual probe for this continuous owned-HWND span;
        # its timestamp remains in the trace rather than inventing a new reply.
        for sample in reversed(trace):
            if sample.get("main_hwnd_still_owned") is not True:
                break
            if sample.get("wm_null", {}).get("responsive") is False:
                return "main_window_still_owned_last_wm_null_unanswered"
            if sample.get("wm_null", {}).get("responsive") is True:
                return "main_window_still_owned_last_wm_null_responded"
        return "main_window_still_owned_response_unknown"
    return "close_window_observation_unavailable"


def cleanup(process: subprocess.Popen, api: Windows, hwnd: int | None,
            deadline: float) -> dict:
    result = {"wm_close_sent": False, "terminated": False, "exit_code": process.poll(),
              "close_trace": [], "close_trace_interval_seconds": 0.5}
    if result["exit_code"] is not None:
        return result
    if hwnd is not None:
        try:
            api.close(hwnd, process.pid)
            result["wm_close_sent"] = True
            started = time.monotonic()
            close_deadline = min(started + 7.0, deadline - 2.0)
            result["close_budget_seconds"] = max(0.0, close_deadline - started)
            next_snapshot = started
            while process.poll() is None and time.monotonic() < close_deadline:
                now = time.monotonic()
                if now >= next_snapshot and len(result["close_trace"]) < 16:
                    try:
                        sample = close_snapshot(process, api, hwnd, started, close_deadline)
                    except (OSError, RuntimeError) as error:
                        sample = {"elapsed_seconds": round(time.monotonic() - started, 6),
                                  "observation_error": str(error)[:512]}
                    result["close_trace"].append(sample)
                    next_snapshot = time.monotonic() + 0.5
                remaining = close_deadline - time.monotonic()
                if remaining > 0 and process.poll() is None:
                    time.sleep(min(0.05, remaining))
            result["close_elapsed_seconds"] = round(time.monotonic() - started, 6)
            exited = process.poll() is not None
            result["close_observation"] = close_observation(result["close_trace"], exited)
            if result["close_trace"]:
                result["last_observation_age_seconds"] = round(
                    time.monotonic() - started - result["close_trace"][-1]["elapsed_seconds"], 6)
            if not exited:
                result["close_timed_out"] = True
                result["close_error"] = "Owned desktop did not exit within its WM_CLOSE deadline"
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            result["close_error"] = str(error)
    if process.poll() is None:
        # Popen retains the actual process HANDLE on Windows. Termination does
        # not reopen a PID, enumerate descendants, or target any user process.
        process.terminate()
        result["terminated"] = True
        try:
            process.wait(timeout=max(0.0, min(2.0, deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            result["cleanup_error"] = "Owned process exit was not confirmed within the total deadline"
    result["exit_code"] = process.poll()
    return result


def smoke(executable: Path, root: Path, output: Path, report: dict) -> None:
    require(sys.platform == "win32" and os.environ.get("GITHUB_ACTIONS") == "true",
            "This smoke check requires a disposable native Windows GitHub Actions runner")
    executable = executable.resolve(strict=True)
    root = root.resolve(strict=True)
    require(executable.is_file() and executable.name.lower() == "pcl-desktop.exe",
            "An explicit pcl-desktop.exe is required")
    require(root.is_dir(), "The explicit private portable root must be a directory")
    marker = root / "pcl-rh.json"
    require(not marker.is_symlink() and marker.stat().st_size <= 4096,
            "A bounded ordinary portable marker is required")
    require(json.loads(marker.read_text(encoding="utf-8")) == {
        "schemaVersion": 1, "productId": "dev.pcl-rh.portable"
    }, "Unsupported private portable root marker")
    require(not os.path.lexists(root / ".pcl-rust"),
            "GUI smoke requires a fresh root without any existing state or state link")
    output = output.resolve()
    require(not output.is_relative_to(root / ".pcl-rust"),
            "Logs and cwd must not precreate launcher state")
    output.mkdir(parents=True, exist_ok=True)
    cwd = Path(tempfile.mkdtemp(prefix="window-cwd-", dir=output))
    stdout = output / "desktop-stdout.log"
    stderr = output / "desktop-stderr.log"
    report.update({"executable": str(executable), "root": str(root), "cwd": str(cwd),
                   "stdout_log": str(stdout), "stderr_log": str(stderr)})
    environment = os.environ.copy()
    environment.update({"PCL_RH_HOME": str(root), "PCL_LINUX_HOME": str(root)})
    api = Windows()
    process = None
    hwnd = None
    failure = None
    started = time.monotonic()
    deadline = started + 30.0
    try:
        with stdout.open("xb") as stdout_file, stderr.open("xb") as stderr_file:
            process = subprocess.Popen([str(executable)], cwd=cwd, env=environment,
                                       stdin=subprocess.DEVNULL, stdout=stdout_file,
                                       stderr=stderr_file, close_fds=True)
            report["pid"] = process.pid
            stable = None
            stable_since = None
            while time.monotonic() < started + 20.0:
                require(process.poll() is None, "Desktop exited before window visibility was confirmed")
                require(stdout.stat().st_size <= 8 * 1024 * 1024
                        and stderr.stat().st_size <= 8 * 1024 * 1024,
                        "Desktop log exceeded the GUI smoke bound")
                windows = api.windows(process.pid)
                report["observed_visible_windows"] = windows
                candidates = [window for window in windows if window["title"] == "PCL RH"
                              and window["client_width"] >= 810 and window["client_height"] >= 500]
                candidate = next((window for window in candidates
                                  if window["hwnd"] == hwnd), None)
                if candidate is None and candidates:
                    candidate = candidates[0]
                if candidate is None:
                    stable = stable_since = None
                else:
                    hwnd = candidate["hwnd"]
                    key = (hwnd, candidate["client_width"], candidate["client_height"])
                    now = time.monotonic()
                    if key != stable:
                        stable, stable_since = key, now
                    elif now - stable_since >= 2.0:
                        report["window"] = candidate
                        report["stable_seconds"] = round(now - stable_since, 3)
                        break
                time.sleep(0.1)
            else:
                raise RuntimeError("No owned PCL RH window remained visible at >=810x500 for two seconds within 20 seconds")
            report["native_window_visible"] = True
    except Exception as error:
        failure = error
    finally:
        if process is not None:
            try:
                report["cleanup"] = cleanup(process, api, hwnd, deadline)
            except Exception as error:
                report["cleanup"] = {"cleanup_error": str(error), "exit_code": process.poll()}
                failure = failure or error
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
    if failure is not None:
        raise failure
    require(report["cleanup"]["wm_close_sent"] and not report["cleanup"]["terminated"]
            and not report["cleanup"].get("close_timed_out", False)
            and report["cleanup"]["exit_code"] == 0,
            "Desktop did not exit successfully after closing its owned window")
    require(stdout.stat().st_size <= 8 * 1024 * 1024 and stderr.stat().st_size <= 8 * 1024 * 1024,
            "Desktop log exceeded the GUI smoke bound during shutdown")
    require(time.monotonic() <= deadline, "GUI smoke exceeded its 30-second total deadline")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = {"schema_version": 1, "ok": False, "native_window_visible": False,
              "proves": "Native owned window visibility and orderly WM_CLOSE exit only",
              "does_not_prove": ["React rendering", "account functionality", "game launch", "AI functionality"],
              "screenshot": None,
              "screenshot_note": "No screenshot captured; no full-desktop or clipboard access"}
    try:
        smoke(args.executable, args.root, args.output, report)
        report["ok"] = True
    except Exception as error:
        report["error"] = str(error)
    # ASCII JSON also works when the Windows parent's console code page is not
    # UTF-8. Paths/titles retain their Unicode values after JSON decoding.
    print(json.dumps(report))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
