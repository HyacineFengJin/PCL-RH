"""Real archive boundaries and native image creation; synthetic binaries never run."""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import struct
import subprocess
import tarfile
import tempfile
import types
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("pcl_dmg_native", Path(__file__).with_name("dmg-native.py"))
DMG = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DMG)
COMMIT = "a" * 40
VERIFY_DMG = DMG.load("pcl_app_dmg_ci_test", Path(__file__).parent / "ci/verify-native-dmg.py")


@unittest.skipUnless(os.name == "posix", "DMG fixtures need Unix executable modes; both native macOS runners execute them")
class NativeDiskImageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="dmg fixture space & 项目-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.portable = self.root / "portable"
        self.portable.mkdir()
        self.output = self.root / "images"
        self.arch = "aarch64" if platform.machine().lower() in ("arm64", "aarch64") else "x86_64"
        header = bytearray(32)
        header[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", header, 4, 0x100000C if self.arch == "aarch64" else 0x1000007)
        struct.pack_into("<I", header, 12, 2)
        self.files = {"pcl-desktop": (bytes(header), 0o755), "pcl-cli": (bytes(header), 0o755),
                      "start-native.sh": (b"#!/bin/sh\n# inert fixture; never executed\n", 0o755),
                      "Start PCL RH.command": (b"#!/bin/sh\n# inert fixture; never executed\n", 0o755)}
        self.environment = mock.patch.dict(os.environ, {"PCL_NATIVE_SOURCE_COMMIT": COMMIT})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.write_archive()

    def write_archive(self, *, link=False):
        manifest = {"schema_version": 1, "product_id": "dev.pcl-rh.portable", "layout": "flat",
                    "platform": "macos", "arch": self.arch, "profile": "release", "version": "0.2.0",
                    "cli_version": "0.1.0", "commit": COMMIT, "signed": False,
                    "curseforge_embedded": False, "files": [
                        {"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(), "mode": mode}
                        for name, (data, mode) in self.files.items()]}
        raw = json.dumps(manifest).encode()
        (self.portable / "package-manifest.json").write_bytes(raw)
        archive = self.portable / "synthetic.tar.gz"
        with tarfile.open(archive, "w:gz") as target:
            for name, (data, mode) in {**self.files, "package-manifest.json": (raw, 0o644)}.items():
                entry = tarfile.TarInfo("PCL-RH/" + name)
                entry.mode, entry.size = mode, len(data)
                target.addfile(entry, io.BytesIO(data))
            if link:
                entry = tarfile.TarInfo("PCL-RH/linked")
                entry.type, entry.linkname = tarfile.SYMTYPE, "/outside"
                target.addfile(entry)
        archive.with_name(archive.name + ".sha256").write_text(DMG.sha256(archive) + "  " + archive.name + "\n")

    def native_context(self):
        return mock.patch.object(DMG.platform, "system", return_value="Darwin")

    def fake_hdiutil(self, command, **kwargs):
        self.assertEqual(command[0], "/usr/bin/hdiutil")
        self.assertTrue(kwargs["check"])
        if command[1] == "create":
            stage = Path(command[command.index("-srcfolder") + 1])
            self.assertEqual((stage / "README-install.txt").read_bytes(), DMG.INSTRUCTIONS)
            self.assertEqual({p.name for p in stage.iterdir()}, {"PCL-RH", "README-install.txt"})
            self.assertFalse((stage / "PCL-RH/.pcl-rust").exists())
            Path(command[command.index("-o") + 1]).write_bytes(b"synthetic disk image; never mounted")
        else:
            self.assertEqual(command[1], "verify")

    def test_bound_archive_and_instructions_are_published_together(self):
        with self.native_context(), mock.patch.object(DMG.subprocess, "run", side_effect=self.fake_hdiutil):
            report = DMG.build(self.portable, self.output)
        self.assertEqual(report["commit"], COMMIT)
        self.assertEqual(DMG.sha256(self.output / report["image"]), report["sha256"])
        self.assertTrue(report["requires_writable_copy"])
        self.assertFalse(report["signed"])
        self.assertFalse(report["user_data_included"])
        self.assertEqual(json.loads((self.output / "dmg-manifest.json").read_bytes()), report)
        self.assertEqual(list(self.output.glob(".pcl-dmg-*")), [])

    def test_native_tool_failure_leaves_no_published_candidate(self):
        with self.native_context(), mock.patch.object(DMG.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "hdiutil")), \
                self.assertRaises(subprocess.CalledProcessError):
            DMG.build(self.portable, self.output)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_existing_candidate_is_not_replaced(self):
        self.output.mkdir()
        existing = self.output / "dmg-manifest.json"
        existing.write_bytes(b"keep existing candidate")
        with self.native_context(), mock.patch.object(DMG.subprocess, "run") as run, self.assertRaisesRegex(ValueError, "replace"):
            DMG.build(self.portable, self.output)
        run.assert_not_called()
        self.assertEqual(existing.read_bytes(), b"keep existing candidate")

    def test_user_state_and_links_cannot_be_added_with_a_consistent_manifest(self):
        with self.native_context(), mock.patch.object(DMG.subprocess, "run") as run:
            for name in (".pcl-rust/settings.json", "Minecraft/saves/private.dat", ".pcl-rust"):
                with self.subTest(name=name):
                    self.files[name] = (b"private user data", 0o600)
                    self.write_archive()
                    with self.assertRaisesRegex(ValueError, "user state"):
                        DMG.build(self.portable, self.output)
                    del self.files[name]
            self.write_archive(link=True)
            with self.assertRaisesRegex(ValueError, "link or special"):
                DMG.build(self.portable, self.output)
            self.files["private-notes.txt"] = (b"not a production resource", 0o644)
            self.write_archive()
            with self.assertRaisesRegex(ValueError, "non-production"):
                DMG.build(self.portable, self.output)
        run.assert_not_called()

    def test_corrupt_archive_or_member_stops_before_image_creation(self):
        archive = self.portable / "synthetic.tar.gz"
        archive.write_bytes(archive.read_bytes() + b"changed after hashing")
        with self.native_context(), mock.patch.object(DMG.subprocess, "run") as run, self.assertRaisesRegex(ValueError, "hash mismatch"):
            DMG.build(self.portable, self.output)
        run.assert_not_called()
        self.write_archive()
        manifest = self.portable / "package-manifest.json"
        manifest.write_bytes(manifest.read_bytes() + b" ")
        with self.native_context(), mock.patch.object(DMG.subprocess, "run") as run, self.assertRaisesRegex(ValueError, "manifests differ"):
            DMG.build(self.portable, self.output)
        run.assert_not_called()

    @unittest.skipUnless(platform.system() == "Darwin", "hdiutil requires a native macOS host")
    def test_native_hdiutil_builds_and_checks_an_actual_readonly_image(self):
        # Real hdiutil, inert synthetic contents. No mounting or executing this
        # fixture; the distribution workflow separately runs real candidates.
        report = DMG.build(self.portable, self.output)
        image = self.output / report["image"]
        result = subprocess.run(["/usr/bin/hdiutil", "imageinfo", "-plist", str(image)],
                                capture_output=True, check=True, timeout=60)
        import plistlib
        self.assertEqual(plistlib.loads(result.stdout)["Format"], "UDZO")


@unittest.skipUnless(os.name == "posix", "Application image fixtures require actual Unix permissions")
class NativeApplicationDiskImageTests(unittest.TestCase):
    def setUp(self):
        work = Path(__file__).resolve().parents[1] / "work"
        work.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="app dmg fixture space & 项目-", dir=work)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.portable, self.app_directory, self.output = self.root / "portable", self.root / "application", self.root / "images"
        self.portable.mkdir()
        self.arch = "aarch64" if platform.machine().lower() in ("aarch64", "arm64") else "x86_64"
        header = bytearray(32)
        header[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", header, 4, 0x100000C if self.arch == "aarch64" else 0x1000007)
        struct.pack_into("<I", header, 12, 2)
        self.app = DMG.load("pcl_app_dmg_fixture", Path(__file__).with_name("app-native.py"))
        self.files = {name: (b"inert production fixture\n", 0o644)
                      for name in (*DMG.PACKAGE.REQUIRED, *DMG.LEGAL.DOCUMENTS)}
        self.files.update({"pcl-desktop": (bytes(header), 0o755), "pcl-cli": (bytes(header), 0o755),
                           "start-native.sh": (b"#!/bin/sh\n# inert fixture, never executed\n", 0o755),
                           "Start PCL RH.command": (b"#!/bin/sh\n# inert fixture, never executed\n", 0o755),
                           "README-native.txt": (b"portable entry instructions\n", 0o644),
                           "assets/pcl-rh.png": ((DMG.ROOT.parent / "assets/pcl-rh.png").read_bytes(), 0o644),
                           "pcl-rh-license-notices.zip": (b"inert notices fixture\n", 0o644),
                           "experimental/runtime/package.json": (b'{"dependencies":{"@earendil-works/pi-coding-agent":"1.0.4"}}', 0o644),
                           "experimental/runtime/package-lock.json": (b'{"packages":{"":{"dependencies":{"@earendil-works/pi-coding-agent":"1.0.4"}}}}', 0o644),
                           "experimental/runtime/node_modules/@earendil-works/pi-coding-agent/package.json": (b'{"version":"1.0.4"}', 0o644),
                           "pcl-rh.json": (b'{"schemaVersion":1,"productId":"dev.pcl-rh.portable"}', 0o644)})
        environment = mock.patch.dict(os.environ, {"PCL_NATIVE_SOURCE_COMMIT": COMMIT})
        environment.start()
        self.addCleanup(environment.stop)
        NativeDiskImageTests.write_archive(self)
        self.info = {"schema_version": 2, "runtime_layout": "split-v1", "commit": COMMIT}
        with self.native_context(), mock.patch.object(self.app, "split_build_info", return_value=self.info), \
                mock.patch.object(self.app, "native_deployment", return_value="11.0"):
            self.app_report = self.app.build(self.portable, self.app_directory)
        self.contents = self.root / "synthetic image contents"
        self.executed, self.detached = 0, False

    def native_context(self):
        return mock.patch.object(DMG.platform, "system", return_value="Darwin")

    def fake_build_hdiutil(self, command, **kwargs):
        self.assertEqual(command[0], "/usr/bin/hdiutil")
        self.assertTrue(kwargs["check"])
        if command[1] == "create":
            self.assertEqual(kwargs["timeout"], 600)
            self.assertIn("-nospotlight", command)
            self.assertIn("-puppetstrings", command)
            stage = Path(command[command.index("-srcfolder") + 1])
            self.assertEqual({path.name for path in stage.iterdir()}, {self.app.BUNDLE, "README-install.txt", "Applications"})
            self.assertTrue((stage / "Applications").is_symlink())
            self.assertEqual(os.readlink(stage / "Applications"), "/Applications")
            self.assertEqual((stage / "README-install.txt").read_bytes(), DMG.APP_INSTRUCTIONS)
            self.app.verify_bundle(stage / self.app.BUNDLE, self.app_report, self.portable / "package-manifest.json")
            shutil.copytree(stage, self.contents, symlinks=True)
            Path(command[command.index("-o") + 1]).write_bytes(b"synthetic app disk image; never mounted")
        else:
            self.assertEqual(command[1], "verify")
            self.assertEqual(kwargs["timeout"], 120)

    def build_image(self):
        with self.native_context(), mock.patch.object(DMG.subprocess, "run", side_effect=self.fake_build_hdiutil):
            return DMG.build(self.portable, self.output, self.app_directory)

    def verify_image(self, *, bad_link=False, execution_failure=False):
        mounted = {"path": None, "active": False}

        def fake_hdiutil(command, **kwargs):
            self.assertEqual(command[0], "/usr/bin/hdiutil")
            if command[1] == "attach":
                self.assertIn("-readonly", command)
                self.assertIn("-noautoopen", command)
                mount = Path(command[command.index("-mountpoint") + 1])
                shutil.copytree(self.contents, mount, symlinks=True, dirs_exist_ok=True)
                if bad_link:
                    (mount / "Applications").unlink()
                    (mount / "Applications").symlink_to("/unexpected-directory", target_is_directory=True)
                mounted.update(path=mount, active=True)
                return subprocess.CompletedProcess(command, 0, plistlib.dumps({
                    "system-entities": [{"mount-point": str(mount)}]}), b"")
            self.assertEqual(command[1], "detach")
            self.assertEqual(Path(command[2]), mounted["path"])
            mounted["active"], self.detached = False, True
            return subprocess.CompletedProcess(command, 0, b"", b"")

        def fake_execution(bundle, scratch, report, manifest):
            self.executed += 1
            self.assertTrue(bundle.is_relative_to(scratch))
            self.assertFalse(bundle.is_relative_to(mounted["path"]))
            self.app.verify_bundle(bundle, report, self.portable / "package-manifest.json")
            if execution_failure:
                raise ValueError("synthetic LaunchServices failure")
            return {"launch_services_entry_checked": True, "read_only_bundle_unchanged": True,
                    "existing_user_data_preserved": True, "desktop_build": self.info}

        original_load = VERIFY_DMG.DMG.load

        def lazy_load(name, path):
            if name == "pcl_dmg_app_execution":
                return types.SimpleNamespace(require_ci=lambda: None, verify_execution=fake_execution,
                    installed=lambda bundle, scratch, env, data: {"installed": True, "plugin_host_checked": True,
                        "plugin_host_retired": True, "data_root": str(data)})
            return original_load(name, path)

        with self.native_context(), mock.patch.object(VERIFY_DMG.DMG, "load", side_effect=lazy_load), \
                mock.patch.object(VERIFY_DMG.subprocess, "run", side_effect=fake_hdiutil), \
                mock.patch.object(VERIFY_DMG.os.path, "ismount", side_effect=lambda path: mounted["active"] and Path(path) == mounted["path"]), \
                mock.patch.object(VERIFY_DMG.os, "statvfs", return_value=types.SimpleNamespace(f_flag=os.ST_RDONLY)):
            return VERIFY_DMG.verify(self.output, self.portable, self.app_directory)

    def test_app_image_contains_only_one_bundle_readme_and_known_applications_link(self):
        report = self.build_image()
        self.assertEqual(report["distribution"], "macos-app-dmg")
        self.assertTrue(report["image"].endswith("-app.dmg"))
        self.assertEqual(report["app_manifest_sha256"], DMG.sha256(self.app_directory / "app-manifest.json"))
        self.assertEqual(report["app_archive_sha256"], self.app_report["sha256"])
        self.assertEqual(report["source_archive_sha256"], self.app_report["source_archive_sha256"])
        self.assertTrue(report["volume_read_only"])
        self.assertTrue(report["read_only_resources"])
        self.assertFalse(report["requires_writable_copy"])
        self.assertFalse(report["signed"])
        self.assertFalse(report["notarized"])
        self.assertFalse(report["user_data_included"])

    def test_app_verification_uses_owned_copy_and_detaches_after_launchservices(self):
        self.build_image()
        result = self.verify_image()
        self.assertEqual(self.executed, 1)
        self.assertTrue(self.detached)
        self.assertTrue(result["app_launch_services"])
        self.assertTrue(result["read_only_bundle"])
        self.assertTrue(result["data_preserved"])
        self.assertFalse(result["finder_or_gui_started"])
        self.assertFalse(result["gatekeeper_assessed"])

    def test_wrong_applications_link_is_rejected_without_following_and_still_detaches(self):
        self.build_image()
        with self.assertRaisesRegex(ValueError, "Applications link"):
            self.verify_image(bad_link=True)
        self.assertEqual(self.executed, 0)
        self.assertTrue(self.detached)

    def test_failed_copied_app_execution_still_detaches_owned_image(self):
        self.build_image()
        with self.assertRaisesRegex(ValueError, "LaunchServices failure"):
            self.verify_image(execution_failure=True)
        self.assertTrue(self.detached)

    def test_app_manifest_drift_is_rejected_before_mounting(self):
        self.build_image()
        metadata = self.app_directory / "app-manifest.json"
        metadata.write_bytes(metadata.read_bytes() + b" ")
        with self.assertRaisesRegex(ValueError, "different application manifest"):
            self.verify_image()
        self.assertEqual(self.executed, 0)
        self.assertFalse(self.detached)

    def test_app_archive_and_source_manifest_drift_stop_before_hdiutil(self):
        archive = self.app_directory / self.app_report["archive"]
        original = archive.read_bytes()
        archive.write_bytes(original + b"changed")
        with self.native_context(), mock.patch.object(DMG.subprocess, "run") as run, \
                self.assertRaisesRegex(ValueError, "Application archive checksum"):
            DMG.build(self.portable, self.output, self.app_directory)
        run.assert_not_called()
        archive.write_bytes(original)
        external = self.portable / "package-manifest.json"
        external.write_bytes(external.read_bytes() + b" ")
        with self.native_context(), mock.patch.object(DMG.subprocess, "run") as run, \
                self.assertRaisesRegex(ValueError, "Source package manifest differs"):
            DMG.build(self.portable, self.output, self.app_directory)
        run.assert_not_called()
        self.assertEqual(list(self.output.iterdir()), [])

    @unittest.skipUnless(platform.system() == "Darwin", "Actual hdiutil app-image construction requires macOS")
    def test_native_hdiutil_builds_an_actual_application_image_without_mounting_or_execution(self):
        # The only symlink is the intended /Applications delivery link. This
        # native check also proves hdiutil does not recursively copy its target.
        report = DMG.build(self.portable, self.output, self.app_directory)
        self.assertLess((self.output / report["image"]).stat().st_size, 10 * 1024**2)
        result = subprocess.run(["/usr/bin/hdiutil", "imageinfo", "-plist", str(self.output / report["image"])],
                                capture_output=True, check=True, timeout=60)
        self.assertEqual(plistlib.loads(result.stdout)["Format"], "UDZO")


class ApplicationImageGuardTests(unittest.TestCase):
    def test_app_image_mounting_is_not_available_on_a_local_machine(self):
        with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), \
                mock.patch.object(VERIFY_DMG.subprocess, "run") as run, \
                self.assertRaisesRegex(ValueError, "disposable"):
            VERIFY_DMG.verify(Path("/unused"), Path("/unused"), Path("/unused"))
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
