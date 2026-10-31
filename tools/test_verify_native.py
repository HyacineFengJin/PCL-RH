"""Check actual candidate-verifier functions with synthetic metadata, never fake executables."""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("pcl_verify_native", Path(__file__).parent / "ci/verify-native-package.py")
assert SPEC is not None and SPEC.loader is not None
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)
COMMIT = "0123456789abcdef0123456789abcdef01234567"


def manifest() -> dict:
    return {"commit": COMMIT, "version": "0.2.0", "cli_version": "0.1.0", "profile": "release",
            "platform": "linux", "arch": "x86_64", "curseforge_embedded": False}


def desktop() -> dict:
    return {"schema_version": 1, "product_id": "dev.pcl-rh.desktop", "version": "0.2.0",
            "commit": COMMIT, "profile": "release", "target": "x86_64-unknown-linux-gnu",
            "target_os": "linux", "target_arch": "x86_64", "debug_assertions": True,
            "custom_protocol": True, "curseforge_embedded": False}


class NativeCandidateVerifierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.environment = mock.patch.dict(os.environ, {"GITHUB_SHA": COMMIT})
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def test_embedded_release_identity_accepts_independent_debug_assertions(self) -> None:
        self.assertEqual(VERIFY.verify_desktop_build(json.dumps(desktop()) + "\n", manifest()), desktop())
        for platform, arch, target in (("windows", "x86_64", "x86_64-pc-windows-msvc"),
                                       ("macos", "aarch64", "aarch64-apple-darwin"),
                                       ("macos", "x86_64", "x86_64-apple-darwin")):
            with self.subTest(platform=platform, arch=arch):
                declared = {**manifest(), "platform": platform, "arch": arch}
                embedded = {**desktop(), "target_os": platform, "target_arch": arch, "target": target}
                self.assertEqual(VERIFY.verify_desktop_build(json.dumps(embedded), declared), embedded)

    def test_wrong_or_unknown_binary_identity_and_services_are_rejected(self) -> None:
        for field, value in (("schema_version", True), ("product_id", "other.product"),
                             ("version", "0.1.0"), ("commit", None), ("commit", "f" * 40),
                             ("profile", "debug"), ("target_os", "windows"), ("target_arch", "aarch64"),
                             ("target", "unknown"), ("target", "/private/build/path"),
                             ("custom_protocol", False), ("custom_protocol", 1),
                             ("debug_assertions", "false"), ("curseforge_embedded", True),
                             ("curseforge_embedded", 0)):
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                VERIFY.verify_desktop_build(json.dumps({**desktop(), field: value}), manifest())
        with self.assertRaisesRegex(ValueError, "Unsupported"):
            VERIFY.verify_desktop_build(json.dumps({**desktop(), "unexpected": "refused"}), manifest())
        with self.assertRaises(ValueError):
            VERIFY.verify_desktop_build("[]", manifest())
        with self.assertRaisesRegex(ValueError, "output bound"):
            VERIFY.verify_desktop_build(" " * 16_385, manifest())

    def test_manifest_and_desktop_cannot_agree_on_another_ci_commit(self) -> None:
        wrong = "f" * 40
        with self.assertRaisesRegex(ValueError, "another CI commit"):
            VERIFY.verify_desktop_build(json.dumps({**desktop(), "commit": wrong}), {**manifest(), "commit": wrong})

    def test_explicit_embedded_service_requires_matching_binary_boolean(self) -> None:
        embedded = {**desktop(), "curseforge_embedded": True}
        declared = {**manifest(), "curseforge_embedded": True}
        self.assertEqual(VERIFY.verify_desktop_build(json.dumps(embedded), declared), embedded)
        with self.assertRaisesRegex(ValueError, "service declaration"):
            VERIFY.verify_desktop_build(json.dumps(desktop()), declared)

    def test_cli_version_is_independent_and_exact(self) -> None:
        VERIFY.verify_cli_version("pcl-cli 0.1.0\n", "0.1.0")
        for invalid in ("pcl-cli 0.2.0\n", "pcl-cli 0.1.0", "pcl-cli 0.1.0\nextra\n", "pcl-cli 0.1.0 \n"):
            with self.subTest(output=invalid), self.assertRaisesRegex(ValueError, "CLI version"):
                VERIFY.verify_cli_version(invalid, "0.1.0")

    def test_smoke_uses_extracted_desktop_report_and_returns_verified_fields(self) -> None:
        with tempfile.TemporaryDirectory(prefix="candidate smoke 测试 ") as temporary:
            scratch = Path(temporary)
            root = scratch / "PCL-RH"
            root.mkdir()
            (root / "pcl-rh.json").write_text(json.dumps({"schemaVersion": 1, "productId": "dev.pcl-rh.portable"}))
            replies = [json.dumps(desktop()), "pcl-cli 0.1.0\n", "[]",
                       '\n'.join(json.dumps({"ok": True, "result": result}) for result in
                                 ({"entries": []}, {"busy": False}, True)), "", "", "", ""]
            # Only run() is replaced. Actual smoke dispatch, metadata validators,
            # private cwd and state checks run; no synthetic binary is executed.
            with mock.patch.object(VERIFY, "native_binary"), mock.patch.object(VERIFY.shutil, "which", return_value="synthetic-node"), \
                    mock.patch.object(VERIFY, "run", side_effect=replies) as run:
                result = VERIFY.smoke(root, scratch, manifest())
            self.assertEqual(result, desktop())
            suffix = ".exe" if os.name == "nt" else ""
            self.assertEqual(run.call_args_list[0], mock.call([str(root / ("pcl-desktop" + suffix)), "--build-info"], scratch))
            self.assertEqual(run.call_args_list[1], mock.call([str(root / ("pcl-cli" + suffix)), "--version"], scratch))
            self.assertFalse((root / ".pcl-rust").exists())

    def test_failed_report_and_output_limit_keep_existing_process_failure_guards(self) -> None:
        failed = subprocess.CompletedProcess(["synthetic"], 1, "", "report failed")
        oversized = subprocess.CompletedProcess(["synthetic"], 0, "x" * (1024 * 1024 + 1), "")
        for reply in (failed, oversized):
            with mock.patch.object(VERIFY.subprocess, "run", return_value=reply) as run, self.assertRaises(ValueError):
                VERIFY.run(["synthetic", "--build-info"], Path("."))
            self.assertEqual(run.call_args.kwargs["timeout"], 20)


if __name__ == "__main__":
    unittest.main()
