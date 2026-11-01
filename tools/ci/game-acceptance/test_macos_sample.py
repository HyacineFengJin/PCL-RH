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
        identity = {"pid": 100, "ppid": 17, "uid": os.geteuid(), "start_sec": int(time.time()) - 12, "start_usec": 0}
        native = mock.Mock()
        with mock.patch.object(SAMPLER, "_process_info", return_value=identity):
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


if __name__ == "__main__":
    unittest.main()
