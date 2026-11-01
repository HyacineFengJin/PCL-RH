"""Exercise the real artifact-download boundary with synthetic API and ZIP data."""
from __future__ import annotations

import contextlib
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import zipfile


SPEC = importlib.util.spec_from_file_location("pcl_fetch_native", Path(__file__).parent / "ci/fetch-native-candidate.py")
FETCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FETCH)
COMMIT = "a" * 40
REPOSITORY = "HyacineFengJin/PCL-RH"
RUN = "73"


def archive(system="windows", arch="x86_64", member="package-manifest.json", symlink=False):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as source:
        entry = zipfile.ZipInfo(member)
        # Windows ZipInfo construction normalizes os.sep. Retain the requested
        # raw ZIP spelling so this actually tests an archive with a backslash.
        entry.filename = entry.orig_filename = member
        entry.create_system = 3
        entry.external_attr = (0o120777 if symlink else 0o100644) << 16
        source.writestr(entry, json.dumps({"platform": system, "arch": arch,
                                          "profile": "release", "commit": COMMIT}))
    return output.getvalue()


class NativeProducerTests(unittest.TestCase):
    def exercise(self, mutate=lambda r, p: None, *, accept=False, source=RUN,
                 system="windows", arch="x86_64", runner="windows-2025",
                 member="package-manifest.json", symlink=False):
        blob = archive(system, arch, member, symlink)
        artifact_name = f"portable-preview-{runner}-release"
        run = {"id": int(RUN), "repository": {"id": 1, "full_name": REPOSITORY},
               "head_repository": {"id": 1, "full_name": REPOSITORY}, "head_sha": COMMIT,
               "head_branch": "master", "event": "workflow_dispatch", "status": "completed",
               "path": ".github/workflows/official-native.yml", "conclusion": "failure"}
        item = {"id": 91, "size_in_bytes": len(blob), "name": artifact_name,
                "expired": False, "expires_at": "2099-01-01T00:00:00Z",
                "digest": "sha256:" + hashlib.sha256(blob).hexdigest(),
                "workflow_run": {"id": int(RUN), "repository_id": 1, "head_repository_id": 1,
                                 "head_branch": "master", "head_sha": COMMIT}}
        pages = [{"artifacts": [item]}]
        mutate(run, pages)
        calls = []

        def api(command, **kwargs):
            calls.append(command)
            self.assertEqual(command[:2], ["gh", "api"])
            if command[-1] == f"repos/{REPOSITORY}/actions/runs/{RUN}":
                return json.dumps(run).encode()
            self.assertEqual(command[-1], f"repos/{REPOSITORY}/actions/runs/{RUN}/artifacts?name={artifact_name}&per_page=100")
            self.assertIn("--paginate", command)
            self.assertIn("--slurp", command)
            return json.dumps(pages).encode()

        def download(command, **kwargs):
            calls.append(command)
            self.assertEqual(command, ["gh", "api", f"repos/{REPOSITORY}/actions/artifacts/91/zip"])
            self.assertTrue(kwargs["check"])
            kwargs["stdout"].write(blob)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            envfile = root / "github-env"
            with mock.patch.dict(os.environ, {"RUNNER_TEMP": str(root), "GITHUB_ENV": str(envfile),
                                               "GITHUB_SHA": "b" * 40}), \
                    mock.patch.object(FETCH, "native_target", return_value=(system, arch, artifact_name)), \
                    mock.patch.object(FETCH.subprocess, "check_output", side_effect=api), \
                    mock.patch.object(FETCH.subprocess, "run", side_effect=download), \
                    contextlib.redirect_stdout(io.StringIO()):
                if accept:
                    FETCH.fetch(source, root / "payload", root / "evidence/source-run.json")
                    report = json.loads((root / "evidence/source-run.json").read_bytes())
                    self.assertEqual(report["source_commit"], COMMIT)
                    self.assertEqual(report["distribution_tool_commit"], "b" * 40)
                    self.assertEqual(report["source_conclusion"], "failure")
                    self.assertEqual(os.environ["GITHUB_SHA"], "b" * 40)
                    self.assertEqual(envfile.read_text(), "PCL_NATIVE_SOURCE_COMMIT=" + COMMIT + "\n")
                else:
                    with self.assertRaises((ValueError, SystemExit)):
                        FETCH.fetch(source, root / "payload", root / "evidence/source-run.json")
                    self.assertFalse(envfile.exists())
                    if source != RUN:
                        self.assertEqual(calls, [])

    def test_same_verified_payload_survives_later_job_failure_and_pagination(self):
        self.exercise(accept=True)
        self.exercise(lambda r, p: p.insert(0, {"artifacts": []}), accept=True)
        self.exercise(lambda r, p: p[0]["artifacts"].append({**p[0]["artifacts"][0], "expired": True}), accept=True)
        for arch, runner in (("aarch64", "macos-15"), ("x86_64", "macos-15-intel")):
            self.exercise(accept=True, system="macos", arch=arch, runner=runner)

    def test_host_architecture_selects_only_its_own_artifact(self):
        for system, machine, expected in (
                ("Darwin", "arm64", ("macos", "aarch64", "portable-preview-macos-15-release")),
                ("Darwin", "x86_64", ("macos", "x86_64", "portable-preview-macos-15-intel-release")),
                ("Windows", "AMD64", ("windows", "x86_64", "portable-preview-windows-2025-release")),
                ("Linux", "x86_64", ("linux", "x86_64", "portable-preview-ubuntu-24.04-release"))):
            with self.subTest(system=system, machine=machine), \
                    mock.patch.object(FETCH.platform, "system", return_value=system), \
                    mock.patch.object(FETCH.platform, "machine", return_value=machine):
                self.assertEqual(FETCH.native_target(), expected)
        with mock.patch.object(FETCH.platform, "system", return_value="Linux"), \
                mock.patch.object(FETCH.platform, "machine", return_value="arm64"), self.assertRaises(ValueError):
            FETCH.native_target()

    def test_unrelated_or_incomplete_producers_cannot_authorize_download(self):
        self.exercise(source="73; echo injected")
        for key, value in (("head_branch", "feature"), ("event", "pull_request"),
                           ("path", ".github/workflows/desktop-platforms.yml"),
                           ("status", "in_progress"), ("head_sha", COMMIT + "\nKEY=value")):
            with self.subTest(key=key):
                self.exercise(lambda r, p: r.update({key: value}))
        for role in ("repository", "head_repository"):
            self.exercise(lambda r, p: r[role].update(full_name="unrelated/PCL-RH"))
        self.exercise(lambda r, p: r["head_repository"].update(id=2))

    def test_ambiguous_expired_or_unbound_artifacts_are_rejected(self):
        self.exercise(lambda r, p: p[0]["artifacts"].clear())
        self.exercise(lambda r, p: p[0]["artifacts"].append(copy.deepcopy(p[0]["artifacts"][0])))
        for field, value in (("expired", True), ("expires_at", "2000-01-01T00:00:00Z"),
                             ("size_in_bytes", 513 * 1024**2), ("digest", None),
                             ("digest", "sha256:" + "0" * 64)):
            with self.subTest(field=field, value=value):
                self.exercise(lambda r, p: p[0]["artifacts"][0].update({field: value}))
        for field, value in (("id", 74), ("repository_id", 2), ("head_repository_id", 2),
                             ("head_branch", "feature"), ("head_sha", "c" * 40)):
            with self.subTest(field=field):
                self.exercise(lambda r, p: p[0]["artifacts"][0]["workflow_run"].update({field: value}))

    def test_authenticated_bytes_still_cannot_escape_extraction_root(self):
        for member in ("../escape", "/absolute", "C:/absolute", "x:stream", "CON.txt", "code/a.",
                       "code/a ", "code\\escape"):
            with self.subTest(member=member):
                self.exercise(member=member)
        self.exercise(member="linked", symlink=True)


if __name__ == "__main__":
    unittest.main()
