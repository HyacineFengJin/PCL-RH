"""Exercise real Debian archive construction without installing or running fixtures."""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("pcl_linux_package_test", Path(__file__).with_name("linux-package-native.py"))
LINUX = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LINUX)
SPEC = importlib.util.spec_from_file_location("pcl_linux_verify_test", Path(__file__).parent / "ci/verify-linux-package.py")
CI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CI)
COMMIT = "a" * 40


@unittest.skipUnless(platform.system() == "Linux" and platform.machine().lower() == "x86_64"
                     and shutil.which("dpkg-deb") and shutil.which("dpkg-shlibdeps"),
                     "Debian archive tests require native Linux x86_64 and dpkg-dev; no package is installed")
class LinuxPackageTests(unittest.TestCase):
    def setUp(self):
        work = Path(__file__).resolve().parents[1] / "work"
        work.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="deb fixture space & 项目-", dir=work)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.portable = self.root / "portable"
        self.portable.mkdir()
        self.output = self.root / "packages"
        header = bytearray(64)
        header[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<H", header, 16, 3)
        struct.pack_into("<H", header, 18, 62)
        self.files = {name: (b"inert production resource\n", 0o644)
                      for name in (*LINUX.PACKAGE.REQUIRED, *LINUX.LEGAL.DOCUMENTS)}
        self.files.update({"pcl-desktop": (bytes(header), 0o755), "pcl-cli": (bytes(header), 0o755),
                           "start-native.sh": (b"#!/bin/sh\n# inert fixture, never executed\n", 0o755),
                           "README-native.txt": (b"inert portable readme\n", 0o644),
                           "LICENSE": (b"MIT license fixture\n", 0o644),
                           "pcl-rh-license-notices.zip": (b"inert license fixture\n", 0o644),
                           "experimental/runtime/package.json": (b'{"dependencies":{"@earendil-works/pi-coding-agent":"1.0.4"}}', 0o644),
                           "experimental/runtime/package-lock.json": (b'{"packages":{"":{"dependencies":{"@earendil-works/pi-coding-agent":"1.0.4"}}}}', 0o644),
                           "experimental/runtime/node_modules/@earendil-works/pi-coding-agent/package.json": (b'{"version":"1.0.4"}', 0o644),
                           "pcl-rh.json": (b'{"schemaVersion":1,"productId":"dev.pcl-rh.portable"}', 0o644)})
        self.version, self.profile = "0.2.0", "release"
        self.environment = mock.patch.dict(os.environ, {"PCL_NATIVE_SOURCE_COMMIT": COMMIT})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.info = {"schema_version": 2, "runtime_layout": "split-v1"}
        self.write_archive()

    def write_archive(self, *, link=False):
        manifest = {"schema_version": 1, "product_id": "dev.pcl-rh.portable", "layout": "flat",
                    "platform": "linux", "arch": "x86_64", "profile": self.profile, "version": self.version,
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
        archive.with_name(archive.name + ".sha256").write_text(LINUX.sha256(archive) + "  " + archive.name + "\n")

    def build(self):
        # The synthetic ELF headers are validated as archive contents only.
        # Actual binary execution/dependency extraction belongs to native CI.
        with mock.patch.object(LINUX, "installed_build_info", return_value=self.info), \
                mock.patch.object(LINUX, "shared_library_dependencies", return_value="libc6 (>= 2.39), libwebkit2gtk-4.1-0"):
            return LINUX.build(self.portable, self.output)

    def test_actual_debian_archive_has_exact_readonly_program_files_and_no_scripts(self):
        report = self.build()
        self.assertEqual(report["debian_version"], "0.2.0-1")
        self.assertEqual(report["commit"], COMMIT)
        self.assertFalse(report["user_data_included"])
        self.assertEqual(report["runtime_layout"], "split-v1")
        archive = CI.checked_archive(self.output, report)
        extracted = self.root / "extracted"
        subprocess.run(["dpkg-deb", "--raw-extract", str(archive), str(extracted)], check=True)
        self.assertEqual((extracted / "usr/bin/pcl-rh").read_bytes(), LINUX.ENTRY)
        self.assertNotIn(b"mkdir", LINUX.ENTRY)
        self.assertNotIn(b"PCL_RH_DATA_ROOT", LINUX.ENTRY)
        self.assertEqual((extracted / "usr/lib/pcl-rh/pcl-desktop").read_bytes(), self.files["pcl-desktop"][0])
        self.assertEqual({item.name for item in (extracted / "DEBIAN").iterdir()}, {"control", "md5sums"})
        self.assertIn("Depends: libc6 (>= 2.39), libwebkit2gtk-4.1-0", (extracted / "DEBIAN/control").read_text())
        self.assertFalse((extracted / "home").exists())
        self.assertFalse((extracted / "var").exists())
        if shutil.which("desktop-file-validate"):
            subprocess.run(["desktop-file-validate", str(extracted / "usr/share/applications/pcl-rh.desktop")], check=True)
        subprocess.run(["sh", "-n", str(extracted / "usr/bin/pcl-rh")], check=True)
        self.assertEqual(list(self.output.glob(".pcl-deb-*")), [])

    def test_real_upgrade_fixture_has_lower_version_and_obsolete_owned_file(self):
        report = self.build()
        archive = CI.checked_archive(self.output, report)
        previous = CI.previous_revision(archive, report, self.root)
        self.assertEqual(subprocess.check_output(["dpkg-deb", "-f", str(previous), "Version"], text=True), "0.2.0-0.ci\n")
        raw = subprocess.check_output(["dpkg-deb", "--fsys-tarfile", str(previous)])
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as source:
            self.assertIn("./usr/lib/pcl-rh/previous-package-fixture.txt", source.getnames())

    def test_restrictive_caller_umask_does_not_make_system_directories_private(self):
        previous = os.umask(0o077)
        try:
            report = self.build()
        finally:
            os.umask(previous)
        CI.checked_archive(self.output, report)

    def test_existing_output_is_never_replaced(self):
        self.output.mkdir()
        existing = self.output / "linux-package-manifest.json"
        existing.write_bytes(b"keep me")
        with self.assertRaisesRegex(ValueError, "replace"):
            self.build()
        self.assertEqual(existing.read_bytes(), b"keep me")

    def test_package_failure_publishes_nothing(self):
        with mock.patch.object(LINUX, "installed_build_info", return_value=self.info), \
                mock.patch.object(LINUX, "shared_library_dependencies", return_value="libc6"), \
                mock.patch.object(LINUX.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "dpkg-deb")), \
                self.assertRaises(subprocess.CalledProcessError):
            LINUX.build(self.portable, self.output)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_user_data_unknown_paths_links_and_unsafe_permissions_are_rejected(self):
        for name, data, mode, error in ((".pcl-rust/settings.json", b"private", 0o644, "user state"),
                                        ("Minecraft/saves/private.dat", b"private", 0o644, "user state"),
                                        ("my-notes.txt", b"private", 0o644, "non-production"),
                                        ("code/private.mjs", b"invalid mode", 0o666, "permissions")):
            with self.subTest(name=name):
                self.files[name] = (data, mode)
                self.write_archive()
                with self.assertRaisesRegex(ValueError, error):
                    self.build()
                del self.files[name]
        self.write_archive(link=True)
        with self.assertRaisesRegex(ValueError, "link or special"):
            self.build()

    def test_archive_or_internal_manifest_drift_is_rejected(self):
        archive = self.portable / "synthetic.tar.gz"
        archive.write_bytes(archive.read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "archive hash"):
            self.build()
        self.write_archive()
        external = self.portable / "package-manifest.json"
        external.write_bytes(external.read_bytes() + b" ")
        with self.assertRaisesRegex(ValueError, "manifests differ"):
            self.build()

    def test_missing_resources_and_debug_builds_are_rejected(self):
        del self.files["code/plugins.json"]
        self.write_archive()
        with self.assertRaisesRegex(ValueError, "required production"):
            self.build()
        self.profile = "debug"
        self.write_archive()
        with self.assertRaisesRegex(ValueError, "optimized release"):
            self.build()

    def test_missing_or_mismatched_node_engine_is_rejected(self):
        engine = "experimental/runtime/node_modules/@earendil-works/pi-coding-agent/package.json"
        self.files[engine] = (b'{"version":"0.0.0"}', 0o644)
        self.write_archive()
        with self.assertRaisesRegex(ValueError, "Node runtime"):
            self.build()
        del self.files[engine]
        self.write_archive()
        with self.assertRaisesRegex(ValueError, "Node runtime"):
            self.build()

    def test_old_flat_executable_is_rejected_before_any_layout_flag(self):
        with mock.patch.object(LINUX.VERIFY, "run", return_value='{"schema_version":1}') as execute:
            with self.assertRaisesRegex(ValueError, "split-v1"):
                LINUX.installed_build_info(self.root, {}, self.root)
        self.assertEqual(execute.call_args.args[0], [str(self.root / "pcl-desktop"), "--build-info"])

    def test_dependency_resolution_cannot_ignore_missing_library_metadata(self):
        root = self.root / "deps"
        (root / "experimental/runtime/node_modules").mkdir(parents=True)
        result = subprocess.CompletedProcess([], 0, "shlibs:Depends=libc6 (>= 2.39)\n", "")
        with mock.patch.object(LINUX.subprocess, "run", return_value=result) as execute:
            self.assertEqual(LINUX.shared_library_dependencies(root, self.root), "libc6 (>= 2.39)")
        command = execute.call_args.args[0]
        self.assertEqual(command[0], "dpkg-shlibdeps")
        self.assertNotIn("--ignore-missing-info", command)
        self.assertTrue(execute.call_args.kwargs["check"])

    def test_dependency_scan_uses_only_native_elf_addons_and_reports_failures(self):
        root = self.root / "native-deps"
        modules = root / "experimental/runtime/node_modules"
        modules.mkdir(parents=True)
        native = bytearray(self.files["pcl-desktop"][0])
        (modules / "native.node").write_bytes(native)
        struct.pack_into("<H", native, 18, 183)
        (modules / "foreign.node").write_bytes(native)
        (modules / "wasi.so").write_bytes(b"\0asm" + bytes(60))
        result = subprocess.CompletedProcess([], 0, "shlibs:Depends=libc6\n", "")
        with mock.patch.object(LINUX.subprocess, "run", return_value=result) as execute:
            self.assertEqual(LINUX.shared_library_dependencies(root, self.root), "libc6")
        command = execute.call_args.args[0]
        self.assertIn("-e" + str(modules / "native.node"), command)
        self.assertNotIn("-e" + str(modules / "foreign.node"), command)
        self.assertNotIn("-e" + str(modules / "wasi.so"), command)
        other = self.root / "failed-deps"
        other.mkdir()
        with mock.patch.object(LINUX.subprocess, "run", side_effect=subprocess.CalledProcessError(
                1, "dpkg-shlibdeps", stderr="missing library metadata")), \
                self.assertRaisesRegex(ValueError, "missing library metadata"):
            LINUX.shared_library_dependencies(root, other)

    def test_changed_debian_archive_or_extra_script_is_rejected(self):
        report = self.build()
        image = self.output / report["package"]
        original = image.read_bytes()
        image.write_bytes(original + b"changed")
        with self.assertRaisesRegex(ValueError, "package hash"):
            CI.checked_archive(self.output, report)
        image.write_bytes(original)
        stage = self.root / "extra-script"
        subprocess.run(["dpkg-deb", "--raw-extract", str(image), str(stage)], check=True)
        (stage / "DEBIAN/postinst").write_text("#!/bin/sh\nexit 0\n")
        (stage / "DEBIAN/postinst").chmod(0o755)
        subprocess.run(["dpkg-deb", "--build", "--root-owner-group", str(stage), str(image)], check=True,
                       stdout=subprocess.PIPE)
        report["sha256"] = LINUX.sha256(image)
        image.with_name(image.name + ".sha256").write_text(report["sha256"] + "  " + image.name + "\n")
        with self.assertRaisesRegex(ValueError, "maintainer scripts"):
            CI.checked_archive(self.output, report)


class LinuxPackagePureTests(unittest.TestCase):
    def test_failed_installation_check_exposes_its_stdout_error_json(self):
        failed = subprocess.CompletedProcess([], 1, '{"error":"fixture diagnostic failure"}\n', "wrapper warning\n")
        with mock.patch.object(CI.subprocess, "run", return_value=failed), \
                self.assertRaisesRegex(ValueError, "fixture diagnostic failure"):
            CI.app(["unused-fixture"], Path("/unused"), {})

    def test_debian_prereleases_and_revisions_sort_correctly(self):
        self.assertEqual(LINUX.debian_version("1.2.3-beta.1+build.2", "3"), "1.2.3~beta.1+build.2-3")
        self.assertEqual(LINUX.debian_version("1.2.3+build-name", "1"), "1.2.3+build-name-1")
        for version, revision in (("1.2.3", "x"), ("1.2.3", "1\npostinst"), ("1.2", "1")):
            with self.subTest(version=version, revision=revision), self.assertRaises(ValueError):
                LINUX.debian_version(version, revision)

    def test_local_machine_cannot_reach_system_installation(self):
        with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), \
                mock.patch.object(CI.subprocess, "run") as execute, \
                self.assertRaisesRegex(ValueError, "disposable"):
            CI.run(Path("/unused-fixture"), Path("/unused-fixture"))
        execute.assert_not_called()

    def test_self_hosted_runner_cannot_reach_system_installation(self):
        with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "self-hosted"}), \
                mock.patch.object(CI.subprocess, "run") as execute, \
                self.assertRaisesRegex(ValueError, "disposable"):
            CI.run(Path("/unused-fixture"), Path("/unused-fixture"))
        execute.assert_not_called()

    def test_unsafe_debian_tar_names_are_rejected(self):
        for name in ("../root", "/etc/passwd", "./usr/../etc", "usr\\bin\\pcl-rh"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                CI.safe_tar_name(name)


if __name__ == "__main__":
    unittest.main()
