"""Installer recipe boundaries; real setup/uninstall is exercised on Windows CI."""
from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("pcl_install_recipe", Path(__file__).with_name("installer-native.py"))
assert SPEC is not None and SPEC.loader is not None
INSTALL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALL)


class InstallerRecipeTests(unittest.TestCase):
    def test_windows_paths_reject_aliases_injection_and_private_state(self) -> None:
        for name in ("../secret", "/secret", "code\\a", "code/a:stream", "assets/CON.txt",
                     "experimental/LPT¹", "code/a.", "code/a ", 'code/a"; Flags: external',
                     "code/{#Exec('arbitrary')}", "code/*.mjs", "code/x\ny", ".pcl-rust/settings.json",
                     "Minecraft/.minecraft/saves/level.dat", "work/personal.txt", "private-key"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                INSTALL.member(name)
        self.assertEqual(INSTALL.member("experimental/runtime/node_modules/@scope/pkg/中文 & a.mjs"),
                         "experimental\\runtime\\node_modules\\@scope\\pkg\\中文 & a.mjs")

    def fixture(self, root: Path, names: list[str]) -> dict:
        files = []
        for name in names:
            data = (name + " synthetic resource").encode()
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            files.append({"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
        manifest = {"platform": "windows", "arch": "x86_64", "version": "0.2.0", "profile": "release",
                    "files": files}
        (root / "package-manifest.json").write_text(json.dumps(manifest))
        return manifest

    def test_exact_program_recipe_ignores_unlisted_user_data_and_binds_hashes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="installer recipe & 测试-") as temporary:
            root = Path(temporary)
            manifest = self.fixture(root, ["pcl-desktop.exe", "code/agent-adapter.mjs", "LICENSE"])
            (root / "private-note.txt").write_text("DO NOT INCLUDE")
            (root / ".pcl-rust").mkdir()
            (root / ".pcl-rust/settings.json").write_text("DO NOT INCLUDE")
            script = INSTALL.inno_script(root, manifest)
            self.assertEqual(script.count("Source: "), 4)
            self.assertTrue(next(line for line in script.splitlines() if line.startswith("Source:"))
                            .startswith('Source: "PCL-RH\\pcl-desktop.exe";'))
            for entry in manifest["files"]:
                self.assertIn('Hash: "' + entry["sha256"] + '"', script)
            self.assertNotIn("DO NOT INCLUDE", script)
            self.assertNotIn("private-note", script)
            self.assertNotIn(".pcl-rust", script)
            self.assertNotIn("[UninstallDelete]", script)
            self.assertNotIn("[InstallDelete]", script)
            self.assertNotIn("[Run]", script)
            self.assertIn("PrivilegesRequired=lowest", script)
            self.assertIn("CloseApplications=no", script)
            self.assertIn("UninstallLogMode=append", script)
            self.assertIn("UsePreviousAppDir=yes", script)

    def test_manifest_case_collisions_and_bad_hashes_refuse_compilation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = self.fixture(root, ["code/a.mjs"])
            manifest["files"].append({**manifest["files"][0], "path": "code/A.mjs"})
            with self.assertRaisesRegex(ValueError, "collision"):
                INSTALL.inno_script(root, manifest)
            manifest["files"].pop()
            manifest["files"][0]["sha256"] = "bad hash\n[Code]"
            with self.assertRaisesRegex(ValueError, "hash"):
                INSTALL.inno_script(root, manifest)
            manifest["platform"] = "linux"
            with self.assertRaisesRegex(ValueError, "Windows"):
                INSTALL.inno_script(root, manifest)


if __name__ == "__main__":
    unittest.main()
