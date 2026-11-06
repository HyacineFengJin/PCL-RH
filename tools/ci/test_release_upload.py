"""Pinned draft metadata and ZIP boundaries; no network or publication."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import warnings
import zipfile

SPEC = importlib.util.spec_from_file_location("release_upload", Path(__file__).with_name("upload-release-packages.py"))
UPLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPLOAD)


def document():
    return {"schema_version": 1, "tag": "v0.2.1", "version": "0.2.1",
            "source_commit": "a" * 40, "release_commit": "b" * 40, "producer_run": 123,
            "distribution_tool_commits": {name: "c" * 40 for name in ("linux", "windows", "macos")},
            "assets": [{"platform": platform, "form": form,
                        "name": f"pcl-rh-0.2.1-{platform}-{form}.zip", "source_artifact_file": "package.zip",
                        "bytes": 3, "sha256": hashlib.sha256(b"abc").hexdigest(),
                        "source_run": 123 if form in ("portable", "per-user-installer") else 456}
                       for platform, form in sorted(UPLOAD.FORMS)]}


class UploadTests(unittest.TestCase):
    def test_exact_distribution_set_and_known_producer_mapping(self):
        doc = document()
        rows = UPLOAD.packages(doc, "v0.2.1")
        self.assertEqual(len(rows), 10)
        for row in rows:
            workflow, name, commit = UPLOAD.producer(row, doc)
            self.assertTrue(workflow.endswith(".yml"))
            self.assertIn(commit, ("a" * 40, "c" * 40))
            self.assertNotIn("/", name)
        windows = next(r for r in rows if r["form"] == "per-user-installer")
        self.assertEqual(UPLOAD.producer(windows, doc)[:2], ("official-native.yml", "windows-user-installer-release"))
        windows["source_run"] = 789
        self.assertEqual(UPLOAD.producer(windows, doc)[:2], ("windows-installer.yml", "windows-user-installer-retest"))

    def test_wrong_version_duplicate_missing_or_unknown_packages_rejected(self):
        variants = []
        doc = document(); doc["version"] = "0.2.0"; variants.append(doc)
        doc = document(); doc["assets"].pop(); variants.append(doc)
        doc = document(); doc["assets"][1] = copy.deepcopy(doc["assets"][0]); variants.append(doc)
        doc = document(); doc["assets"][0]["form"] = "arbitrary-program"; variants.append(doc)
        for doc in variants:
            with self.subTest(doc=doc), self.assertRaises(ValueError):
                UPLOAD.packages(doc, "v0.2.1")

    def test_paths_invalid_digests_and_unbounded_sizes_rejected(self):
        for key, value in (("name", "../secret"), ("source_artifact_file", "x/../../secret"),
                           ("source_artifact_file", "C:\\secret"), ("name", "pcl-rh-0.2.0-old.zip"),
                           ("sha256", "not-a-digest"), ("bytes", True), ("bytes", UPLOAD.MAX_PACKAGE + 1),
                           ("source_run", True), ("source_run", -1)):
            doc = document(); doc["assets"][0][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                UPLOAD.packages(doc, "v0.2.1")

    def test_uploaded_asset_requires_exact_digest_size_and_state(self):
        asset = {"size": 3, "state": "uploaded", "digest": "sha256:" + "a" * 64}
        self.assertTrue(UPLOAD.matches(asset, 3, "a" * 64))
        self.assertFalse(UPLOAD.matches(asset, 4, "a" * 64))
        self.assertFalse(UPLOAD.matches(asset, 3, "b" * 64))
        self.assertFalse(UPLOAD.matches({**asset, "state": "starter"}, 3, "a" * 64))

    def test_public_releases_and_duplicate_assets_are_never_overwritten(self):
        with mock.patch.object(UPLOAD, "api", return_value=[[{"tag_name": "v0.2.1", "draft": False}]]):
            with self.assertRaises(ValueError):
                UPLOAD.draft("v0.2.1")
        with self.assertRaises(ValueError):
            UPLOAD.selected_asset({"assets": [{"name": "x"}, {"name": "x"}]}, "x")

    def test_real_zip_copy_checks_size_hash_and_duplicate_member(self):
        parent = Path(__file__).resolve().parents[2] / "work"
        parent.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="release-upload-test-", dir=parent) as temporary:
            root = Path(temporary); archive = root / "artifact.zip"
            row = document()["assets"][0]
            with zipfile.ZipFile(archive, "w") as z:
                z.writestr("package.zip", b"abc")
                z.writestr("../ignored", b"never extracted")
            UPLOAD.copy_member(archive, root / "copied", row)
            self.assertEqual((root / "copied").read_bytes(), b"abc")
            self.assertEqual({p.name for p in root.iterdir()}, {"artifact.zip", "copied"})
            with self.assertRaises(ValueError):
                UPLOAD.copy_member(archive, root / "bad-size", {**row, "bytes": 4})
            with self.assertRaises(ValueError):
                UPLOAD.copy_member(archive, root / "bad-hash", {**row, "sha256": "0" * 64})
            with zipfile.ZipFile(archive, "a") as z, warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                z.writestr("package.zip", b"abc")
            with self.assertRaises(ValueError):
                UPLOAD.copy_member(archive, root / "duplicate", row)


if __name__ == "__main__":
    unittest.main()
