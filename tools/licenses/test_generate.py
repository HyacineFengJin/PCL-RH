"""Regression checks for legal-text evidence; no package code or Cargo runs."""
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("license_inventory", Path(__file__).with_name("generate.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[2] / "work/license-inventory-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(dir=scratch))
        self.addCleanup(shutil.rmtree, self.root)
        self.audit = module.Audit(self.root)

    def test_icon_source_map_is_not_legal_text(self):
        package = self.root / "package"
        package.mkdir()
        (package / "copyright.js.map").write_text('{"sourcesContent":["not a license"]}')
        self.assertEqual(self.audit.licenses(package, "npm:fixture@1"), [])
        self.assertEqual(self.audit.texts, {})
        (package / "LICENSE").write_text("Fixture permission notice.\n")
        refs = self.audit.licenses(package, "npm:fixture@1")
        self.assertEqual([row["file"] for row in refs], ["LICENSE"])

    def test_identical_text_retains_both_package_sources(self):
        for name in ("first", "second"):
            package = self.root / name
            package.mkdir()
            (package / "NOTICE.txt").write_text("Same original notice.\n")
            self.audit.licenses(package, name)
        self.assertEqual(len(self.audit.texts), 1)
        row = next(iter(self.audit.texts.values()))
        self.assertEqual(row["sources"], ["first/NOTICE.txt", "second/NOTICE.txt"])
        self.assertEqual(row["text"], "Same original notice.\n")

    def test_external_declared_file_is_not_collected(self):
        package = self.root / "package"
        package.mkdir()
        (self.root / "outside.txt").write_text("Not this package's evidence")
        self.assertEqual(self.audit.licenses(package, "fixture", "../outside.txt"), [])
        self.assertEqual(self.audit.texts, {})
        self.assertEqual(self.audit.issues[0]["code"], "declared_license_file_unavailable")

    def test_legacy_npm_supplement_can_supply_multiple_original_documents(self):
        tree = self.root / "fixture"
        package = tree / "node_modules/example"
        package.mkdir(parents=True)
        (package / "package.json").write_text(json.dumps({"name": "example", "version": "1.0.0", "license": "MIT OR Apache-2.0"}))
        (tree / "package.json").write_text(json.dumps({"dependencies": {"example": "1.0.0"}}))
        (tree / "package-lock.json").write_text(json.dumps({"packages": {
            "": {"dependencies": {"example": "1.0.0"}},
            "node_modules/example": {"version": "1.0.0", "license": "MIT OR Apache-2.0"},
        }}))
        source_dir = self.root / "tools/licenses"
        source_dir.mkdir(parents=True)
        sources = []
        for name, text in (("LICENSE-MIT", "Original MIT notice\n"), ("LICENSE-APACHE", "Original Apache notice\n")):
            sources.append({"license_text": text, "license_sha256": module.sha256(text.encode()),
                            "license_url": "https://example.org/fixed-commit/" + name, "source_file": name,
                            "verified_packages": [{"name": "example", "version": "1.0.0", "declared_license": "MIT OR Apache-2.0"}]})
        (source_dir / "upstream-supplements.json").write_text(json.dumps({"sources": sources}))
        with patch.object(module, "NPM_TREES", ("fixture",)):
            row = self.audit.npm()[0]["packages"][0]
        self.assertEqual(len(row["license_files"]), 2)
        self.assertEqual(len(row["upstream_supplements"]), 2)
        self.assertEqual({record["text"] for record in self.audit.texts.values()}, {source["license_text"] for source in sources})
        self.assertEqual(row["declared_license"], "MIT OR Apache-2.0")
        self.assertEqual(self.audit.issues, [])

    def cargo_evidence_fixture(self):
        package = self.root / "registry/src/registry-id/example-1.0.0"
        package.mkdir(parents=True)
        original = b'[package]\nname="example"\nversion="1.0.0"\nlicense="MIT"\n'
        (package / "Cargo.toml.orig").write_bytes(original)
        commit = "a" * 40
        (package / ".cargo_vcs_info.json").write_text(json.dumps({"git": {"sha1": commit}, "path_in_vcs": "example"}))
        checksum = module.sha256(b"expected published archive")
        evidence = {"cargo_vcs_commit": commit, "repository_directory": "example",
                    "published_original_manifest_sha256": module.sha256(original),
                    "published_archive_sha256": checksum, "cargo_lock_checksum": checksum}
        return package, evidence, {"source_commit": commit}, checksum

    def test_npm_same_version_with_different_archive_integrity_is_rejected(self):
        tree = self.root / "fixture"
        package = tree / "node_modules/example"
        package.mkdir(parents=True)
        metadata = {"name": "example", "version": "1.0.0", "license": "MIT"}
        (package / "package.json").write_text(json.dumps(metadata))
        (tree / "package.json").write_text(json.dumps({"dependencies": {"example": "1.0.0"}}))
        (tree / "package-lock.json").write_text(json.dumps({"packages": {
            "": {"dependencies": {"example": "1.0.0"}},
            "node_modules/example": {"version": "1.0.0", "license": "MIT", "integrity": "sha512-changed"},
        }}))
        source_dir = self.root / "tools/licenses"
        source_dir.mkdir(parents=True)
        text = "Original verified notice\n"
        (source_dir / "upstream-supplements.json").write_text(json.dumps({"sources": [{
            "license_text": text, "license_sha256": module.sha256(text.encode()),
            "license_url": "https://example.org/fixed-commit/LICENSE",
            "verified_packages": [{**metadata, "declared_license": "MIT", "registry_integrity": "sha512-original"}],
        }]}))
        with patch.object(module, "NPM_TREES", ("fixture",)):
            with self.assertRaisesRegex(ValueError, "integrity differs"):
                self.audit.npm()
        self.assertEqual(self.audit.texts, {})

    def test_cargo_changed_vcs_commit_is_rejected_before_adopting_text(self):
        package, evidence, source, checksum = self.cargo_evidence_fixture()
        (package / ".cargo_vcs_info.json").write_text(json.dumps({"git": {"sha1": "b" * 40}, "path_in_vcs": "example"}))
        with self.assertRaisesRegex(ValueError, "vcs commit"):
            self.audit.validate_cargo_supplement(package, evidence, source, checksum)
        self.assertEqual(self.audit.texts, {})

    def test_cargo_cached_archive_checksum_mismatch_is_rejected(self):
        package, evidence, source, checksum = self.cargo_evidence_fixture()
        archive = self.root / "registry/cache/registry-id/example-1.0.0.crate"
        archive.parent.mkdir(parents=True)
        archive.write_bytes(b"different cached archive")
        with self.assertRaisesRegex(ValueError, "cached archive checksum"):
            self.audit.validate_cargo_supplement(package, evidence, source, checksum)
        self.assertEqual(self.audit.texts, {})


if __name__ == "__main__":
    unittest.main()
