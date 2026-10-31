"""Real temporary-file installation checks; never build or execute a launcher."""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("pcl_build_native", Path(__file__).with_name("build-native.py"))
assert SPEC is not None and SPEC.loader is not None
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class PortableInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / "portable root"
        self.root.mkdir()
        self.outputs = self.base / "build-output"
        self.outputs.mkdir()

    def source(self, name: str = "pcl-desktop.exe") -> Path:
        source = self.outputs / name
        source.write_bytes(b"synthetic executable; never executed")
        return source

    def assert_no_scratch(self) -> None:
        self.assertEqual(list(self.root.rglob(".pcl-build-*")), [])

    def test_windows_flat_install_never_creates_private_state(self) -> None:
        for name in ("pcl-desktop.exe", "pcl-cli.exe"):
            source = self.source(name)
            destination = BUILD.install_binary(source, self.root, windows=True)
            self.assertEqual(destination, self.root / name)
            self.assertEqual(destination.read_bytes(), source.read_bytes())
        self.assertFalse((self.root / ".pcl-rust").exists())
        self.assertFalse((self.root / ".pcl-linux").exists())
        self.assert_no_scratch()

    def test_windows_existing_state_and_nested_executable_are_untouched(self) -> None:
        state = self.root / ".pcl-rust"
        nested = state / "bin"
        nested.mkdir(parents=True)
        settings = state / "settings.json"
        settings.write_bytes(b"existing private settings")
        old = nested / "pcl-desktop.exe"
        old.write_bytes(b"legacy nested binary")
        mode = state.stat().st_mode
        BUILD.install_binary(self.source(), self.root, windows=True)
        self.assertEqual(settings.read_bytes(), b"existing private settings")
        self.assertEqual(old.read_bytes(), b"legacy nested binary")
        self.assertEqual(state.stat().st_mode, mode)
        self.assert_no_scratch()

    def test_successful_flat_replacement_publishes_all_new_bytes(self) -> None:
        destination = self.root / "pcl-desktop.exe"
        destination.write_bytes(b"old binary")
        source = self.source()
        BUILD.install_binary(source, self.root, windows=True)
        self.assertEqual(destination.read_bytes(), source.read_bytes())
        self.assert_no_scratch()

    def test_replacement_failure_keeps_old_binary_and_retires_temporary(self) -> None:
        destination = self.root / "pcl-desktop.exe"
        destination.write_bytes(b"old binary")
        source = self.source()
        with mock.patch.object(BUILD.os, "replace", side_effect=PermissionError("synthetic sharing violation")):
            with self.assertRaises(PermissionError):
                BUILD.install_binary(source, self.root, windows=True)
        self.assertEqual(destination.read_bytes(), b"old binary")
        self.assertEqual(source.read_bytes(), b"synthetic executable; never executed")
        self.assertFalse((self.root / ".pcl-rust").exists())
        self.assert_no_scratch()

    def test_partial_copy_failure_keeps_old_binary_and_retires_temporary(self) -> None:
        destination = self.root / "pcl-desktop.exe"
        destination.write_bytes(b"old binary")

        def interrupted_copy(_source: Path, temporary: Path) -> None:
            temporary.write_bytes(b"partial new bytes")
            raise OSError("synthetic copy failure")

        with mock.patch.object(BUILD.shutil, "copyfile", side_effect=interrupted_copy):
            with self.assertRaises(OSError):
                BUILD.install_binary(self.source(), self.root, windows=True)
        self.assertEqual(destination.read_bytes(), b"old binary")
        self.assert_no_scratch()

    def test_missing_source_keeps_old_binary_and_retires_temporary(self) -> None:
        destination = self.root / "pcl-desktop.exe"
        destination.write_bytes(b"old binary")
        with self.assertRaises(FileNotFoundError):
            BUILD.install_binary(self.outputs / "pcl-desktop.exe", self.root, windows=True)
        self.assertEqual(destination.read_bytes(), b"old binary")
        self.assert_no_scratch()

    def test_unix_install_retains_the_existing_nested_layout(self) -> None:
        source = self.source("pcl-desktop")
        destination = BUILD.install_binary(source, self.root, windows=False)
        self.assertEqual(destination, self.root / ".pcl-rust/bin/pcl-desktop")
        self.assertEqual(destination.read_bytes(), source.read_bytes())
        self.assertFalse((self.root / "pcl-desktop").exists())
        if os.name != "nt":
            self.assertEqual(destination.stat().st_mode & 0o777, 0o755)
        self.assert_no_scratch()


if __name__ == "__main__":
    unittest.main()
