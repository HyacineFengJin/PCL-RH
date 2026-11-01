"""Exercise CI Node preparation with inert files and mocked subprocesses only."""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import types
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("pcl_system_node_test", Path(__file__).parent / "ci/prepare-system-node.py")
NODE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NODE)


@unittest.skipUnless(os.name == "posix", "Source identity fixtures require native Unix metadata and modes")
class SystemNodeTests(unittest.TestCase):
    def setUp(self):
        work = Path(__file__).resolve().parents[1] / "work"
        work.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="node CI fixture 空格 &-", dir=work)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.cache = self.root / "toolcache"
        self.source = self.cache / "node/24.0.0/x64/bin/node"
        self.source.parent.mkdir(parents=True)
        self.source.write_bytes(b"inert setup-node executable, never executed\n")
        self.source.chmod(0o755)
        self.target = self.root / "system/bin/node"
        self.commands = []
        self.hook = None
        for patch in (
            mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                                         "RUNNER_OS": "Linux", "RUNNER_TOOL_CACHE": str(self.cache),
                                         "NODE_OPTIONS": "--require=/untrusted-file"}),
            mock.patch.object(NODE.platform, "system", return_value="Linux"),
            mock.patch.object(NODE.platform, "machine", return_value="x86_64"),
            mock.patch.object(NODE.os, "getuid", return_value=1001),
            mock.patch.object(NODE.os, "geteuid", return_value=1001),
            mock.patch.object(NODE.os, "getgid", return_value=1001),
            mock.patch.object(NODE.os, "getegid", return_value=1001),
            mock.patch.object(NODE.shutil, "which", return_value=str(self.source)),
        ):
            patch.start()
            self.addCleanup(patch.stop)
        self.real_destination = NODE.destination
        self.runner = mock.patch.object(NODE.subprocess, "run", side_effect=self.execute)
        self.runner.start()
        self.addCleanup(self.runner.stop)

    def execute(self, command, **options):
        self.commands.append((command, options))
        if self.hook is not None:
            overridden = self.hook(command)
            if overridden is not None:
                return overridden
        if command[-1] == "--version":
            return subprocess.CompletedProcess(command, 0, b"v24.0.0\n", b"")
        self.assertEqual(command[:2], ["/usr/bin/sudo", "-n"])
        tool, arguments = command[2], command[3:]
        if tool == "/bin/mkdir":
            self.assertEqual(arguments[0], "-p")
            Path(arguments[1]).mkdir(parents=True)
        elif tool == "/usr/bin/install":
            self.assertEqual(arguments[:2], ["-m", "0755"])
            shutil.copyfile(arguments[2], arguments[3])
            Path(arguments[3]).chmod(0o755)
        elif tool == "/bin/mv":
            self.assertIn(arguments[:2], (["-f", "-T"], ["-f", "-h"]))
            os.replace(arguments[2], arguments[3])
        elif tool == "/bin/rm":
            self.assertEqual(arguments[0], "-f")
            Path(arguments[1]).unlink()
        else:
            self.fail("Unexpected privileged command: " + repr(command))
        return subprocess.CompletedProcess(command, 0)

    def prepare(self):
        def fixed_fixture_destination():
            self.real_destination()
            return self.target
        with mock.patch.object(NODE, "destination", side_effect=fixed_fixture_destination):
            return NODE.prepare()

    def test_guards_reject_local_self_hosted_root_and_setuid_before_any_execution(self):
        cases = [("GITHUB_ACTIONS", "false"), ("RUNNER_ENVIRONMENT", "self-hosted"),
                 ("RUNNER_OS", "Windows")]
        for name, value in cases:
            with self.subTest(name=name), mock.patch.dict(os.environ, {name: value}), self.assertRaisesRegex(ValueError, "disposable"):
                NODE.prepare()
        for patch in (mock.patch.object(NODE.os, "geteuid", return_value=0),
                      mock.patch.object(NODE.os, "geteuid", return_value=1002),
                      mock.patch.object(NODE.os, "getegid", return_value=1002),
                      mock.patch.object(NODE.platform, "system", return_value="Windows")):
            with patch, self.assertRaisesRegex(ValueError, "disposable"):
                NODE.prepare()
        self.assertEqual(self.commands, [])

    def test_native_targets_are_fixed_for_linux_and_both_macos_architectures(self):
        for system, runner, machine, expected in (
            ("Linux", "Linux", "x86_64", "/usr/bin/node"),
            ("Linux", "Linux", "aarch64", "/usr/bin/node"),
            ("Darwin", "macOS", "arm64", "/opt/homebrew/bin/node"),
            ("Darwin", "macOS", "x86_64", "/usr/local/bin/node"),
        ):
            with self.subTest(system=system, machine=machine), \
                    mock.patch.object(NODE.platform, "system", return_value=system), \
                    mock.patch.object(NODE.platform, "machine", return_value=machine), \
                    mock.patch.dict(os.environ, {"RUNNER_OS": runner}):
                self.assertEqual(NODE.destination(), Path(expected))
        with mock.patch.object(NODE.platform, "machine", return_value="riscv64"), self.assertRaisesRegex(ValueError, "architecture"):
            NODE.prepare()
        self.assertEqual(self.commands, [])

    def test_cache_boundary_rejects_missing_relative_outside_and_escaping_node_links(self):
        for value in ("", "relative-cache", str(self.root / "missing")):
            with self.subTest(cache=value), mock.patch.dict(os.environ, {"RUNNER_TOOL_CACHE": value}), self.assertRaises(ValueError):
                self.prepare()
        outside = self.root / "toolcache-other/node"
        outside.parent.mkdir()
        outside.write_bytes(b"outside")
        outside.chmod(0o755)
        with mock.patch.object(NODE.shutil, "which", return_value=str(outside)), self.assertRaisesRegex(ValueError, "outside"):
            self.prepare()
        link = self.source.with_name("linked-node")
        link.symlink_to(outside)
        with mock.patch.object(NODE.shutil, "which", return_value=str(link)), self.assertRaisesRegex(ValueError, "outside"):
            self.prepare()
        self.assertFalse(NODE.regular(types.SimpleNamespace(st_mode=stat.S_IFREG | 0o755, st_file_attributes=0x400)))
        self.assertEqual(self.commands, [])

    def test_invalid_or_old_version_never_reaches_sudo(self):
        for version in (b"v22.18.9\n", b"v22.19.0-rc.1\n", b"v24.0.0\nextra", b"v024.0.0\n", b"invalid"):
            self.hook = lambda command, raw=version: subprocess.CompletedProcess(command, 0, raw, b"")
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.prepare()
        self.assertTrue(all(command[-1] == "--version" for command, _ in self.commands))

    def test_source_mutation_after_version_is_rejected_before_privileged_actions(self):
        def mutate(command):
            if command[-1] == "--version":
                self.source.write_bytes(b"changed source\n")
            return None
        self.hook = mutate
        with self.assertRaisesRegex(ValueError, "hash or identity"):
            self.prepare()
        self.assertEqual(len(self.commands), 1)

    def test_success_uses_only_literal_commands_clean_environment_and_same_parent_rename(self):
        report = self.prepare()
        self.assertEqual(self.target.read_bytes(), self.source.read_bytes())
        self.assertEqual(report["version"], "24.0.0")
        self.assertFalse(report["npm_copied"])
        self.assertEqual(list(self.target.parent.glob(".pcl-rh-node-*")), [])
        install = next(command for command, _ in self.commands if "/usr/bin/install" in command)
        move = next(command for command, _ in self.commands if "/bin/mv" in command)
        self.assertEqual(Path(install[-1]).parent, self.target.parent)
        self.assertEqual(move, ["/usr/bin/sudo", "-n", "/bin/mv", "-f", "-T", install[-1], str(self.target)])
        for command, options in self.commands:
            self.assertIsInstance(command, list)
            self.assertIs(options["shell"], False)
            self.assertEqual(options["env"], {"PATH": "/usr/bin:/bin", "LANG": "C"})
            self.assertTrue(options["check"])

    def test_macos_uses_no_follow_bsd_move(self):
        with mock.patch.object(NODE.platform, "system", return_value="Darwin"), \
                mock.patch.object(NODE.platform, "machine", return_value="arm64"), \
                mock.patch.dict(os.environ, {"RUNNER_OS": "macOS"}):
            self.prepare()
        move = next(command for command, _ in self.commands if "/bin/mv" in command)
        self.assertEqual(move[3:5], ["-f", "-h"])

    def test_old_destination_link_is_replaced_without_writing_its_target(self):
        self.target.parent.mkdir(parents=True)
        outside = self.root / "old-node-outside"
        outside.write_bytes(b"preserve original link target")
        self.target.symlink_to(outside)
        self.prepare()
        self.assertFalse(self.target.is_symlink())
        self.assertEqual(outside.read_bytes(), b"preserve original link target")
        self.assertEqual(self.target.read_bytes(), self.source.read_bytes())

    def test_copy_source_identity_drift_never_replaces_the_existing_node(self):
        self.target.parent.mkdir(parents=True)
        self.target.write_bytes(b"existing node")
        def replace_source(command):
            if "/usr/bin/install" in command:
                replacement = self.source.with_name("replacement")
                shutil.copyfile(self.source, replacement)
                replacement.chmod(0o755)
                os.replace(replacement, self.source)
            return None
        self.hook = replace_source
        with self.assertRaisesRegex(ValueError, "hash or identity"):
            self.prepare()
        self.assertEqual(self.target.read_bytes(), b"existing node")
        self.assertFalse(any("/bin/mv" in command for command, _ in self.commands))
        self.assertEqual(list(self.target.parent.glob(".pcl-rh-node-*")), [])

    def test_corrupt_staging_is_cleaned_but_a_replaced_staging_link_is_retained(self):
        outside = self.root / "unrelated"
        outside.write_bytes(b"retain unrelated")
        for linked in (False, True):
            with self.subTest(linked=linked):
                self.commands.clear()
                def corrupt(command):
                    if command[-1] == "--version" and Path(command[0]) != self.source:
                        staged = Path(command[0])
                        if linked:
                            staged.unlink()
                            staged.symlink_to(outside)
                        else:
                            staged.write_bytes(b"modified staged bytes")
                    return None
                self.hook = corrupt
                with self.assertRaises(ValueError):
                    self.prepare()
                self.assertFalse(self.target.exists())
                retained = list(self.target.parent.glob(".pcl-rh-node-*"))
                self.assertEqual(len(retained), int(linked))
                self.assertEqual(outside.read_bytes(), b"retain unrelated")
                if linked:
                    self.assertTrue(retained[0].is_symlink())
                    self.assertFalse(any("/bin/rm" in command for command, _ in self.commands))

    def test_existing_destination_directory_is_never_treated_as_a_move_parent(self):
        self.target.mkdir(parents=True)
        with self.assertRaisesRegex(ValueError, "directory or special"):
            self.prepare()
        self.assertEqual(list(self.target.iterdir()), [])
        self.assertFalse(any(command[0] == "/usr/bin/sudo" for command, _ in self.commands))


if __name__ == "__main__":
    unittest.main()
