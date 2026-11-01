"""Owned Windows close diagnostics use mocked clocks, windows and Popen only."""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("pcl_windows_smoke_test", Path(__file__).parent / "ci/windows-window-smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class Clock:
    def __init__(self):
        self.value = 0.0

    def monotonic(self):
        return self.value

    def sleep(self, duration):
        self.value += duration


class Child:
    pid = 12345

    def __init__(self, clock):
        self.clock, self.exit_at, self.returncode = clock, None, None
        self.natural_code = 0
        self.terminate_calls = self.wait_calls = 0

    def poll(self):
        if self.returncode is None and self.exit_at is not None and self.clock.value >= self.exit_at:
            self.returncode = self.natural_code
        return self.returncode

    def terminate(self):
        self.terminate_calls += 1
        self.returncode = 1  # TerminateProcess cleanup; never an app crash claim.

    def wait(self, timeout):
        self.wait_calls += 1
        if self.poll() is None:
            self.clock.sleep(timeout)
            raise subprocess.TimeoutExpired("owned fixture", timeout)
        return self.returncode


class Windows:
    hwnd = 42

    def __init__(self, clock, child):
        self.clock, self.child = clock, child
        self.close_calls, self.probes = [], []
        self.close_at = self.disappear_after = self.exit_after_close = None
        self.answered = True
        self.probe_duration = 0.0

    def owned(self, hwnd, pid):
        return hwnd == self.hwnd and pid == self.child.pid and (
            self.close_at is None or self.disappear_after is None
            or self.clock.value - self.close_at < self.disappear_after)

    def windows(self, pid):
        if pid != self.child.pid:
            raise AssertionError("Observed an unrelated PID")
        main = [{"hwnd": self.hwnd, "title": "PCL RH", "client_width": 1028, "client_height": 716}]
        if not self.owned(self.hwnd, pid):
            main = []
        return [*main, {"hwnd": 99, "title": "", "client_width": 0, "client_height": 0}]

    def close(self, hwnd, pid):
        if not self.owned(hwnd, pid):
            raise RuntimeError("Refused another HWND/PID")
        self.close_calls.append((hwnd, pid))
        self.close_at = self.clock.value
        if self.exit_after_close is not None:
            self.child.exit_at = self.clock.value + self.exit_after_close

    def responsiveness(self, hwnd, pid, timeout_ms):
        if not self.owned(hwnd, pid):
            raise AssertionError("Probed another HWND/PID")
        self.probes.append((self.clock.value, hwnd, pid, timeout_ms))
        self.clock.sleep(self.probe_duration)
        return {"responsive": self.answered, "timeout_ms": timeout_ms,
                "error_code": 0 if self.answered else 1460}


class OwnedCloseTests(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        self.child = Child(self.clock)
        self.api = Windows(self.clock, self.child)
        for patch in (
            mock.patch.object(SMOKE.time, "monotonic", side_effect=self.clock.monotonic),
            mock.patch.object(SMOKE.time, "sleep", side_effect=self.clock.sleep),
        ):
            patch.start()
            self.addCleanup(patch.stop)

    def cleanup(self, deadline=30.0):
        return SMOKE.cleanup(self.child, self.api, self.api.hwnd, deadline)

    def test_timely_zero_exit_records_close_elapsed_without_forcing_or_repeating_close(self):
        self.api.exit_after_close = 0.2
        result = self.cleanup()
        self.assertEqual(self.api.close_calls, [(42, self.child.pid)])
        self.assertTrue(result["wm_close_sent"])
        self.assertFalse(result["terminated"])
        self.assertEqual(result["exit_code"], 0)
        self.assertAlmostEqual(result["close_elapsed_seconds"], 0.2)
        self.assertEqual(result["close_budget_seconds"], 7.0)
        self.assertEqual(result["close_observation"], "process_exited_after_wm_close")
        self.assertEqual((self.child.terminate_calls, self.child.wait_calls), (0, 0))

    def test_timeout_keeps_seven_seconds_and_one_close_with_bounded_trace_and_probes(self):
        self.api.answered, self.api.probe_duration = False, 0.05
        result = self.cleanup()
        self.assertEqual(len(self.api.close_calls), 1)
        self.assertEqual(self.child.terminate_calls, 1)
        self.assertEqual(result["exit_code"], 1)
        self.assertTrue(result["close_timed_out"] and result["terminated"])
        self.assertAlmostEqual(result["close_elapsed_seconds"], 7.0)
        self.assertLessEqual(self.clock.value, 30.0)
        self.assertLessEqual(len(result["close_trace"]), 16)
        self.assertTrue(all(len(row["visible_owned_hwnds"]) <= 32 for row in result["close_trace"]))
        self.assertTrue(all(probe[1:] == (42, self.child.pid, 50) for probe in self.api.probes))
        self.assertTrue(all(second[0] - first[0] >= 0.5 for first, second in zip(self.api.probes, self.api.probes[1:])))
        self.assertEqual(result["close_observation"], "main_window_still_owned_last_wm_null_unanswered")

    def test_disappeared_main_with_lingering_process_never_probes_another_window(self):
        self.api.disappear_after = 0.0
        result = self.cleanup()
        self.assertEqual(len(self.api.close_calls), 1)
        self.assertEqual(self.api.probes, [])
        self.assertTrue(result["terminated"])
        self.assertEqual(result["close_observation"], "main_window_no_longer_owned_process_still_alive")
        self.assertTrue(all(row["visible_owned_hwnds"] == [99] and not row["main_hwnd_still_owned"]
                            for row in result["close_trace"]))

    def test_responsive_but_unclosed_main_is_reported_and_never_passed_by_wm_null(self):
        result = self.cleanup()
        self.assertTrue(result["terminated"] and result["close_timed_out"])
        self.assertEqual(result["close_observation"], "main_window_still_owned_last_wm_null_responded")
        self.assertEqual(len(self.api.close_calls), 1)

    def test_no_remaining_probe_margin_sends_no_wm_null_and_keeps_total_deadline(self):
        result = self.cleanup(deadline=2.05)
        self.assertEqual(self.api.probes, [])
        self.assertAlmostEqual(result["close_elapsed_seconds"], 0.05)
        self.assertLessEqual(self.clock.value, 2.05)

    def test_window_observation_failure_remains_diagnostic_and_does_not_add_close_attempts(self):
        with mock.patch.object(self.api, "windows", side_effect=RuntimeError("synthetic observation failure")):
            result = self.cleanup()
        self.assertEqual(len(self.api.close_calls), 1)
        self.assertEqual(result["close_observation"], "close_window_observation_unavailable")
        self.assertTrue(result["close_timed_out"] and result["terminated"])
        self.assertTrue(all("observation_error" in row for row in result["close_trace"]))

    def test_already_exited_process_has_no_close_probe_or_force(self):
        self.child.returncode = 0
        result = self.cleanup()
        self.assertEqual(self.api.close_calls, [])
        self.assertEqual(self.api.probes, [])
        self.assertFalse(result["wm_close_sent"] or result["terminated"])

    def test_native_close_and_probe_refuse_a_rebound_foreign_hwnd(self):
        api = object.__new__(SMOKE.Windows)
        api.user = types.SimpleNamespace(PostMessageW=mock.Mock(return_value=1), SendMessageTimeoutW=mock.Mock(return_value=1))
        api.owned = mock.Mock(return_value=False)
        with self.assertRaisesRegex(RuntimeError, "no longer belongs"):
            api.close(42, self.child.pid)
        with self.assertRaisesRegex(RuntimeError, "no longer belongs"):
            api.responsiveness(42, self.child.pid, 50)
        api.user.PostMessageW.assert_not_called()
        api.user.SendMessageTimeoutW.assert_not_called()

    def test_native_probe_uses_only_wm_null_and_an_explicit_fifty_ms_ceiling(self):
        api = object.__new__(SMOKE.Windows)
        api.user = types.SimpleNamespace(SendMessageTimeoutW=mock.Mock(return_value=1))
        api.owned = mock.Mock(return_value=True)
        with mock.patch.object(SMOKE.ctypes, "set_last_error", create=True), \
                mock.patch.object(SMOKE.ctypes, "get_last_error", return_value=0, create=True):
            self.assertTrue(api.responsiveness(42, self.child.pid, 50)["responsive"])
        arguments = api.user.SendMessageTimeoutW.call_args.args
        self.assertEqual(arguments[:6], (42, 0, 0, 0, 0x22, 50))
        for timeout in (0, 51, 1000):
            with self.subTest(timeout=timeout), self.assertRaisesRegex(RuntimeError, "timeout"):
                api.responsiveness(42, self.child.pid, timeout)
        self.assertEqual(api.user.SendMessageTimeoutW.call_count, 1)

    def test_smoke_still_requires_zero_exit_without_terminate(self):
        work = Path(__file__).resolve().parents[1] / "work"
        work.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="Win window fixture 空格 &-", dir=work) as temporary:
            folder = Path(temporary).resolve()
            root, output = folder / "portable", folder / "evidence"
            root.mkdir()
            executable = root / "pcl-desktop.exe"
            executable.write_bytes(b"inert executable, never run")
            (root / "pcl-rh.json").write_text('{"schemaVersion":1,"productId":"dev.pcl-rh.portable"}')
            self.api.exit_after_close = 0.2
            self.child.natural_code = 7
            report = {}
            with mock.patch.object(SMOKE.sys, "platform", "win32"), \
                    mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true"}), \
                    mock.patch.object(SMOKE, "Windows", return_value=self.api), \
                    mock.patch.object(SMOKE.subprocess, "Popen", return_value=self.child), \
                    self.assertRaisesRegex(RuntimeError, "did not exit successfully"):
                SMOKE.smoke(executable, root, output, report)
            self.assertTrue(report["native_window_visible"])
            self.assertEqual(report["cleanup"]["exit_code"], 7)
            self.assertFalse(report["cleanup"]["terminated"])
            self.assertEqual(len(self.api.close_calls), 1)

    def test_late_zero_exit_cannot_turn_an_observed_close_timeout_green(self):
        work = Path(__file__).resolve().parents[1] / "work"
        work.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="Win late-close fixture-", dir=work) as temporary:
            folder = Path(temporary).resolve()
            executable = folder / "pcl-desktop.exe"
            executable.write_bytes(b"inert executable, never run")
            (folder / "pcl-rh.json").write_text('{"schemaVersion":1,"productId":"dev.pcl-rh.portable"}')
            late = {"wm_close_sent": True, "terminated": False, "exit_code": 0, "close_timed_out": True}
            with mock.patch.object(SMOKE.sys, "platform", "win32"), \
                    mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true"}), \
                    mock.patch.object(SMOKE, "Windows", return_value=self.api), \
                    mock.patch.object(SMOKE.subprocess, "Popen", return_value=self.child), \
                    mock.patch.object(SMOKE, "cleanup", return_value=late), \
                    self.assertRaisesRegex(RuntimeError, "did not exit successfully"):
                SMOKE.smoke(executable, folder, folder / "evidence", {})


if __name__ == "__main__":
    unittest.main()
