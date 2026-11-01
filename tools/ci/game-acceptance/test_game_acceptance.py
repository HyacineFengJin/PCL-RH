"""Finite evidence regressions; these tests do not simulate a game pass."""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("game_acceptance", Path(__file__).parents[1] / "game-acceptance.py")
ACCEPTANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ACCEPTANCE)
COLLECT_SPEC = importlib.util.spec_from_file_location("collect_game", Path(__file__).parents[1] / "collect-game-evidence.py")
COLLECT = importlib.util.module_from_spec(COLLECT_SPEC)
COLLECT_SPEC.loader.exec_module(COLLECT)


class EvidenceTests(unittest.TestCase):
    def test_report_is_separate_from_console_and_cannot_overwrite_an_existing_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "reports/space & report.json"
            with mock.patch("builtins.print") as console:
                ACCEPTANCE.write_report({"game_acceptance": "incomplete"}, report)
            console.assert_not_called()
            self.assertEqual(json.loads(report.read_bytes()), {"game_acceptance": "incomplete"})
            with self.assertRaises(FileExistsError):
                ACCEPTANCE.write_report({"replacement": True}, report)
            self.assertEqual(json.loads(report.read_bytes()), {"game_acceptance": "incomplete"})

    def collector_fixture(self, temporary, *, extended=False):
        root = Path(temporary)
        parent = root / "parent"
        workspace = parent / "workspace"
        logs = workspace / "game/.pcl-linux/logs"
        logs.mkdir(parents=True)
        (workspace / "game-acceptance-owner.json").write_text(json.dumps(
            {"schema_version": 1, "purpose": "disposable-game-acceptance"}))
        (logs / "stderr.log").write_bytes(b"native fixture error\n")
        spelling = str(workspace.resolve())
        if extended and not spelling.startswith("\\\\?\\"):
            spelling = "\\\\?\\" + spelling
        report = root / "report.json"
        report.write_text(json.dumps({"client_attempt": {"client": {"workspace": spelling}}}))
        return report, parent, root / "collected"

    def test_monitor_logs_are_collected_outside_hidden_artifact_directories(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, parent, output = self.collector_fixture(temporary)
            result = COLLECT.collect(report, parent, output)
            self.assertTrue(result["collected"])
            entry = result["files"][0]
            self.assertEqual(entry["path"], "game/.pcl-linux/logs/stderr.log")
            self.assertEqual(entry["collected_path"], "00-stderr.log")
            self.assertEqual((output / entry["collected_path"]).read_bytes(), b"native fixture error\n")

    def test_collector_refuses_a_marker_bearing_workspace_under_another_parent(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, parent, output = self.collector_fixture(temporary)
            other = parent.parent / "other"
            other.mkdir()
            with self.assertRaisesRegex(ValueError, "direct child"):
                COLLECT.collect(report, other, output)
            self.assertFalse(output.exists())

    @unittest.skipUnless(os.name == "nt", "Windows native path namespaces")
    def test_windows_verbatim_workspace_keeps_the_same_real_parent_authority(self):
        with tempfile.TemporaryDirectory() as temporary:
            report, parent, output = self.collector_fixture(temporary, extended=True)
            result = COLLECT.collect(report, parent, output)
            self.assertTrue(result["collected"])

    def test_display_presence_and_preflight_cannot_pass_game_stages(self):
        with tempfile.TemporaryDirectory() as parent, \
             mock.patch.object(ACCEPTANCE, "memory_available", return_value=8 * ACCEPTANCE.GIB), \
             mock.patch.object(ACCEPTANCE, "session_observation", return_value={"status": "available"}), \
             mock.patch.object(ACCEPTANCE, "opengl_observation", return_value={"status": "available"}):
            result = ACCEPTANCE.preflight(Path(parent))
        self.assertEqual(result["run_readiness"], "candidate")
        self.assertEqual(result["game_acceptance"], "not_run")
        self.assertTrue(all(value["status"] == "not_run" for value in result["stages"].values()))

    def test_software_context_is_explicit_and_version_is_real(self):
        data = "OpenGL renderer string: llvmpipe (LLVM 18)\nOpenGL core profile version string: 4.5 (Core Profile) Mesa\n"
        result = ACCEPTANCE.parse_glxinfo(data, 0)
        self.assertEqual(result["status"], "available")
        self.assertTrue(result["software_renderer"])
        self.assertTrue(result["opengl_3_2"])
        for wrong in ("OpenGL version string: 1.1\n", "error: cannot open display\n"):
            self.assertEqual(ACCEPTANCE.parse_glxinfo(wrong, 0)["status"], "blocked")
        self.assertEqual(ACCEPTANCE.parse_glxinfo(data, 1)["status"], "blocked")

    def test_real_window_alone_is_insufficient(self):
        result = ACCEPTANCE.render_assessment({"attempt_status": "complete", "stages": {}},
                                             {"sustained_visible_seconds": 10})
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(result["interactive_gameplay"], "not_run")

    def test_early_pipeline_failure_preserves_a_blocked_assessment(self):
        client = {"attempt_status": "fail", "stages": {"game_initialization": {"status": "not_run", "evidence": None}}}
        self.assertEqual(ACCEPTANCE.render_assessment(client, {})["status"], "blocked")

    def test_renderer_failure_and_missing_cleanup_cannot_be_passed_by_log_markers(self):
        client = {"attempt_status": "complete", "stages": {
            "game_initialization": {"evidence": {"initialization_markers": ["Created:", "Backend library:"],
                                                  "renderer_errors": ["GLFW error 65542"]}},
            "owned_shutdown": {"status": "pass"}}}
        self.assertEqual(ACCEPTANCE.render_assessment(client, {"sustained_visible_seconds": 3})["status"], "blocked")
        client["stages"]["game_initialization"]["evidence"]["renderer_errors"] = []
        client["stages"]["owned_shutdown"]["status"] = "fail"
        self.assertEqual(ACCEPTANCE.render_assessment(client, {"sustained_visible_seconds": 3})["status"], "blocked")

    def test_rendered_menu_cannot_substitute_for_actual_singleplayer_join(self):
        client = {"attempt_status": "complete", "stages": {
            "game_initialization": {"evidence": {"initialization_markers": ["Created:", "Backend library:"],
                                                  "renderer_errors": []}},
            "owned_shutdown": {"status": "pass"}}}
        result = ACCEPTANCE.render_assessment(client, {"sustained_visible_seconds": 5})
        self.assertEqual(result["status"], "blocked")
        self.assertFalse(result["singleplayer_join"])

    def test_known_old_binary_commit_cannot_enable_unknown_role(self):
        commit = "a" * 40
        for fields in ({"schema_version": 1}, {"schema_version": 2, "runtime_layout": "legacy"}):
            with self.assertRaises(ValueError):
                ACCEPTANCE.validate_build({"commit": commit, "product_id": "dev.pcl-rh.desktop", **fields}, commit)


if __name__ == "__main__":
    unittest.main()
