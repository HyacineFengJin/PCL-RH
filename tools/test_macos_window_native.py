"""Mac GUI smoke fixtures use mocked native observation, AppKit and Popen only."""
from __future__ import annotations

import ctypes as c
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import types
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("pcl_macos_smoke_test", Path(__file__).parent / "ci/macos-window-smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)
REAL_REQUIRE_CI = SMOKE.require_ci


class Clock:
    def __init__(self):
        self.value = 0.0

    def monotonic(self):
        return self.value

    def sleep(self, duration):
        self.value += duration


class Child:
    def __init__(self, clock):
        self.pid, self.returncode = 12345, None
        self.clock = clock
        self.terminate_calls = self.kill_calls = 0
        self.ignore_term = False

    def poll(self):
        return self.returncode

    def wait(self, timeout):
        if self.returncode is None:
            self.clock.sleep(timeout)
            raise subprocess.TimeoutExpired("owned fixture", timeout)
        return self.returncode

    def terminate(self):
        self.terminate_calls += 1
        if not self.ignore_term:
            self.returncode = -15

    def kill(self):
        self.kill_calls += 1
        self.returncode = -9


@unittest.skipUnless(os.name == "posix", "Private directory fixtures require actual Unix permissions")
class MacWindowTests(unittest.TestCase):
    def setUp(self):
        work = Path(__file__).resolve().parents[1] / "work"
        work.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="Mac GUI fixture 中文 &-", dir=work)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.bundle = self.root / "App copy 空格 &.app"
        self.binary = self.bundle / "Contents/MacOS/pcl-desktop"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b"inert executable, never run\n")
        self.binary.chmod(0o755)
        self.data, self.output = self.root / "new data 中文 &", self.root / "private evidence"
        self.report = SMOKE.new_report()
        self.clock = Clock()
        self.process = Child(self.clock)
        self.api = mock.Mock()
        self.api.terminate.side_effect = self.normal_exit
        self.spawned = []
        for patch in (
            mock.patch.dict(os.environ, {"RUNNER_TEMP": str(self.root), "HOME": "/real-user-home",
                "PCL_RH_HOME": "/old-home", "PCL_LINUX_HOME": "/legacy-home",
                "PCL_RH_RESOURCE_ROOT": "/inherited-resource", "PCL_RH_DATA_ROOT": "/inherited-data"}),
            mock.patch.object(SMOKE, "require_ci", return_value=None),
            mock.patch.object(SMOKE.time, "monotonic", side_effect=self.clock.monotonic),
            mock.patch.object(SMOKE.time, "sleep", side_effect=self.clock.sleep),
            mock.patch.object(SMOKE, "MacApplication", return_value=self.api),
            mock.patch.object(SMOKE.subprocess, "Popen", side_effect=self.spawn),
            mock.patch.object(SMOKE.WINDOW, "observe", side_effect=lambda pid: self.observation(pid)),
        ):
            patch.start()
            self.addCleanup(patch.stop)

    def normal_exit(self, process):
        self.assertIs(process, self.process)
        process.returncode = 0
        return True

    def spawn(self, command, **options):
        self.spawned.append((command, options))
        return self.process

    def observation(self, pid):
        self.assertEqual(pid, self.process.pid)
        return {"method": "fixture PID metadata", "windows": [{"id": "42", "owner_pid": pid,
                "visible": True, "title": "PCL RH", "width": 1048, "height": 716}]}

    def smoke(self):
        SMOKE.smoke(self.bundle, self.data, self.output, self.report)

    def test_local_self_hosted_wrong_os_and_root_are_rejected_before_native_access(self):
        for values in ({"GITHUB_ACTIONS": "false"}, {"RUNNER_ENVIRONMENT": "self-hosted"},
                       {"RUNNER_OS": "Linux"}):
            environment = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "macOS", **values}
            with self.subTest(values=values), mock.patch.dict(os.environ, environment), \
                    mock.patch.object(SMOKE, "require_ci", side_effect=REAL_REQUIRE_CI), \
                    mock.patch.object(SMOKE.sys, "platform", "darwin"), \
                    mock.patch.object(SMOKE.platform, "system", return_value="Darwin"), \
                    self.assertRaisesRegex(RuntimeError, "disposable"):
                self.smoke()
        with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "macOS"}), \
                mock.patch.object(SMOKE, "require_ci", side_effect=REAL_REQUIRE_CI), \
                mock.patch.object(SMOKE.sys, "platform", "darwin"), \
                mock.patch.object(SMOKE.platform, "system", return_value="Darwin"), \
                mock.patch.object(SMOKE.os, "geteuid", return_value=0), self.assertRaisesRegex(RuntimeError, "disposable"):
            self.smoke()
        self.assertFalse(self.data.exists() or self.output.exists())
        self.assertEqual(self.spawned, [])
        self.api.terminate.assert_not_called()

    def test_success_proves_only_owned_stable_window_and_orderly_exit(self):
        self.smoke()
        self.assertTrue(self.report["native_window_visible"])
        self.assertGreaterEqual(self.report["stable_seconds"], 2.0)
        self.assertEqual(self.report["window"]["owner_pid"], self.process.pid)
        self.assertEqual(self.report["cleanup"]["exit_code"], 0)
        self.assertTrue(self.report["cleanup"]["normal_termination_requested"])
        self.assertFalse(self.report["cleanup"]["forced"])
        self.assertEqual((self.process.terminate_calls, self.process.kill_calls), (0, 0))
        self.assertLessEqual(self.clock.value, 45.0)
        self.assertIsNone(self.report["screenshot"])
        self.assertFalse(self.report["focus_control_used"] or self.report["clipboard_read"])
        self.assertIn("React rendering", self.report["does_not_prove"])
        self.assertIn("Gatekeeper acceptance", self.report["does_not_prove"])

    def test_real_entry_has_no_diagnostic_flags_and_private_environment_disables_clipboard_plugin(self):
        self.smoke()
        command, options = self.spawned[0]
        self.assertEqual(command, [str(self.binary)])
        self.assertIs(options["shell"], False)
        environment = options["env"]
        self.assertEqual(environment["PCL_RH_DATA_ROOT"], str(self.data))
        for key in ("PCL_RH_RESOURCE_ROOT", "PCL_RH_HOME", "PCL_LINUX_HOME"):
            self.assertNotIn(key, environment)
        self.assertEqual(environment["HOME"], str(self.output / "isolated-home"))
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "TMPDIR"):
            path = Path(environment[key])
            self.assertTrue(path.is_relative_to(self.output))
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o700)
        workspace = self.data / ".pcl-rust/rh-code/workspace.json"
        self.assertEqual(json.loads(workspace.read_bytes()), {"schemaVersion": 1, "projects": [],
            "disabledPlugins": ["launcher.discovery.clipboard"], "layoutPresets": []})
        self.assertEqual(stat.S_IMODE(workspace.stat().st_mode), 0o600)
        for directory in (self.data, self.data / ".pcl-rust", workspace.parent, self.output):
            self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o700)
        self.assertFalse((self.bundle / ".pcl-rust").exists())

    def test_existing_data_link_or_profile_is_never_opened(self):
        self.data.symlink_to(self.root / "missing-real-profile")
        with self.assertRaisesRegex(RuntimeError, "new data root"):
            self.smoke()
        self.assertEqual(self.spawned, [])
        self.assertFalse(self.output.exists())
        self.assertTrue(self.data.is_symlink())

    def test_escaped_bundle_executable_and_public_evidence_directory_are_rejected(self):
        self.binary.unlink()
        self.binary.symlink_to(self.root / "unrelated-executable")
        with self.assertRaises((RuntimeError, FileNotFoundError)):
            self.smoke()
        self.assertEqual(self.spawned, [])
        self.binary.unlink()
        self.binary.write_bytes(b"inert")
        self.binary.chmod(0o755)
        self.output.mkdir(mode=0o755)
        with self.assertRaisesRegex(RuntimeError, "private"):
            self.smoke()
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o755)
        self.assertFalse(self.data.exists())

    def test_other_pid_wrong_title_and_short_or_invisible_windows_never_pass(self):
        def wrong(pid):
            windows = self.observation(pid)["windows"]
            windows[0]["owner_pid"] = pid + 1
            return {"method": "fixture", "windows": [*windows,
                {"id": "43", "owner_pid": pid, "visible": True, "title": "Other", "width": 1048, "height": 716},
                {"id": "44", "owner_pid": pid, "visible": True, "title": "PCL RH", "width": 809, "height": 500},
                {"id": "45", "owner_pid": pid, "visible": False, "title": "PCL RH", "width": 1048, "height": 716}]}
        with mock.patch.object(SMOKE.WINDOW, "observe", side_effect=wrong), self.assertRaisesRegex(RuntimeError, "No owned"):
            self.smoke()
        self.assertFalse(self.report["native_window_visible"])
        self.assertTrue(all(window["owner_pid"] == self.process.pid for window in self.report["observed_owned_windows"]))
        self.assertLessEqual(self.clock.value, 45.0)

    def test_window_identity_and_visibility_must_remain_continuous(self):
        def changing(pid):
            observation = self.observation(pid)
            observation["windows"][0]["id"] = str(1 + int(self.clock.value * 10))
            return observation
        with mock.patch.object(SMOKE.WINDOW, "observe", side_effect=changing), self.assertRaisesRegex(RuntimeError, "No owned"):
            self.smoke()
        self.assertFalse(self.report["native_window_visible"])

    def test_early_exited_child_cannot_pass(self):
        self.process.returncode = 0
        with self.assertRaisesRegex(RuntimeError, "exited before"):
            self.smoke()
        self.api.terminate.assert_not_called()
        self.assertFalse(self.report["native_window_visible"])

    def test_nonzero_normal_exit_cannot_pass_even_after_stable_visibility(self):
        def failed_exit(process):
            process.returncode = 7
            return True
        self.api.terminate.side_effect = failed_exit
        with self.assertRaisesRegex(RuntimeError, "did not exit successfully"):
            self.smoke()
        self.assertTrue(self.report["native_window_visible"])
        self.assertEqual(self.report["cleanup"]["exit_code"], 7)
        self.assertFalse(self.report["cleanup"]["forced"])

    def test_native_termination_exception_still_retires_only_the_owned_child(self):
        self.api.terminate.side_effect = c.ArgumentError("synthetic ABI failure")
        with self.assertRaisesRegex(RuntimeError, "did not exit successfully"):
            self.smoke()
        self.assertTrue(self.report["cleanup"]["forced"])
        self.assertEqual(self.process.terminate_calls, 1)
        self.assertEqual(self.process.kill_calls, 0)
        self.assertIn("ABI failure", self.report["cleanup"]["normal_termination_error"])

    def test_rejected_termination_forces_only_owned_child_and_fails_the_smoke(self):
        self.api.terminate.side_effect = None
        self.api.terminate.return_value = False
        self.process.ignore_term = True
        with self.assertRaisesRegex(RuntimeError, "did not exit successfully"):
            self.smoke()
        self.api.terminate.assert_called_once_with(self.process)
        self.assertTrue(self.report["cleanup"]["forced"])
        self.assertTrue(self.report["cleanup"]["kill_sent"])
        self.assertEqual((self.process.terminate_calls, self.process.kill_calls), (1, 1))
        self.assertLessEqual(self.clock.value, 45.0)

    def test_log_budget_failure_still_closes_only_the_owned_child(self):
        def noisy(command, **options):
            options["stdout"].truncate(SMOKE.LOG_LIMIT + 1)
            return self.spawn(command, **options)
        with mock.patch.object(SMOKE.subprocess, "Popen", side_effect=noisy), self.assertRaisesRegex(RuntimeError, "log exceeded"):
            self.smoke()
        self.api.terminate.assert_called_once_with(self.process)
        self.assertEqual(self.report["cleanup"]["exit_code"], 0)
        self.assertFalse(self.report["native_window_visible"])

    def test_cli_writes_private_truthful_json_after_actual_entry_protocol(self):
        arguments = ["macos-window-smoke.py", "--bundle", str(self.bundle), "--data", str(self.data), "--output", str(self.output)]
        with mock.patch.object(SMOKE.sys, "argv", arguments), mock.patch.object(SMOKE.sys, "stdout", io.StringIO()) as stream:
            self.assertEqual(SMOKE.main(), 0)
        report = json.loads(stream.getvalue())
        saved = self.output / "macos-window-smoke.json"
        self.assertEqual(json.loads(saved.read_bytes()), report)
        self.assertTrue(report["ok"])
        self.assertEqual(stat.S_IMODE(saved.stat().st_mode), 0o600)


