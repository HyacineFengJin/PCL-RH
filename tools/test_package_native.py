"""Synthetic archives and real temporary Git/filesystem inputs; no launcher runs."""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock
import zipfile

SPEC = importlib.util.spec_from_file_location("pcl_package_native", Path(__file__).with_name("package-native.py"))
assert SPEC is not None and SPEC.loader is not None
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)
LICENSE_SOURCE = Path(__file__).parent / "licenses/release.py"


def binary_header(system: str, arch: str) -> bytes:
    header = bytearray(512)
    arm = arch == "aarch64"
    if system == "linux":
        header[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HH", header, 16, 3, 183 if arm else 62)
    elif system == "windows":
        header[:2] = b"MZ"
        struct.pack_into("<I", header, 60, 128)
        header[128:132] = b"PE\0\0"
        struct.pack_into("<H", header, 132, 0xAA64 if arm else 0x8664)
        struct.pack_into("<H", header, 150, 2)
        struct.pack_into("<H", header, 152, 0x20B)
    else:
        header[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", header, 4, 0x100000C if arm else 0x1000007)
        struct.pack_into("<I", header, 12, 2)
    return bytes(header)


class NativePackageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="native package 测试 ")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / "source"
        self.root.mkdir()
        self.binaries = self.base / "build output"
        self.binaries.mkdir()
        self.output = self.base / "artifacts"
        self.system, self.arch = PACKAGE.native_host()
        self.git("init", "-q")
        for name in PACKAGE.REQUIRED:
            self.write(name, b"synthetic production resource\n")
        self.write("pcl-rh.json", json.dumps({"schemaVersion": 1, "productId": "dev.pcl-rh.portable"}).encode())
        dependencies = {"@earendil-works/pi-coding-agent": "1.0.4"}
        self.write("experimental/runtime/package.json", json.dumps({"dependencies": dependencies}).encode())
        self.write("experimental/runtime/package-lock.json", json.dumps({
            "lockfileVersion": 3, "packages": {"": {"dependencies": dependencies}}}).encode())
        self.write("tools/licenses/release.py", LICENSE_SOURCE.read_bytes())
        legal_spec = importlib.util.spec_from_file_location("fixture_license", LICENSE_SOURCE)
        legal = importlib.util.module_from_spec(legal_spec)
        legal_spec.loader.exec_module(legal)
        for name in legal.DOCUMENTS:
            self.write(name, ("Synthetic attribution " + name + "\n").encode())
        self.write("THIRDPARTY.generated.json", b'{"schema_version":1,"inputs":[],"issues":[]}')
        self.write("apps/desktop/src/assets/game-icons/SOURCES.json", b'{"assets":[]}')
        for name in ("start-native.sh", "start-native.ps1"):
            self.write(name, b"synthetic startup; never executed\n")
        self.write("code/tests/excluded.test.mjs", b"DO NOT SHIP SOURCE TEST")
        self.write("experimental/extensions/compat/excluded.mjs", b"DO NOT SHIP COMPAT TOOL")
        self.write("experimental/runtime/vendor/maker/tests/excluded.py", b"DO NOT SHIP VENDOR TEST")
        self.write(".github/workflows/private.yml", b"DO NOT SHIP WORKFLOW")
        self.write(".gitignore", b"private-secret\nexperimental/runtime/node_modules/\n")
        self.commit()
        self.write("private-secret", b"IGNORED PERSONAL SECRET")
        self.write("untracked-private.json", b"UNTRACKED PERSONAL SECRET")
        # Dirty tracked source must not replace its committed bytes in a package.
        self.write("experimental/host.mjs", b"DIRTY PERSONAL HOST SOURCE")
        self.nodes = self.root / "experimental/runtime/node_modules"
        self.node("@earendil-works/pi-coding-agent/package.json", b'{"name":"@earendil-works/pi-coding-agent","version":"1.0.4"}')
        self.node("@earendil-works/pi-coding-agent/index.mjs", b"export const synthetic = true;\n")
        self.node("runtime-package/compat/runtime.mjs", b"legitimate runtime compatibility module")
        self.node(".bin/private-executable", b"DO NOT SHIP NPM BIN")
        self.node(".npmrc", b"DO NOT SHIP NPM TOKEN")
        self.node(".env.local", b"DO NOT SHIP ENV")
        extension = ".exe" if self.system == "windows" else ""
        for name in ("pcl-desktop", "pcl-cli"):
            path = self.binaries / (name + extension)
            path.write_bytes(binary_header(self.system, self.arch) + b"synthetic never executed")
            path.chmod(0o755)

    def git(self, *args: str) -> bytes:
        return subprocess.run(["git", "-C", str(self.root), *args], check=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout

    def commit(self) -> None:
        self.git("add", ".")
        self.git("-c", "user.name=Synthetic fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "synthetic package resources")

    def write(self, name: str, data: bytes) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def node(self, name: str, data: bytes) -> Path:
        return self.write("experimental/runtime/node_modules/" + name, data)

    def run_package(self) -> dict:
        return PACKAGE.package(self.root, self.binaries, self.output, "debug")

    def assert_no_scratch(self) -> None:
        self.assertEqual(list(self.output.glob(".pcl-native-package-*")), [])

    def unpack(self, archive: Path) -> Path:
        destination = self.base / "relocated space & 项目"
        destination.mkdir()
        if archive.suffix == ".zip":
            with zipfile.ZipFile(archive) as source:
                self.assertTrue(all(stat.S_ISREG(item.external_attr >> 16) for item in source.infolist()))
                source.extractall(destination)
        else:
            with tarfile.open(archive) as source:
                self.assertTrue(all(item.isfile() for item in source.getmembers()))
                source.extractall(destination, filter="data")
        return destination / "PCL-RH"

    def test_archive_roundtrip_head_only_manifest_and_attribution(self) -> None:
        result = self.run_package()
        archive = self.output / result["archive"]
        extracted = self.unpack(archive)
        raw = (extracted / "package-manifest.json").read_bytes()
        self.assertEqual(raw, (self.output / result["manifest"]).read_bytes())
        manifest = json.loads(raw)
        self.assertEqual(manifest["commit"], self.git("rev-parse", "HEAD").decode().strip())
        self.assertEqual(manifest["commit_scope"], "tracked_resources")
        self.assertIsNone(manifest["binary_source_commit"])
        self.assertEqual(manifest["binary_provenance"], "explicit_native_inputs")
        self.assertEqual(manifest["platform"], self.system)
        self.assertEqual(manifest["arch"], self.arch)
        self.assertEqual(manifest["layout"], "flat")
        self.assertFalse(manifest["signed"])
        expected = set()
        for entry in manifest["files"]:
            path = extracted / entry["path"]
            expected.add(entry["path"])
            self.assertEqual(path.stat().st_size, entry["bytes"])
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), entry["sha256"])
            if self.system != "windows":
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), entry["mode"])
        actual = {path.relative_to(extracted).as_posix() for path in extracted.rglob("*") if path.is_file()}
        self.assertEqual(actual, expected | {"package-manifest.json"})
        self.assertEqual((extracted / "experimental/host.mjs").read_bytes(), b"synthetic production resource\n")
        for excluded in ("private-secret", "untracked-private.json", ".github", "code/tests",
                         "experimental/extensions/compat", "experimental/runtime/vendor/maker/tests",
                         "experimental/runtime/node_modules/.bin", "experimental/runtime/node_modules/.npmrc",
                         "experimental/runtime/node_modules/.env.local", ".pcl-rust", ".pcl-linux"):
            self.assertFalse((extracted / excluded).exists(), excluded)
        self.assertTrue((extracted / "experimental/runtime/node_modules/runtime-package/compat/runtime.mjs").is_file())
        notices = (self.output / result["license"]).read_bytes()
        self.assertEqual(notices, (extracted / "pcl-rh-license-notices.zip").read_bytes())
        with zipfile.ZipFile(self.output / result["license"]) as license_zip:
            self.assertEqual(len(license_zip.namelist()), 20)
            self.assertIn("MANIFEST.json", license_zip.namelist())
        checksum = (self.output / result["sha256"]).read_text().split()
        self.assertEqual(checksum, [hashlib.sha256(archive.read_bytes()).hexdigest(), archive.name])
        self.assertNotIn(str(self.root).encode(), raw)
        self.assert_no_scratch()

    def test_missing_binary_retires_only_owned_stage(self) -> None:
        next(self.binaries.iterdir()).unlink()
        self.output.mkdir()
        sentinel = self.output / "keep-user-file"
        sentinel.write_bytes(b"untouched")
        with self.assertRaises(FileNotFoundError):
            self.run_package()
        self.assertEqual(list(self.output.iterdir()), [sentinel])
        self.assertEqual(sentinel.read_bytes(), b"untouched")
        self.assert_no_scratch()

    def test_missing_marker_or_lock_is_not_satisfied_by_worktree_copy(self) -> None:
        for name in ("pcl-rh.json", "experimental/runtime/package-lock.json"):
            with self.subTest(name=name):
                old = (self.root / name).read_bytes()
                self.git("rm", "--", name)
                self.commit()
                self.write(name, old)
                with self.assertRaisesRegex(ValueError, "Missing tracked HEAD resource"):
                    self.run_package()
                self.git("add", "--", name)
                self.commit()

    def test_existing_output_is_not_replaced(self) -> None:
        result = self.run_package()
        archive = self.output / result["archive"]
        before = archive.read_bytes()
        with self.assertRaisesRegex(ValueError, "Refusing to replace"):
            self.run_package()
        self.assertEqual(archive.read_bytes(), before)
        self.assert_no_scratch()

    def test_wrong_binary_architecture_is_rejected(self) -> None:
        path = next(self.binaries.iterdir())
        path.write_bytes(binary_header(self.system, "x86_64" if self.arch == "aarch64" else "aarch64"))
        with self.assertRaisesRegex(ValueError, "native"):
            self.run_package()
        self.assert_no_scratch()

    @unittest.skipUnless(os.name == "nt", "CPython Windows path-derived execute bits")
    def test_windows_exe_path_and_handle_have_same_snapshot_despite_synthetic_execute_bits(self) -> None:
        path = self.binaries / "pcl-cli.exe"
        before = path.lstat()
        with path.open("rb") as source:
            held = os.fstat(source.fileno())
            diagnostic = f"synthetic EXE path={before!r}, handle={held!r}"
            self.assertEqual(before.st_mode ^ held.st_mode, 0o111, diagnostic)
            self.assertEqual(PACKAGE.stamp(before), PACKAGE.stamp(held), diagnostic)
        # A real edit still changes the snapshot. Growth guarantees an
        # observable difference without depending on NTFS timestamp timing.
        with path.open("ab") as output:
            output.write(b"real synthetic input edit")
        changed = path.lstat()
        self.assertNotEqual(PACKAGE.stamp(before), PACKAGE.stamp(changed))
        with path.open("rb") as source:
            self.assertEqual(PACKAGE.stamp(changed), PACKAGE.stamp(os.fstat(source.fileno())))

    def test_snapshot_keeps_file_identity_after_same_size_replacement(self) -> None:
        path = self.base / "snapshot-input.bin"
        path.write_bytes(b"old input bytes")
        before = path.lstat()
        replacement = self.base / "snapshot-replacement.bin"
        replacement.write_bytes(b"new input bytes")
        os.utime(replacement, ns=(before.st_atime_ns, before.st_mtime_ns))
        replacement.replace(path)
        after = path.lstat()
        self.assertEqual(before.st_size, after.st_size)
        self.assertEqual(before.st_mtime_ns, after.st_mtime_ns)
        self.assertNotEqual(before.st_ino, after.st_ino)
        self.assertNotEqual(PACKAGE.stamp(before), PACKAGE.stamp(after))

    @unittest.skipIf(os.name == "nt", "Unix execute permissions are real metadata")
    def test_unix_snapshot_keeps_real_execute_permission_changes(self) -> None:
        path = self.base / "snapshot-permissions"
        path.write_bytes(b"synthetic bytes")
        path.chmod(0o644)
        before = path.lstat()
        path.chmod(0o755)
        after = path.lstat()
        self.assertNotEqual(PACKAGE.stamp(before), PACKAGE.stamp(after))

    def test_installed_runtime_version_must_match_commit(self) -> None:
        self.node("@earendil-works/pi-coding-agent/package.json", b'{"version":"999"}')
        with self.assertRaisesRegex(ValueError, "Installed Pi version"):
            self.run_package()
        self.assert_no_scratch()

    @unittest.skipIf(os.name == "nt", "Real Unix symlinks; native Windows junction check below")
    def test_in_tree_file_link_materializes_but_absolute_escape_and_directory_links_fail(self) -> None:
        target = self.node("safe/target.mjs", b"safe runtime target")
        link = self.nodes / "safe/link.mjs"
        link.symlink_to("target.mjs")
        result = self.run_package()
        extracted = self.unpack(self.output / result["archive"])
        restored = extracted / "experimental/runtime/node_modules/safe/link.mjs"
        self.assertFalse(restored.is_symlink())
        self.assertEqual(restored.read_bytes(), target.read_bytes())
        # Test each invalid link against a fresh output, without weakening the
        # archive assertion or claiming that a refused link was copied.
        for kind in ("absolute", "escape", "directory"):
            with self.subTest(kind=kind):
                self.output = self.base / ("invalid-" + kind)
                link.unlink(missing_ok=True)
                if kind == "absolute":
                    link.symlink_to(target)
                elif kind == "escape":
                    outside = self.base / "outside-secret"
                    outside.write_bytes(b"EXTERNAL SECRET")
                    link.symlink_to(os.path.relpath(outside, link.parent))
                else:
                    link.symlink_to(target.parent, target_is_directory=True)
                with self.assertRaises(ValueError):
                    self.run_package()
                self.assert_no_scratch()

    @unittest.skipUnless(os.name == "nt", "Windows junction requires native NTFS")
    def test_windows_directory_junction_is_rejected_without_symlink_privilege(self) -> None:
        target = self.base / "outside private directory"
        target.mkdir()
        (target / "secret").write_bytes(b"EXTERNAL SECRET")
        junction = self.nodes / "junction"
        subprocess.run(["cmd.exe", "/d", "/c", "mklink", "/J", str(junction), str(target)],
                       check=True, capture_output=True)
        self.addCleanup(lambda: os.rmdir(junction) if junction.exists() else None)
        with self.assertRaisesRegex(ValueError, "Linked Node directory"):
            self.run_package()
        self.assert_no_scratch()

    @unittest.skipUnless(os.name == "nt", "Windows file-link permission check requires native Windows")
    def test_windows_file_link_is_materialized_or_reports_native_privilege_unavailable(self) -> None:
        self.node("safe/target.mjs", b"safe runtime target")
        link = self.nodes / "safe/link.mjs"
        try:
            link.symlink_to("target.mjs")
        except OSError as error:
            if error.winerror == 1314:
                self.skipTest("Native ERROR_PRIVILEGE_NOT_HELD (1314): file symlinks unavailable; "
                              "independent real junction rejection still runs")
            raise
        result = self.run_package()
        extracted = self.unpack(self.output / result["archive"])
        restored = extracted / "experimental/runtime/node_modules/safe/link.mjs"
        self.assertFalse(restored.is_symlink())
        self.assertEqual(restored.read_bytes(), b"safe runtime target")

    def test_zip_writer_emits_only_regular_files_and_unix_modes(self) -> None:
        stage = PACKAGE.Stage(self.base / "zip-stage")
        stage.root.mkdir()
        stage.add_bytes("pcl-cli.exe", b"synthetic never executed", 0o755)
        archive = self.base / "synthetic.zip"
        PACKAGE.write_archive(archive, stage, "windows")
        with zipfile.ZipFile(archive) as source:
            entry = source.infolist()[0]
            self.assertEqual(entry.filename, "PCL-RH/pcl-cli.exe")
            self.assertTrue(stat.S_ISREG(entry.external_attr >> 16))
            self.assertEqual(stat.S_IMODE(entry.external_attr >> 16), 0o755)
            self.assertEqual(source.read(entry), b"synthetic never executed")

    def test_all_native_header_formats_are_checked(self) -> None:
        for system, arch in (("linux", "x86_64"), ("windows", "x86_64"),
                             ("macos", "x86_64"), ("macos", "aarch64")):
            with self.subTest(system=system, arch=arch):
                PACKAGE.validate_binary(binary_header(system, arch), system, arch)
                with self.assertRaises(ValueError):
                    PACKAGE.validate_binary(binary_header(system, arch), system,
                                            "x86_64" if arch == "aarch64" else "aarch64")
                with self.assertRaises(ValueError):
                    PACKAGE.validate_binary(b"not an executable", system, arch)

    def test_host_rejects_unsupported_native_platform(self) -> None:
        with mock.patch.object(PACKAGE.platform, "system", return_value="Plan9"):
            with self.assertRaisesRegex(ValueError, "Unsupported native"):
                PACKAGE.native_host()


if __name__ == "__main__":
    unittest.main()
