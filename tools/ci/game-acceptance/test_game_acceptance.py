"""Finite evidence regressions; these tests do not simulate a game pass."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("game_acceptance", Path(__file__).parents[1] / "game-acceptance.py")
ACCEPTANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ACCEPTANCE)


class EvidenceTests(unittest.TestCase):
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
