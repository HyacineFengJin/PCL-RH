"""Sampling admission and bounds, without sampling any host application."""
from __future__ import annotations

import ctypes
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("macos_sample", Path(__file__).with_name("macos_sample.py"))
SAMPLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SAMPLER)


class SamplingTests(unittest.TestCase):
    def test_xnu_structure_has_native_size_and_start_offsets(self):
        self.assertEqual(ctypes.sizeof(SAMPLER.ProcBSDInfo), 136)
        self.assertEqual(SAMPLER.ProcBSDInfo.start_sec.offset, 120)
        self.assertEqual(SAMPLER.ProcBSDInfo.start_usec.offset, 128)

    def test_non_ci_host_never_opens_processes_or_starts_a_sampler(self):
        with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), \
             mock.patch.object(SAMPLER, "_libproc") as native, \
             mock.patch.object(SAMPLER.subprocess, "Popen") as popen:
            result = SAMPLER.capture(100, 99, Path("/unused/java"), Path("/unused/report.log"))
        self.assertEqual(result["status"], "blocked")
        self.assertFalse(result["java_stop_requested"])
        native.assert_not_called()
        popen.assert_not_called()

    def test_output_must_be_new_nonhidden_and_under_selected_artifacts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            output = root / "target/game-evidence/vanilla-native-stack.log"
            output.parent.mkdir(parents=True)
            parent = root / "client-parent"
            private = parent / (".install-" + "a" * 32)
            java = private / ".pcl-rust/java/runtime/bin/java"
            java.parent.mkdir(parents=True)
            java.write_bytes(b"not executed")
            (private / "game-acceptance-owner.json").write_text(json.dumps({
                "schema_version": 1, "purpose": "disposable-game-acceptance"}))
            with mock.patch.dict(os.environ, {"GITHUB_WORKSPACE": str(root), "PCL_ACCEPTANCE_PARENT": str(parent)}):
                expected, selected = SAMPLER._paths(java, Path("target/game-evidence/vanilla-native-stack.log"))
                self.assertEqual(expected, java)
                self.assertEqual(selected, output)
                output.write_text("existing evidence")
                with self.assertRaises(ValueError):
                    SAMPLER._paths(java, output)
                self.assertEqual(output.read_text(), "existing evidence")
                for wrong in (root / "other.log", output.parent / ".hidden.log", output.parent / "../escape.log"):
                    with self.assertRaises(ValueError):
                        SAMPLER._paths(java, wrong)

    def test_wrong_parent_is_rejected_before_executable_lookup(self):
        identity = {"pid": 100, "ppid": 17, "uid": 1234, "start_sec": int(time.time()) - 12, "start_usec": 0}
        native = mock.Mock()
        with mock.patch.object(SAMPLER, "_process_info", return_value=identity), \
             mock.patch.object(SAMPLER.os, "geteuid", return_value=1234, create=True):
            with self.assertRaisesRegex(ValueError, "selected monitor"):
                SAMPLER._identity(native, 100, 99, Path("/unused/java"))
        native.proc_pidpath.assert_not_called()

    def test_changed_start_identity_never_publishes_a_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "native-stack.log"
            def sample(_pid, raw, _deadline):
                raw.write_text("synthetic diagnostic fixture, not a game result")
                return {"helper_exit_code": 0, "helper_reaped": True, "helper_timed_out": False}
            with mock.patch.object(SAMPLER, "_host_guard"), \
                 mock.patch.object(SAMPLER, "_paths", return_value=(Path("/fixture/java"), output)), \
                 mock.patch.object(SAMPLER, "_libproc"), \
                 mock.patch.object(SAMPLER, "_identity", side_effect=[{"start_sec": 1}, {"start_sec": 2}]), \
                 mock.patch.object(SAMPLER, "_sample", side_effect=sample):
                result = SAMPLER.capture(100, 99, Path("/fixture/java"), output)
            self.assertEqual(result["status"], "failed")
            self.assertFalse(result["identity_unchanged"])
            self.assertFalse(output.exists())

    @unittest.skipUnless(os.name == "posix" and hasattr(os, "O_NOFOLLOW"),
                         "The macOS sampler publishes through a native Unix O_NOFOLLOW descriptor")
    def test_report_prefix_is_capped_and_existing_output_is_never_truncated(self):
        with tempfile.TemporaryDirectory() as temporary:
            raw, output = Path(temporary) / "raw.log", Path(temporary) / "kept.log"
            raw.write_bytes(b"s" * (SAMPLER.REPORT_LIMIT + 17))
            result = SAMPLER._publish(raw, output)
            self.assertEqual(output.stat().st_size, SAMPLER.REPORT_LIMIT)
            self.assertEqual(result["source_bytes"], SAMPLER.REPORT_LIMIT + 17)
            self.assertTrue(result["truncated"])
            with self.assertRaises(FileExistsError):
                SAMPLER._publish(raw, output)
            self.assertEqual(output.stat().st_size, SAMPLER.REPORT_LIMIT)

    @unittest.skipUnless(os.name == "posix", "The macOS sampler uses POSIX selector-readable pipes")
    def test_console_is_drained_but_only_a_bounded_prefix_is_retained(self):
        real_popen = subprocess.Popen
        with tempfile.TemporaryDirectory() as temporary:
            raw = Path(temporary) / "raw.log"
            def own_fixture(args, **options):
                self.assertEqual(args, ["/usr/bin/sample", "100", "1", "1", "-file", str(raw)])
                code = "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('fixture'); print('c'*70000)"
                return real_popen([sys.executable, "-c", code, str(raw)], **options)
            with mock.patch.object(SAMPLER.subprocess, "Popen", side_effect=own_fixture):
                result = SAMPLER._sample(100, raw, time.monotonic() + 10)
        self.assertEqual(result["helper_exit_code"], 0)
        self.assertTrue(result["helper_reaped"])
        self.assertTrue(result["console_truncated"])
        self.assertLessEqual(len(result["console"].encode()), SAMPLER.CONSOLE_LIMIT)

    @unittest.skipUnless(os.name == "posix", "The macOS sampler uses POSIX selector-readable pipes")
    def test_timeout_reaps_only_the_owned_synthetic_helper(self):
        real_popen = subprocess.Popen
        children = []
        with tempfile.TemporaryDirectory() as temporary:
            raw = Path(temporary) / "raw.log"
            def own_fixture(_args, **options):
                child = real_popen([sys.executable, "-c", "import time; time.sleep(20)"], **options)
                children.append(child)
                return child
            with mock.patch.object(SAMPLER.subprocess, "Popen", side_effect=own_fixture):
                result = SAMPLER._sample(100, raw, time.monotonic() + 1.1)
        self.assertTrue(result["helper_timed_out"])
        self.assertTrue(result["helper_terminated"])
        self.assertTrue(result["helper_reaped"])
        self.assertTrue(all(child.poll() is not None for child in children))

    def test_child_inventory_uses_only_selected_parent_and_has_a_hard_count_bound(self):
        native = mock.Mock()
        def children(flavor, parent, buffer, size):
            self.assertEqual((flavor, parent), (6, 100))
            self.assertEqual(size, 17 * ctypes.sizeof(ctypes.c_int))
            buffer[0], buffer[1] = 101, 102
            return 2 * ctypes.sizeof(ctypes.c_int)
        native.proc_listpids.side_effect = children
        self.assertEqual(SAMPLER._children(native, 100), [101, 102])
        native.proc_listpids.side_effect = lambda *_: 17 * ctypes.sizeof(ctypes.c_int)
        with self.assertRaises(ValueError):
            SAMPLER._children(native, 100)

    def test_foreign_descendant_parent_never_grants_argv_read(self):
        root = {"pid": 100, "ppid": 99, "uid": 1234, "start_sec": 10, "start_usec": 0}
        wrong = {"pid": 101, "ppid": 88, "uid": 1234, "start_sec": 11, "start_usec": 0}
        with mock.patch.object(SAMPLER, "_process_info", side_effect=lambda _, pid: root if pid == 100 else wrong), \
             mock.patch.object(SAMPLER, "_children", return_value=[101]), \
             mock.patch.object(SAMPLER, "_command") as command:
            result = SAMPLER._child_arguments(None, root, time.monotonic() + 3)
        self.assertEqual(result["children"], [])
        command.assert_not_called()

    def test_native_child_query_error_is_not_an_empty_observation(self):
        native = mock.Mock()
        def denied(*_):
            ctypes.set_errno(13)
            return 0
        native.proc_listpids.side_effect = denied
        with self.assertRaises(OSError):
            SAMPLER._children(native, 100)

    def test_owned_argv_query_is_explicit_and_keeps_birth_evidence(self):
        root = {"pid": 100, "ppid": 99, "uid": 1234, "start_sec": 10, "start_usec": 0}
        child = {"pid": 101, "ppid": 100, "uid": 1234, "start_sec": 11, "start_usec": 0}
        image = mock.Mock()
        image.samefile.return_value = True
        response = ({"helper_exit_code": 0, "helper_timed_out": False, "stdout_truncated": False},
                    b"/bin/sh -c osascript -e 'fixture GLFW error'\n")
        with mock.patch.object(SAMPLER, "_process_info", side_effect=lambda _, pid: root if pid == 100 else child), \
             mock.patch.object(SAMPLER, "_children", side_effect=lambda _, pid: [101] if pid == 100 else []), \
             mock.patch.object(SAMPLER, "_process_path", return_value=image), \
             mock.patch.object(SAMPLER, "_command", return_value=response) as command:
            result = SAMPLER._child_arguments(None, root, time.monotonic() + 3)
        self.assertEqual(command.call_args.args[0], ["/bin/ps", "-ww", "-p", "101", "-o", "command="])
        self.assertEqual(result["children"][0]["identity_before"], result["children"][0]["identity_after"])
        self.assertIn("fixture GLFW error", result["children"][0]["command"])

    def test_missing_preinstalled_jcmd_does_not_install_or_execute_anything(self):
        with mock.patch.object(SAMPLER, "_select_jcmd", return_value=None), \
             mock.patch.object(SAMPLER, "_command") as command:
            result = SAMPLER._java_threads(None, 100, 99, Path("/fixture/java"), Path("/fixture/dump.log"), {}, time.monotonic() + 8)
        self.assertEqual(result["status"], "not_available")
        command.assert_not_called()

    @unittest.skipUnless(os.name == "posix", "The macOS sampler validates native Unix executable ownership/modes")
    def test_jcmd_discovery_requires_major21_and_a_trusted_home(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            home = root / "jdk"
            (home / "bin").mkdir(parents=True)
            (home / "release").write_text('JAVA_VERSION="17.0.8"\n')
            tool = home / "bin/jcmd"
            tool.write_bytes(b"fixture, never executed")
            tool.chmod(0o755)
            self.assertIsNone(SAMPLER._jdk_tool(home, [root]))
            (home / "release").write_text('JAVA_VERSION="21.0.7"\n')
            self.assertIsNone(SAMPLER._jdk_tool(home, [root / "different"]))
            self.assertEqual(SAMPLER._jdk_tool(home, [root]), tool)

    @unittest.skipUnless(os.name == "posix", "The macOS sampler uses POSIX selector-readable pipes")
    def test_auxiliary_helper_drains_both_bounded_streams(self):
        real_popen = subprocess.Popen
        def own_fixture(_args, **options):
            code = "import sys; sys.stdout.buffer.write(b't'*4000); sys.stdout.flush(); sys.stderr.buffer.write(b'e'*70000)"
            return real_popen([sys.executable, "-c", code], **options)
        with mock.patch.object(SAMPLER.subprocess, "Popen", side_effect=own_fixture):
            result, body = SAMPLER._command(["/fixture/helper"], time.monotonic() + 8, 1024)
        self.assertEqual(result["helper_exit_code"], 0)
        self.assertEqual(len(body), 1024)
        self.assertTrue(result["stdout_truncated"])
        self.assertTrue(result["console_truncated"])
        self.assertLessEqual(len(result["console"].encode()), SAMPLER.CONSOLE_LIMIT)


if __name__ == "__main__":
    unittest.main()
