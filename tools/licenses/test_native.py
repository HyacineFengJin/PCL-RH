"""Native attribution regressions with synthetic caches and command replies.

No actual Cargo/rustc commands, dependency code, network or user packages run.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("native_attribution", Path(__file__).with_name("native.py"))
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class NativeTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[2] / "work/native-license-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(dir=scratch))
        self.addCleanup(shutil.rmtree, self.root)
        self.commit = "a" * 40
        self.inputs = {}
        for name in module.REQUIRED_INPUTS | {"apps/desktop/src-tauri/Cargo.toml", "crates/cli/Cargo.toml"}:
            self.write(name, "{}\n" if name.endswith(".json") else "# Fixture input\n")
        self.cargo_source = "registry+https://github.com/rust-lang/crates.io-index"
        self.cargo_packages = [self.workspace("pcl-desktop"), self.workspace("pcl-cli"), self.workspace("unused-workspace")]
        self.cargo_nodes = [self.node("pcl-desktop", [("normal-lib", None), ("generator-lib", "build"), ("test-only", "dev")]),
                            self.node("pcl-cli", [("normal-lib", None)]),
                            self.node("unused-workspace", [("unused-lib", None)])]
        locked = []
        for name in ("normal-lib", "generator-lib", "test-only", "unused-lib"):
            package = self.cargo_package(name, installed=name in {"normal-lib", "generator-lib"})
            self.cargo_packages.append(package)
            self.cargo_nodes.append(self.node(name))
            locked.append('[[package]]')
            locked.extend([f'name = "{name}"', 'version = "1.0.0"', f'source = "{self.cargo_source}"',
                           f'checksum = "{module.inventory.sha256(name.encode())}"', ""])
        self.write("Cargo.lock", "\n".join(locked))
        self.runtime_lock = {"packages": {
            "": {"dependencies": {"runtime-lib": "1.0.0"}},
            "node_modules/runtime-lib": {"version": "1.0.0", "license": "MIT", "integrity": "sha512-fixture"},
            "node_modules/absent-other-host": {"version": "1.0.0", "optional": True},
        }}
        self.write("experimental/runtime/package.json", json.dumps({"dependencies": {"runtime-lib": "1.0.0"}}))
        self.save_runtime_lock()
        self.npm_package("runtime-lib")
        self.calls = []
        self.addCleanup(patch.stopall)
        patch.object(module, "command", side_effect=self.command).start()

    def write(self, name, text):
        data = text.encode("utf-8")
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        self.inputs[name] = data

    def save_runtime_lock(self):
        self.write("experimental/runtime/package-lock.json", json.dumps(self.runtime_lock))

    @staticmethod
    def oid(data):
        return hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()

    def command(self, args, root):
        self.assertEqual(root, self.root)
        self.calls.append(args)
        if args[:2] == ["git", "rev-parse"]:
            return (self.commit + "\n").encode()
        if args[:2] == ["git", "ls-tree"]:
            return b"".join(f"100644 blob {self.oid(data)}\t{name}\0".encode()
                            for name, data in sorted(self.inputs.items()))
        if args[:3] == ["git", "cat-file", "blob"]:
            return next(data for data in self.inputs.values() if self.oid(data) == args[3])
        if args == ["rustc", "-vV"]:
            return b"rustc 1.95.0\nrelease: 1.95.0\ncommit-hash: abcdef\nhost: aarch64-apple-darwin\n"
        if args[:2] == ["cargo", "metadata"]:
            self.assertIn("--offline", args)
            self.assertIn("--locked", args)
            self.assertEqual(args[args.index("--filter-platform") + 1], "aarch64-apple-darwin")
            self.assertEqual(args[-2:], ["--features", "pcl-desktop/custom-protocol"])
            return json.dumps({"workspace_members": ["pcl-desktop", "pcl-cli", "unused-workspace"],
                               "packages": self.cargo_packages, "resolve": {"nodes": self.cargo_nodes}}).encode()
        self.fail("Unexpected child process: " + repr(args))

    def workspace(self, name):
        return {"id": name, "name": name, "version": "0.2.0"}

    @staticmethod
    def node(name, dependencies=()):
        return {"id": name, "features": [], "deps": [
            {"pkg": package, "dep_kinds": [{"kind": kind, "target": None}]} for package, kind in dependencies]}

    def cargo_package(self, name, installed):
        directory = self.root / "cache" / name
        if installed:
            directory.mkdir(parents=True)
            manifest = f'[package]\nname="{name}"\nversion="1.0.0"\nlicense="MIT"\n'.encode()
            notice = ("Original " + name + " license\r\nCopyright (c) Fixture author\r\n").encode()
            (directory / "Cargo.toml").write_bytes(manifest)
            (directory / "LICENSE").write_bytes(notice)
            (directory / ".cargo-checksum.json").write_text(json.dumps({
                "package": module.inventory.sha256(name.encode()),
                "files": {"Cargo.toml": module.inventory.sha256(manifest), "LICENSE": module.inventory.sha256(notice)},
            }))
        return {"id": name, "name": name, "version": "1.0.0", "source": self.cargo_source,
                "license": "MIT", "manifest_path": str(directory / "Cargo.toml"), "authors": ["Fixture author"]}

    def npm_package(self, name):
        directory = self.root / "experimental/runtime/node_modules" / name
        directory.mkdir(parents=True)
        data = {"name": name, "version": "1.0.0", "license": "MIT",
                "scripts": {"postinstall": "this-fixture-must-never-execute"}, "author": "Fixture author"}
        (directory / "package.json").write_text(json.dumps(data))
        (directory / "LICENSE").write_bytes(b"Original runtime license\r\nCopyright (c) Runtime author\r\n")
        return directory

    def test_native_graph_preserves_originals_and_excludes_unshipped_packages(self):
        document, text = module.documents(self.root)
        self.assertEqual(document["source_commit"], self.commit)
        self.assertEqual(document["target"], "aarch64-apple-darwin")
        self.assertEqual([(row["name"], row["scope"]) for row in document["cargo"]["packages"]],
                         [("generator-lib", "build"), ("normal-lib", "normal")])
        self.assertEqual([row["name"] for row in document["npm"][0]["packages"]], ["runtime-lib"])
        self.assertEqual(document["npm"][0]["summary"]["not_installed_optional"], 1)
        self.assertEqual(document["issues"], [])
        self.assertIn("Original normal-lib license\r\nCopyright (c) Fixture author\r\n", text)
        self.assertIn("Original runtime license\r\nCopyright (c) Runtime author\r\n", text)
        self.assertEqual(document["license_text_bundle_sha256"], module.inventory.sha256(text.encode()))
        self.assertNotIn(str(self.root), json.dumps(document) + text)
        self.assertEqual({row["path"] for row in document["inputs"]}, self.inputs.keys())
        self.assertTrue(all(row["sha256"] == module.inventory.sha256(self.inputs[row["path"]]) for row in document["inputs"]))
        self.assertEqual(sum(args[:2] == ["cargo", "metadata"] for args in self.calls), 1)
        output = self.root / "out"
        module.write_outputs(output, document, text)
        self.assertEqual((output / module.OUTPUT_TEXT).read_bytes(), text.encode())
        self.assertEqual(json.loads((output / module.OUTPUT_JSON).read_bytes()), document)

    def test_dirty_source_input_is_rejected_before_any_dependency_scan(self):
        (self.root / "Cargo.lock").write_text("changed lock")
        with self.assertRaisesRegex(ValueError, "differs from HEAD: Cargo.lock"):
            module.documents(self.root)
        self.assertFalse(any(args[0] in {"cargo", "rustc"} for args in self.calls))

    def test_changed_commit_during_collection_is_rejected(self):
        original = module.NativeAudit.npm
        def npm(audit):
            result = original(audit)
            self.commit = "b" * 40
            return result
        with patch.object(module.NativeAudit, "npm", npm):
            with self.assertRaisesRegex(ValueError, "changed during collection"):
                module.documents(self.root)

    def test_installed_npm_identity_and_unlocked_extra_packages_are_rejected(self):
        metadata = self.root / "experimental/runtime/node_modules/runtime-lib/package.json"
        data = json.loads(metadata.read_bytes())
        data["version"] = "2.0.0"
        metadata.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "differs from the lockfile"):
            module.documents(self.root)
        data["version"] = "1.0.0"
        metadata.write_text(json.dumps(data))
        self.npm_package("extra-unlocked")
        with self.assertRaisesRegex(ValueError, "absent from the committed npm lock"):
            module.documents(self.root)

    def test_missing_required_runtime_package_is_not_silently_omitted(self):
        shutil.rmtree(self.root / "experimental/runtime/node_modules/runtime-lib")
        with self.assertRaisesRegex(ValueError, "required_package_not_installed"):
            module.documents(self.root)

    def test_missing_declaration_or_legal_text_blocks_generation(self):
        metadata = self.root / "experimental/runtime/node_modules/runtime-lib/package.json"
        data = json.loads(metadata.read_bytes())
        del data["license"]
        metadata.write_text(json.dumps(data))
        del self.runtime_lock["packages"]["node_modules/runtime-lib"]["license"]
        self.save_runtime_lock()
        with self.assertRaisesRegex(ValueError, "license_declaration_missing"):
            module.documents(self.root)
        data["license"] = "MIT"
        metadata.write_text(json.dumps(data))
        self.runtime_lock["packages"]["node_modules/runtime-lib"]["license"] = "MIT"
        self.save_runtime_lock()
        (metadata.parent / "LICENSE").unlink()
        with self.assertRaisesRegex(ValueError, "license_text_missing"):
            module.documents(self.root)

    def test_canonical_terms_preserve_original_evidence_warning(self):
        package = self.root / "experimental/runtime/node_modules/runtime-lib"
        (package / "LICENSE").unlink()
        terms = "Canonical MIT terms\nCopyright (c) <year> <copyright holders>\n"
        source = {"evidence_kind": "canonical_text_for_declared_spdx", "license_identifier": "MIT",
                  "license_text": terms, "license_sha256": module.inventory.sha256(terms.encode()),
                  "license_url": "https://example.org/immutable/MIT",
                  "verified_packages": [{"name": "runtime-lib", "version": "1.0.0", "declared_license": "MIT",
                                         "registry_integrity": "sha512-fixture",
                                         "published_manifest_sha256": module.inventory.sha256((package / "package.json").read_bytes())}]}
        self.write("tools/licenses/upstream-supplements.json", json.dumps({"sources": [source]}))
        document, text = module.documents(self.root)
        self.assertEqual(document["issues"][0]["code"], "original_license_notice_missing")
        self.assertTrue(document["issues"][0]["fails_strict"])
        self.assertIn(terms, text)
        self.assertNotIn("Original runtime license", text)
        row = document["npm"][0]["packages"][0]
        self.assertEqual(row["license_files"], [])
        self.assertEqual(row["canonical_license_texts"][0]["text_sha256"], source["license_sha256"])

    def test_cargo_source_cache_or_lock_mismatch_blocks_generation(self):
        (self.root / "cache/normal-lib/LICENSE").write_text("Changed notice")
        with self.assertRaisesRegex(ValueError, "differs from Cargo source checksums"):
            module.documents(self.root)

    def test_source_paths_are_rejected_without_rewriting_legal_text(self):
        (self.root / "experimental/runtime/node_modules/runtime-lib/LICENSE").write_text("Copyright Fixture\n" + str(self.root))
        with self.assertRaisesRegex(ValueError, "contains a local machine path"):
            module.documents(self.root)

    def test_linked_runtime_scope_is_rejected_without_following_it(self):
        outside = self.root / "outside"
        outside.mkdir()
        scope = self.root / "experimental/runtime/node_modules/@linked"
        try:
            scope.symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("Directory symlinks are unavailable on this host")
        with self.assertRaisesRegex(ValueError, "Linked runtime package or scope"):
            module.documents(self.root)


if __name__ == "__main__":
    unittest.main()
