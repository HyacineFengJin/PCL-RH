"""Read one ACKed disposable Windows CI Java process's error-dialog text.

Diagnostic only: no clicks, focus changes, screenshots, Java signals or verdict
changes. A query-only process handle is retained through every window read.
"""
from __future__ import annotations

import ctypes as c
import json
import os
from pathlib import Path
import platform
import re
import stat
import time

READ_SECONDS = 1.0
MESSAGE_MS = 50
TEXT_CHARS = 4096  # WM_GETTEXT includes the terminating null in this limit.
CONTROL_LIMIT = 16


def _host_guard() -> None:
    if platform.system() != "Windows" or os.environ.get("GITHUB_ACTIONS") != "true" \
            or os.environ.get("RUNNER_OS") != "Windows" \
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise ValueError("Dialog text is limited to a disposable GitHub-hosted Windows runner")


def _runtime(expected: Path, parent: Path) -> Path:
    parent, expected = parent.resolve(strict=True), expected.resolve(strict=True)
    parts = expected.relative_to(parent).parts
    if len(parts) < 6 or not re.fullmatch(r"\.install-[0-9a-f]{32}", parts[0]) \
            or parts[1:3] != (".pcl-rust", "java") \
            or expected.name.lower() != "java.exe" or expected.parent.name.lower() != "bin" \
            or not expected.is_file():
        raise ValueError("Expected Java is not the newly allocated acceptance runtime")
    marker = parent / parts[0] / "game-acceptance-owner.json"
    info = marker.lstat()
    if not stat.S_ISREG(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400 \
            or info.st_size > 1024:
        raise ValueError("Expected Java lacks the disposable acceptance owner record")
    with marker.open("rb") as stream:
        data = stream.read(1025)
    if len(data) > 1024 or json.loads(data) != {"schema_version": 1, "purpose": "disposable-game-acceptance"}:
        raise ValueError("Expected Java lacks the disposable acceptance owner record")
    return expected


class _Win32:
    def __init__(self):
        from ctypes import wintypes as w
        self.w = w
        self.kernel = c.WinDLL("kernel32", use_last_error=True)
        self.user = c.WinDLL("user32", use_last_error=True)
        self.callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
        declarations = (
            (self.kernel.OpenProcess, [w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            (self.kernel.CloseHandle, [w.HANDLE], w.BOOL),
            (self.kernel.GetProcessId, [w.HANDLE], w.DWORD),
            (self.kernel.GetExitCodeProcess, [w.HANDLE, c.POINTER(w.DWORD)], w.BOOL),
            (self.kernel.GetProcessTimes, [w.HANDLE] + [c.POINTER(w.FILETIME)] * 4, w.BOOL),
            (self.kernel.QueryFullProcessImageNameW,
             [w.HANDLE, w.DWORD, w.LPWSTR, c.POINTER(w.DWORD)], w.BOOL),
            (self.user.EnumWindows, [self.callback_type, w.LPARAM], w.BOOL),
            (self.user.EnumChildWindows, [w.HWND, self.callback_type, w.LPARAM], w.BOOL),
            (self.user.GetWindowThreadProcessId, [w.HWND, c.POINTER(w.DWORD)], w.DWORD),
            (self.user.GetClassNameW, [w.HWND, w.LPWSTR, c.c_int], c.c_int),
            (self.user.IsWindowVisible, [w.HWND], w.BOOL),
            (self.user.GetAncestor, [w.HWND, w.UINT], w.HWND),
            (self.user.GetWindowLongW, [w.HWND, c.c_int], w.LONG),
            (self.user.SendMessageTimeoutW,
             [w.HWND, w.UINT, w.WPARAM, w.LPARAM, w.UINT, w.UINT, c.POINTER(c.c_size_t)], c.c_ssize_t),
        )
        for function, arguments, result in declarations:
            function.argtypes, function.restype = arguments, result

    def open(self, pid):
        handle = self.kernel.OpenProcess(0x1000, False, pid)  # QUERY_LIMITED_INFORMATION only.
        if not handle:
            raise OSError("Cannot retain the acknowledged Java process")
        return handle

    def close(self, handle):
        if not self.kernel.CloseHandle(handle):
            raise OSError("Cannot close the diagnostic process handle")

    def identity(self, handle):
        w = self.w
        code = w.DWORD()
        created, exited, kernel, user = (w.FILETIME() for _ in range(4))
        if not self.kernel.GetExitCodeProcess(handle, c.byref(code)) or code.value != 259 \
                or not self.kernel.GetProcessTimes(handle, c.byref(created), c.byref(exited),
                                                   c.byref(kernel), c.byref(user)):
            raise ValueError("The retained Java process is no longer live")
        image, size = c.create_unicode_buffer(32768), w.DWORD(32768)
        if not self.kernel.QueryFullProcessImageNameW(handle, 0, image, c.byref(size)) \
                or not 0 < size.value < len(image):
            raise OSError("Cannot query the retained Java image")
        return {"pid": int(self.kernel.GetProcessId(handle)),
                "creation_filetime": (created.dwHighDateTime << 32) | created.dwLowDateTime,
                "executable": image.value}

    def _enumerate(self, dialog, visit):
        errors, stopped = [], []
        @self.callback_type
        def callback(window, _):
            try:
                keep = bool(visit(window))
                if not keep:
                    stopped.append(True)
                return keep
            except Exception as error:
                errors.append(error)
                return False
        if dialog is None:
            ok = self.user.EnumWindows(callback, 0)
            if not ok and not stopped and not errors:
                raise OSError("Cannot enumerate the acknowledged process's dialog")
        else:
            self.user.EnumChildWindows(dialog, callback, 0)
        if errors:
            raise errors[0]

    def windows(self, visit):
        self._enumerate(None, visit)

    def children(self, dialog, visit):
        self._enumerate(dialog, visit)

    def owner(self, window):
        pid = self.w.DWORD()
        self.user.GetWindowThreadProcessId(window, c.byref(pid))
        return pid.value

    def klass(self, window):
        text = c.create_unicode_buffer(128)
        return text.value if self.user.GetClassNameW(window, text, len(text)) else ""

    def visible(self, window):
        return bool(self.user.IsWindowVisible(window))

    def root(self, window):
        return self.user.GetAncestor(window, 2)  # GA_ROOT, not another process's owner window.

    def text_static(self, window):
        # SS_ICON's WM_GETTEXT result can be an icon handle, not text.
        return self.user.GetWindowLongW(window, -16) & 0x1f in (0, 1, 2, 11, 12)

    def text(self, window, timeout_ms):
        text, copied = c.create_unicode_buffer(TEXT_CHARS), c.c_size_t()
        # WM_GETTEXT only. Never broadcast; do not use NOTIMEOUTIFNOTHUNG.
        ok = self.user.SendMessageTimeoutW(window, 0x000d, len(text), c.addressof(text),
                                           0x0001 | 0x0002 | 0x0020, timeout_ms, c.byref(copied))
        if not ok:
            raise OSError("Owned Static text read timed out or its window disappeared")
        if copied.value >= len(text):
            raise ValueError("Owned Static text exceeded the fixed buffer")
        return text[:copied.value], copied.value == len(text) - 1


def _identity(api, handle, pid, expected):
    identity = api.identity(handle)
    age = time.time() - (identity["creation_filetime"] / 10_000_000 - 11644473600)
    image = Path(identity["executable"])
    if identity["pid"] != pid or not -2 <= age <= 300 or not image.is_absolute() \
            or not image.samefile(expected):
        raise ValueError("Retained process is not the fresh acknowledged Java image")
    return identity


def _read(api, handle, pid, expected, before):
    started = time.monotonic()
    deadline = started + READ_SECONDS
    dialogs, attempted, visited = [], 0, 0
    limited = False

    def live():
        if _identity(api, handle, pid, expected) != before:
            raise ValueError("Retained Java identity changed during dialog observation")

    def own_dialog(window):
        return api.owner(window) == pid and api.visible(window) and api.klass(window) == "#32770"

    def own_static(dialog, window):
        return api.owner(window) == pid and api.root(window) == dialog \
            and api.visible(window) and api.klass(window) == "Static" and api.text_static(window)

    def visit(dialog):
        nonlocal attempted, visited, limited
        visited += 1
        if time.monotonic() >= deadline or visited > 4096 or len(dialogs) >= 4:
            limited = True
            return False
        if not own_dialog(dialog):
            return True
        entry = {"window_id": str(dialog), "class": "#32770", "controls": []}
        dialogs.append(entry)
        seen = set()
        def control(window):
            nonlocal attempted, limited
            if time.monotonic() >= deadline or len(seen) >= 256 or attempted >= CONTROL_LIMIT:
                limited = True
                return False
            if window in seen:
                return True
            seen.add(window)
            if not own_static(dialog, window):
                return True
            live()
            if not own_dialog(dialog) or not own_static(dialog, window):
                raise ValueError("Owned dialog/control changed before text admission")
            remaining_ms = int((deadline - time.monotonic()) * 1000)
            if remaining_ms < 1:
                limited = True
                return False
            attempted += 1
            try:
                text, truncated = api.text(window, min(MESSAGE_MS, remaining_ms))
            except OSError as error:
                observed = {"window_id": str(window), "error": str(error)[:256]}
            else:
                observed = {"window_id": str(window), "class": "Static", "text": text[:TEXT_CHARS - 1],
                            "truncated": truncated or len(text) >= TEXT_CHARS}
            live()
            if not own_dialog(dialog) or not own_static(dialog, window):
                raise ValueError("Owned dialog/control changed during its text read")
            entry["controls"].append(observed)
            return True
        api.children(dialog, control)
        return attempted < CONTROL_LIMIT and time.monotonic() < deadline
    api.windows(visit)
    live()
    return {"status": "captured" if any(item.get("text") for dialog in dialogs for item in dialog["controls"])
            else "no_text_observed", "dialogs": dialogs, "identity_unchanged": True,
            "controls_attempted": attempted, "limit_reached": limited,
            "read_elapsed_seconds": time.monotonic() - started}


def capture(pid: int, expected_executable: Path, parent: Path) -> dict:
    result = {"schema_version": 1, "kind": "windows_owned_java_error_dialog", "pid": pid,
              "diagnostic_only": True, "affects_game_acceptance": False,
              "read_budget_seconds": READ_SECONDS, "message_timeout_ms": MESSAGE_MS,
              "control_limit": CONTROL_LIMIT, "text_buffer_characters": TEXT_CHARS,
              "screenshots_taken": False, "focus_changed": False, "buttons_clicked": False}
    api, handle = None, None
    try:
        _host_guard()
        if type(pid) is not int or not 0 < pid < 2 ** 32:
            raise ValueError("Expected an ACKed game PID")
        expected = _runtime(expected_executable, parent)
        api = _Win32()
        handle = api.open(pid)
        before = _identity(api, handle, pid, expected)
        result.update(_read(api, handle, pid, expected, before))
        result["identity_before"] = before
        result["identity_after"] = _identity(api, handle, pid, expected)
        if result["identity_after"] != before:
            raise ValueError("Java identity changed after dialog observation")
    except (OSError, ValueError, RuntimeError) as error:
        result.update(status="diagnostic_failed", reason=str(error)[:512])
        result.pop("dialogs", None)
    finally:
        if handle is not None:
            try:
                api.close(handle)
                result["process_handle_closed"] = True
            except OSError as error:
                result.update(status="diagnostic_failed", process_handle_closed=False, reason=str(error)[:512])
    return result
