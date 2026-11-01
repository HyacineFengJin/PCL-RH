"""Owned-dialog admission/bounds regressions; no real Java or desktop IO."""
from __future__ import annotations

import ctypes
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("windows_dialog", Path(__file__).with_name("windows_dialog.py"))
DIALOG = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DIALOG)
NOW = 1_800_000_000
HOST = {"GITHUB_ACTIONS": "true", "RUNNER_OS": "Windows", "RUNNER_ENVIRONMENT": "github-hosted"}


class NativeFixture:
    def __init__(self, executable):
        self.identity_value = {"pid": 55, "creation_filetime": (NOW + 11644473600) * 10_000_000,
                               "executable": str(executable)}
        self.items = {
            1: {"pid": 55, "class": "#32770", "visible": True},
            2: {"pid": 55, "class": "Static", "root": 1, "text": "GLFW error 65542: fixture"},
            3: {"pid": 55, "class": "Button", "root": 1},
            4: {"pid": 55, "class": "Static", "root": 1, "icon": True},
            9: {"pid": 99, "class": "#32770", "visible": True},
            10: {"pid": 99, "class": "Static", "root": 1},
        }
        self.dialogs = [9, 1]
        self.controls = [10, 3, 4, 2]
        self.reads, self.closed = [], []
        self.on_text = lambda: None
        self.on_identity = lambda: None

    def open(self, pid):
        if pid != 55:
            raise AssertionError("Unexpected process admission")
        return 77

    def close(self, handle):
        self.closed.append(handle)

    def identity(self, handle):
        if handle != 77:
            raise AssertionError("Process handle changed")
        self.on_identity()
        return self.identity_value.copy()

    def windows(self, visit):
        for window in self.dialogs:
            if not visit(window):
                break

    def children(self, dialog, visit):
        for window in self.controls:
            if not visit(window):
                break

    def owner(self, window):
        return self.items[window]["pid"]

    def klass(self, window):
        if self.owner(window) != 55:
            raise AssertionError("Foreign window metadata was inspected")
        return self.items[window]["class"]

    def visible(self, window):
        return self.items[window].get("visible", True)

    def root(self, window):
        return self.items[window].get("root")

    def text_static(self, window):
        return not self.items[window].get("icon", False)

    def text(self, window, timeout):
        if self.owner(window) != 55 or self.klass(window) != "Static" or not self.text_static(window):
            raise AssertionError("Foreign/nontext/button read")
        self.reads.append((window, timeout))
        self.on_text()
        return self.items[window].get("text", "fixture"), False


class DialogTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name)
        workspace = self.parent / (".install-" + "a" * 32)
        self.java = workspace / ".pcl-rust/java/official-runtime/bin/java.exe"
        self.java.parent.mkdir(parents=True)
        self.java.write_bytes(b"inert fixture, never executed")
        self.marker = workspace / "game-acceptance-owner.json"
        self.marker.write_text(json.dumps({"schema_version": 1, "purpose": "disposable-game-acceptance"}))
        self.api = NativeFixture(self.java)

    def capture(self):
        with mock.patch.dict(os.environ, HOST), mock.patch.object(DIALOG.platform, "system", return_value="Windows"), \
                mock.patch.object(DIALOG, "_Win32", return_value=self.api), \
                mock.patch.object(DIALOG.time, "time", return_value=NOW):
            return DIALOG.capture(55, self.java, self.parent)

    def test_only_hosted_windows_can_open_a_process(self):
        for values, system in ((HOST, "Linux"), ({**HOST, "RUNNER_ENVIRONMENT": "self-hosted"}, "Windows"),
                               ({**HOST, "GITHUB_ACTIONS": "false"}, "Windows")):
            with self.subTest(values=values, system=system), mock.patch.dict(os.environ, values), \
                    mock.patch.object(DIALOG.platform, "system", return_value=system), \
                    mock.patch.object(DIALOG, "_Win32") as api:
                self.assertEqual(DIALOG.capture(55, self.java, self.parent)["status"], "diagnostic_failed")
                api.assert_not_called()

    def test_private_runtime_marker_is_required_before_process_admission(self):
        self.marker.write_text('{"schema_version":1,"purpose":"user-data"}')
        with mock.patch.object(self.api, "open") as opened:
            self.assertEqual(self.capture()["status"], "diagnostic_failed")
        opened.assert_not_called()

    def test_exact_native_image_must_match_prepared_runtime(self):
        other = self.parent / "another-java.exe"
        other.write_bytes(b"different inert image")
        self.api.identity_value["executable"] = str(other)
        result = self.capture()
        self.assertEqual(result["status"], "diagnostic_failed")
        self.assertEqual(self.api.reads, [])
        self.assertEqual(self.api.closed, [77])

    def test_only_owned_visible_dialog_static_text_is_read(self):
        result = self.capture()
        self.assertEqual(result["status"], "captured")
        self.assertEqual([window for window, _ in self.api.reads], [2])
        self.assertEqual(result["dialogs"][0]["controls"][0]["text"], "GLFW error 65542: fixture")
        self.assertTrue(result["identity_unchanged"])
        self.assertEqual(result["identity_before"], result["identity_after"])
        self.assertEqual(self.api.closed, [77])
        self.assertFalse(result["affects_game_acceptance"])
        self.assertFalse(result["buttons_clicked"])

    def test_hidden_dialog_and_foreign_process_text_are_never_read(self):
        self.api.items[1]["visible"] = False
        result = self.capture()
        self.assertEqual(result["status"], "no_text_observed")
        self.assertEqual(self.api.reads, [])

    def test_same_pid_with_a_changed_birth_identity_drops_text(self):
        self.api.on_text = lambda: self.api.identity_value.update(creation_filetime=self.api.identity_value["creation_filetime"] + 1)
        result = self.capture()
        self.assertEqual(result["status"], "diagnostic_failed")
        self.assertNotIn("dialogs", result)
        self.assertEqual(self.api.closed, [77])

    def test_reused_control_handle_is_rejected_after_read(self):
        self.api.on_text = lambda: self.api.items[2].update(pid=99)
        result = self.capture()
        self.assertEqual(result["status"], "diagnostic_failed")
        self.assertNotIn("dialogs", result)

    def test_message_and_character_limits_are_aggregate(self):
        self.api.controls = list(range(100, 140))
        for window in self.api.controls:
            self.api.items[window] = {"pid": 55, "class": "Static", "root": 1, "text": "x" * 5000}
        result = self.capture()
        controls = result["dialogs"][0]["controls"]
        self.assertEqual(len(self.api.reads), 16)
        self.assertTrue(all(1 <= timeout <= 50 for _, timeout in self.api.reads))
        self.assertEqual(len(controls), 16)
        self.assertTrue(all(len(control["text"]) == 4095 and control["truncated"] for control in controls))
        self.assertTrue(result["limit_reached"])

    def test_elapsed_budget_stops_further_text_admission(self):
        self.api.controls = list(range(100, 140))
        for window in self.api.controls:
            self.api.items[window] = {"pid": 55, "class": "Static", "root": 1}
        clock = [0.0]
        def progress():
            clock[0] += 0.2
        self.api.on_identity = progress
        with mock.patch.object(DIALOG.time, "monotonic", side_effect=lambda: clock[0]):
            result = self.capture()
        self.assertLess(len(self.api.reads), 16)
        self.assertLessEqual(sum(timeout for _, timeout in self.api.reads), 1000)
        self.assertTrue(result["limit_reached"])

    def test_timeout_is_diagnostic_only_and_still_closes_handle(self):
        with mock.patch.object(self.api, "text", side_effect=OSError("fixed-timeout fixture")):
            result = self.capture()
        self.assertEqual(result["status"], "no_text_observed")
        self.assertIn("fixed-timeout", result["dialogs"][0]["controls"][0]["error"])
        self.assertTrue(result["process_handle_closed"])
        self.assertFalse(result["affects_game_acceptance"])

    def test_native_access_and_message_parameters_stay_query_only_and_bounded(self):
        api = object.__new__(DIALOG._Win32)
        api.kernel, api.user = mock.Mock(), mock.Mock()
        api.kernel.OpenProcess.return_value = 77
        self.assertEqual(api.open(55), 77)
        api.kernel.OpenProcess.assert_called_once_with(0x1000, False, 55)
        api.user.SendMessageTimeoutW.return_value = 1
        self.assertEqual(api.text(2, 50), ("", False))
        args = api.user.SendMessageTimeoutW.call_args.args
        self.assertEqual(args[:3], (2, 0x000d, 4096))
        self.assertEqual(args[4:6], (0x23, 50))
        self.assertEqual(ctypes.sizeof(ctypes.c_size_t), ctypes.sizeof(ctypes.c_void_p))


if __name__ == "__main__":
    unittest.main()