class ObjCTypesTests(unittest.TestCase):
    def test_pid_and_bool_message_signatures_and_pool_cleanup_are_explicit(self):
        for machine, bool_type in (("arm64", c.c_bool), ("x86_64", c.c_byte)):
            with self.subTest(machine=machine):
                signatures, calls, selectors = [], [], {}
                def selector(name):
                    selectors.setdefault(name, 100 + len(selectors))
                    return selectors[name]
                library = types.SimpleNamespace(objc_getClass=mock.Mock(side_effect=lambda name: 10 if name == b"NSRunningApplication" else 20),
                                                sel_registerName=mock.Mock(side_effect=selector))
                def message_factory(result_type, *argument_types):
                    signatures.append((result_type, argument_types))
                    def bind(_symbol):
                        def send(receiver, name, *arguments):
                            calls.append((receiver, name, arguments))
                            text = next(key for key, value in selectors.items() if value == name)
                            if text in (b"alloc", b"init"):
                                return 40
                            if text == b"runningApplicationWithProcessIdentifier:":
                                return 30
                            if text == b"processIdentifier":
                                return 12345
                            if text == b"terminate":
                                return 1
                            return None
                        return send
                    return bind
                with mock.patch.object(SMOKE.c, "CDLL", return_value=library), \
                        mock.patch.object(SMOKE.c, "CFUNCTYPE", side_effect=message_factory), \
                        mock.patch.object(SMOKE.platform, "machine", return_value=machine):
                    api = SMOKE.MacApplication()
                    process = types.SimpleNamespace(pid=12345, poll=lambda: None)
                    self.assertTrue(api.terminate(process))
                    self.assertIn((c.c_void_p, (c.c_void_p, c.c_void_p, c.c_int)), signatures)
                    self.assertIn((c.c_int, (c.c_void_p, c.c_void_p)), signatures)
                    self.assertIn((bool_type, (c.c_void_p, c.c_void_p)), signatures)
                    self.assertEqual(calls[-1][1], selectors[b"drain"])
                    self.assertIn((10, selectors[b"runningApplicationWithProcessIdentifier:"], (12345,)), calls)

    def test_rebound_application_pid_never_receives_a_termination_message(self):
        api = object.__new__(SMOKE.MacApplication)
        api.pool_class, api.application = 20, 10
        api.selectors = {name: name for name in ("alloc", "init", "drain", "runningApplicationWithProcessIdentifier:", "processIdentifier", "terminate")}
        api.object_message = mock.Mock(return_value=40)
        api.application_for_pid = mock.Mock(return_value=30)
        api.pid_message = mock.Mock(return_value=999)
        api.bool_message, api.void_message = mock.Mock(), mock.Mock()
        with self.assertRaisesRegex(RuntimeError, "no longer identifies"):
            api.terminate(types.SimpleNamespace(pid=12345, poll=lambda: None))
        api.bool_message.assert_not_called()
        api.void_message.assert_called_once_with(40, "drain")


if __name__ == "__main__":
    unittest.main()
