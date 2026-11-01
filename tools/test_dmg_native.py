"""Real archive boundaries and native image creation; synthetic binaries never run."""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("pcl_dmg_native", Path(__file__).with_name("dmg-native.py"))
DMG = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DMG)
COMMIT = "a" * 40


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


if __name__ == "__main__":
    unittest.main()
