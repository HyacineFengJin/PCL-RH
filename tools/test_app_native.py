"""Build real .app tar archives from inert Mach-O fixtures; never execute them."""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import plistlib
import stat
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("pcl_app_test", Path(__file__).with_name("app-native.py"))
APP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(APP)
SPEC = importlib.util.spec_from_file_location("pcl_app_ci_test", Path(__file__).parent / "ci/verify-native-app.py")
CI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CI)
COMMIT = "a" * 40


@unittest.skipUnless(os.name == "posix", "Application archive fixtures require actual Unix executable modes")
class MacApplicationTests(unittest.TestCase):
    def setUp(self):
        work = APP.ROOT.parent / "work"
        work.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="app fixture space & 项目-", dir=work)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.portable, self.output = self.root / "portable", self.root / "application"
        self.portable.mkdir()
        self.arch = "aarch64" if platform.machine().lower() in ("aarch64", "arm64") else "x86_64"
        header = bytearray(32)
        header[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", header, 4, 0x100000C if self.arch == "aarch64" else 0x1000007)
        struct.pack_into("<I", header, 12, 2)
        self.png = (APP.ROOT.parent / "assets/pcl-rh.png").read_bytes()
        self.files = {name: (b"inert production resource\n", 0o644)
                      for name in (*APP.PACKAGE.REQUIRED, *APP.LEGAL.DOCUMENTS)}
        self.files.update({"pcl-desktop": (bytes(header), 0o755), "pcl-cli": (bytes(header), 0o755),
                           "start-native.sh": (b"#!/bin/sh\n# inert, never executed\n", 0o755),
                           "Start PCL RH.command": (b"#!/bin/sh\n# inert, never executed\n", 0o755),
                           "README-native.txt": (b"portable entry instructions\n", 0o644),
                           "assets/pcl-rh.png": (self.png, 0o644),
                           "pcl-rh-license-notices.zip": (b"inert notices fixture\n", 0o644),
                           "experimental/runtime/package.json": (b'{"dependencies":{"@earendil-works/pi-coding-agent":"1.0.4"}}', 0o644),
                           "experimental/runtime/package-lock.json": (b'{"packages":{"":{"dependencies":{"@earendil-works/pi-coding-agent":"1.0.4"}}}}', 0o644),
                           "experimental/runtime/node_modules/@earendil-works/pi-coding-agent/package.json": (b'{"version":"1.0.4"}', 0o644),
                           "pcl-rh.json": (b'{"schemaVersion":1,"productId":"dev.pcl-rh.portable"}', 0o644)})
        self.info = {"schema_version": 2, "runtime_layout": "split-v1", "product_id": "dev.pcl-rh.desktop",
                     "version": "0.2.0", "commit": COMMIT, "profile": "release", "target": self.arch + "-apple-darwin",
                     "target_os": "macos", "target_arch": self.arch, "debug_assertions": False,
                     "custom_protocol": True, "curseforge_embedded": False}
        self.environment = mock.patch.dict(os.environ, {"PCL_NATIVE_SOURCE_COMMIT": COMMIT})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.write_source()

    def write_source(self, *, link=False):
        manifest = {"schema_version": 1, "product_id": "dev.pcl-rh.portable", "layout": "flat",
                    "platform": "macos", "arch": self.arch, "profile": "release", "version": "0.2.0",
                    "cli_version": "0.1.0", "commit": COMMIT, "signed": False, "curseforge_embedded": False,
                    "files": [{"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(), "mode": mode}
                              for name, (data, mode) in self.files.items()]}
        raw = json.dumps(manifest).encode()
        (self.portable / "package-manifest.json").write_bytes(raw)
        archive = self.portable / "source.tar.gz"
        with tarfile.open(archive, "w:gz") as target:
            for name, (data, mode) in {**self.files, "package-manifest.json": (raw, 0o644)}.items():
                entry = tarfile.TarInfo("PCL-RH/" + name)
                entry.mode, entry.size = mode, len(data)
                target.addfile(entry, io.BytesIO(data))
            if link:
                entry = tarfile.TarInfo("PCL-RH/linked")
                entry.type, entry.linkname = tarfile.SYMTYPE, "/outside"
                target.addfile(entry)
        archive.with_name(archive.name + ".sha256").write_text(APP.sha256(archive) + "  " + archive.name + "\n")

    def build(self):
        with mock.patch.object(APP.platform, "system", return_value="Darwin"), \
                mock.patch.object(APP, "split_build_info", return_value=self.info), \
                mock.patch.object(APP, "native_deployment", side_effect=lambda path: "12.0" if path.name == "pcl-desktop" else "10.13"):
            return APP.build(self.portable, self.output)

    def extracted(self, report):
        folder = self.root / "extracted"
        folder.mkdir()
        bundle = APP.unpack(self.output / report["archive"], folder)
        manifest = APP.verify_bundle(bundle, report, self.portable / "package-manifest.json")
        return bundle, manifest

    def test_actual_archive_has_one_bundle_exact_mapping_original_icon_and_closed_plist(self):
        report = self.build()
        bundle, _ = self.extracted(report)
        self.assertEqual(report["archive"], "mac-app.tar.gz")
        self.assertEqual(APP.sha256(self.output / report["archive"]), report["sha256"])
        self.assertEqual(report["minimum_macos_version"], "12.0")
        self.assertFalse(report["signed"])
        self.assertFalse(report["notarized"])
        self.assertFalse(report["user_data_included"])
        self.assertEqual((bundle / "Contents/MacOS/pcl-desktop").read_bytes(), self.files["pcl-desktop"][0])
        for name in APP.BINARIES | APP.EXCLUDED:
            self.assertFalse((bundle / APP.RESOURCE / name).exists())
        self.assertEqual((bundle / APP.SOURCE_MANIFEST).read_bytes(), (self.portable / "package-manifest.json").read_bytes())
        icon = (bundle / APP.ICON).read_bytes()
        self.assertEqual(icon[:4], b"icns")
        self.assertEqual(struct.unpack_from(">I", icon, 4)[0], len(icon))
        self.assertEqual(icon[8:12], b"ic08")
        self.assertEqual(struct.unpack_from(">I", icon, 12)[0], len(self.png) + 8)
        self.assertEqual(icon[16:], self.png)
        plist = plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
        self.assertEqual(plist["CFBundleIdentifier"], "dev.pcl-rh.desktop")
        self.assertEqual(plist["CFBundleExecutable"], "pcl-desktop")
        self.assertEqual(plist["CFBundlePackageType"], "APPL")
        self.assertEqual(plist["LSMinimumSystemVersion"], "12.0")
        self.assertEqual(len(plist), 11)
        self.assertEqual(len(report["generated_files"]), 3)
        self.assertEqual(len(report["source_mapping"]), len(self.files) - len(APP.EXCLUDED))
        self.assertEqual(list(self.output.glob(".pcl-app-*")), [])

    def test_readonly_bundle_snapshot_and_restoration_cover_every_directory_and_file(self):
        report = self.build()
        bundle, _ = self.extracted(report)
        before = CI.snapshot(bundle)
        CI.readonly(bundle)
        readonly = CI.snapshot(bundle)
        self.assertTrue(all(not entry["mode"] & 0o222 for entry in readonly.values()))
        self.assertEqual({name: entry["sha256"] for name, entry in before.items()},
                         {name: entry["sha256"] for name, entry in readonly.items()})
        CI.restore_permissions(bundle, report)
        self.assertEqual(CI.snapshot(bundle), before)
        APP.verify_bundle(bundle, report)

    def test_ci_protocol_uses_automatic_bundle_resources_and_two_private_data_roots(self):
        report = self.build()
        bundle, manifest = self.extracted(report)
        calls = []

        def diagnostic(command, scratch, environment):
            self.assertNotIn("PCL_RH_RESOURCE_ROOT", environment)
            self.assertTrue(all(not entry["mode"] & 0o222 for entry in CI.snapshot(bundle).values()))
            calls.append(command[-1])
            if command[-1] == "--build-info":
                self.assertNotIn("PCL_RH_DATA_ROOT", environment)
                return json.dumps(self.info)
            if command[-1] == "--installation-check":
                data = Path(environment.get("PCL_RH_DATA_ROOT", str(Path(environment["HOME"]) / "Library/Application Support/PCL RH")))
                data.mkdir(mode=0o700, parents=True, exist_ok=True)
                return json.dumps({"schema_version": 1, "ok": True, "installed": True,
                                   "plugin_host_checked": True, "plugin_host_retired": True,
                                   "resource_root": str(bundle / APP.RESOURCE), "data_root": str(data),
                                   "game_root": str(data / "Minecraft/.minecraft"), "desktop_or_game_started": False})
            if command[-1] == "--version":
                return "pcl-cli 0.1.0\n"
            self.assertEqual(command[-1], "list")
            return "[]"

        def opened(command, **kwargs):
            self.assertEqual(command[:5], ["/usr/bin/open", "-n", "-W", "-g", "-j"])
            self.assertEqual(command[command.index("-a") + 1], str(bundle))
            self.assertEqual(command[-2:], ["--args", "--installation-check"])
            self.assertEqual(kwargs["timeout"], 60)
            self.assertNotIn("PCL_RH_RESOURCE_ROOT", kwargs["env"])
            self.assertIn("HOME=" + kwargs["env"]["HOME"], command)
            self.assertIn("PCL_RH_DATA_ROOT=" + kwargs["env"]["PCL_RH_DATA_ROOT"], command)
            output = Path(command[command.index("--stdout") + 1])
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            output.write_text(diagnostic(["pcl-desktop", "--installation-check"], kwargs["cwd"], kwargs["env"]))
            return subprocess.CompletedProcess(command, 0, "", "")

        with mock.patch.object(CI, "require_ci"), mock.patch.object(CI, "run", side_effect=diagnostic), \
                mock.patch.object(CI.subprocess, "run", side_effect=opened), \
                mock.patch.object(CI.APP.platform, "system", return_value="Darwin"), \
                mock.patch.object(CI.APP, "native_deployment", side_effect=lambda path: report["binary_deployment_targets"][path.name]):
            result = CI.verify_execution(bundle, self.root, report, manifest)
        self.assertEqual(calls, ["--build-info", "--installation-check", "--installation-check",
                                 "--installation-check", "--installation-check", "--version", "list"])
        self.assertTrue(result["read_only_bundle_unchanged"])
        self.assertTrue(result["existing_user_data_preserved"])
        self.assertTrue(result["launch_services_entry_checked"])
        APP.verify_bundle(bundle, report)

    def test_corrupt_resource_mapping_or_generated_metadata_is_rejected(self):
        report = self.build()
        bundle, _ = self.extracted(report)
        source = bundle / APP.RESOURCE / "code/plugins.json"
        source.write_bytes(b"changed resource")
        with self.assertRaisesRegex(ValueError, "member changed"):
            APP.verify_bundle(bundle, report)
        source.write_bytes(self.files["code/plugins.json"][0])
        report["source_mapping"][0]["destination"] = "Contents/MacOS/other"
        with self.assertRaisesRegex(ValueError, "mapping differs"):
            APP.verify_bundle(bundle, report)

    def test_new_plist_key_and_unrecorded_file_are_rejected(self):
        report = self.build()
        bundle, _ = self.extracted(report)
        plist = bundle / "Contents/Info.plist"
        original = plist.read_bytes()
        values = plistlib.loads(original)
        values["Unexpected"] = True
        plist.write_bytes(plistlib.dumps(values))
        with self.assertRaisesRegex(ValueError, "member changed"):
            APP.verify_bundle(bundle, report)
        plist.write_bytes(original)
        (bundle / "private-notes.txt").write_text("must not be distributed")
        with self.assertRaisesRegex(ValueError, "unrecorded"):
            APP.verify_bundle(bundle, report)

    def test_private_source_and_links_are_rejected_before_any_binary_execution(self):
        for name in (".pcl-rust/accounts.json", "Minecraft/saves/private.dat", "private-notes.txt"):
            self.files[name] = (b"private", 0o644)
            self.write_source()
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "user state|non-production"):
                self.build()
            del self.files[name]
        self.write_source(link=True)
        with self.assertRaisesRegex(ValueError, "link or special"):
            self.build()

    def test_old_executable_receives_only_the_existing_build_info_flag(self):
        with mock.patch.object(APP.VERIFY, "run", return_value='{"schema_version":1}') as execute, \
                self.assertRaisesRegex(ValueError, "split-v1"):
            APP.split_build_info(self.root, {}, self.root)
        self.assertEqual(execute.call_args.args[0], [str(self.root / "pcl-desktop"), "--build-info"])

    def test_existing_output_and_source_manifest_drift_are_rejected(self):
        self.output.mkdir()
        existing = self.output / "app-manifest.json"
        existing.write_bytes(b"keep existing")
        with self.assertRaisesRegex(ValueError, "replace"):
            self.build()
        self.assertEqual(existing.read_bytes(), b"keep existing")
        existing.unlink()
        external = self.portable / "package-manifest.json"
        external.write_bytes(external.read_bytes() + b" ")
        with self.assertRaisesRegex(ValueError, "manifests differ"):
            self.build()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_generation_failure_publishes_no_partial_archive(self):
        with mock.patch.object(APP.platform, "system", return_value="Darwin"), \
                mock.patch.object(APP, "split_build_info", return_value=self.info), \
                mock.patch.object(APP, "native_deployment", side_effect=ValueError("missing deployment")), \
                self.assertRaisesRegex(ValueError, "deployment"):
            APP.build(self.portable, self.output)
        self.assertEqual(list(self.output.iterdir()), [])

    @unittest.skipUnless(platform.system() == "Darwin", "Real otool/icon parser checks require native macOS")
    def test_native_otool_and_icon_parser_inspect_real_files_without_execution(self):
        self.assertGreaterEqual(APP.os_version(APP.native_deployment(Path("/usr/bin/true"))), (10, 0, 0))
        icon = self.root / "source.icns"
        icon.write_bytes(APP.icon_bytes(self.png))
        result = subprocess.run(["/usr/bin/sips", "--getProperty", "format", str(icon)],
                                capture_output=True, text=True, check=True, timeout=20)
        self.assertIn("format: icns", result.stdout)


class MacApplicationPureTests(unittest.TestCase):
    def test_open_success_without_application_json_cannot_pass(self):
        work = APP.ROOT.parent / "work"
        work.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="empty LaunchServices output-", dir=work) as temporary:
            scratch = Path(temporary)
            with mock.patch.object(CI.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")), \
                    self.assertRaisesRegex(ValueError, "without an actual application diagnostic"):
                CI.launch_services(scratch / APP.BUNDLE, scratch, {"HOME": str(scratch)}, scratch / "data", {})

    def test_modern_and_legacy_macos_deployment_commands(self):
        modern = "file:\nLoad command 8\n      cmd LC_BUILD_VERSION\n  cmdsize 32\n platform 1\n    minos 13.1\n      sdk 15.2\n"
        legacy = "file:\nLoad command 7\n      cmd LC_VERSION_MIN_MACOSX\n  cmdsize 16\n  version 10.13\n      sdk 13.3\n"
        self.assertEqual(APP.deployment_version(modern), "13.1")
        self.assertEqual(APP.deployment_version(legacy), "10.13")
        self.assertEqual(APP.deployment_version(modern.replace("platform 1", "platform MACOS")), "13.1")
        for raw in ("no deployment", modern.replace("platform 1", "platform 2"), modern.replace("13.1", "relative")):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                APP.deployment_version(raw)

    def test_plist_uses_numeric_release_and_an_explicit_minimum(self):
        values = plistlib.loads(APP.info_plist("1.2.3-beta.1+build", "11.0"))
        self.assertEqual(values["CFBundleVersion"], "1.2.3")
        self.assertEqual(values["CFBundleShortVersionString"], "1.2.3")
        self.assertEqual(values["LSMinimumSystemVersion"], "11.0")

    def test_icon_rejects_arbitrary_bytes_and_other_dimensions(self):
        with self.assertRaisesRegex(ValueError, "256x256"):
            APP.icon_bytes(b"not an image")
        png = bytearray((APP.ROOT.parent / "assets/pcl-rh.png").read_bytes())
        struct.pack_into(">I", png, 16, 512)
        with self.assertRaisesRegex(ValueError, "256x256"):
            APP.icon_bytes(bytes(png))

    def test_local_and_self_hosted_execution_are_rejected(self):
        for value in ("false", "true"):
            with self.subTest(actions=value), mock.patch.dict(os.environ, {
                    "GITHUB_ACTIONS": value, "RUNNER_ENVIRONMENT": "self-hosted"}), \
                    mock.patch.object(CI.subprocess, "run") as execute, \
                    self.assertRaisesRegex(ValueError, "disposable"):
                CI.verify(Path("/unused"), Path("/unused"))
            execute.assert_not_called()

    def test_archive_rejects_escaping_link_and_duplicate_members(self):
        work = APP.ROOT.parent / "work"
        work.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="bad app archive-", dir=work) as temporary:
            root = Path(temporary)
            for name, kind in (("../outside", tarfile.REGTYPE), (APP.BUNDLE + "/linked", tarfile.SYMTYPE)):
                archive = root / "bad.tar.gz"
                with tarfile.open(archive, "w:gz") as target:
                    entry = tarfile.TarInfo(name)
                    entry.mode, entry.type, entry.linkname = 0o644, kind, "/outside"
                    target.addfile(entry, io.BytesIO())
                with self.subTest(name=name), self.assertRaises(ValueError):
                    APP.unpack(archive, root / "extract")
            archive = root / "duplicate.tar.gz"
            with tarfile.open(archive, "w:gz") as target:
                for _ in range(2):
                    entry = tarfile.TarInfo(APP.BUNDLE + "/duplicate")
                    entry.mode = 0o644
                    target.addfile(entry, io.BytesIO())
            with self.assertRaisesRegex(ValueError, "Duplicate"):
                APP.unpack(archive, root / "extract")


if __name__ == "__main__":
    unittest.main()
