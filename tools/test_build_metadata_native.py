"""Actual incremental Cargo/Git fixture for the production build identity helper.

Only a tiny std-only binary is compiled in a temporary checkout. No launcher,
WebView, service credential, dependency download or user state is involved.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "apps/desktop/src-tauri/build_metadata.rs"


class IncrementalBuildIdentityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="pcl identity space ")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / "source"
        self.root.mkdir()
        self.environment = os.environ.copy()
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
            self.environment.pop(name, None)
        self.environment["CARGO_TARGET_DIR"] = str(self.base / "target")
        self.environment["CARGO_BUILD_JOBS"] = "1"
        self.environment["CARGO_INCREMENTAL"] = "0"
        self.environment["CARGO_PROFILE_DEV_DEBUG"] = "0"
        self.git("init", "-q")
        self.write(".gitignore", "/target/\n")
        self.write("Cargo.toml", '[workspace]\nmembers=["apps/desktop/src-tauri"]\nresolver="2"\n')
        self.write("apps/desktop/src-tauri/Cargo.toml",
                   '[package]\nname="identity-fixture"\nversion="0.0.0"\nedition="2021"\n')
        self.write("apps/desktop/src-tauri/src/main.rs",
                   'fn main() { println!("{}", env!("FIXTURE_BUILD_COMMIT")); }\n')
        # The production helper is included by path, not copied/reimplemented.
        self.write("apps/desktop/src-tauri/build.rs", f"""
#[path = {json.dumps(str(HELPER), ensure_ascii=False)}]
mod production_metadata;
fn main() {{
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let identity = production_metadata::identity(&root).unwrap_or_default();
    println!("cargo:rustc-env=FIXTURE_BUILD_COMMIT={{identity}}");
    let counter = std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("identity-invocations");
    let count = std::fs::read_to_string(&counter).ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    std::fs::write(counter, (count + 1).to_string()).unwrap();
}}
""")
        self.leaf = self.write("crates/captured/source.txt", "original source\n")
        self.cargo("generate-lockfile", "--offline")
        self.commit()

    def command(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
        return subprocess.run(arguments, cwd=self.root, env=self.environment,
                              capture_output=True, text=True, encoding="utf-8",
                              timeout=90, check=True)

    def git(self, *arguments: str) -> str:
        return self.command(["git", *arguments]).stdout.strip()

    def cargo(self, *arguments: str) -> str:
        return self.command(["cargo", *arguments, "--manifest-path",
                             str(self.root / "apps/desktop/src-tauri/Cargo.toml")]).stdout.strip()

    def write(self, relative: str, text: str) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def commit(self) -> str:
        self.git("add", ".")
        self.git("-c", "user.name=Identity fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "synthetic build sources")
        return self.git("rev-parse", "HEAD")

    def probe(self) -> str:
        return self.cargo("run", "--offline", "--locked", "--quiet", "-j", "1")

    def invocations(self) -> int:
        counters = list((self.base / "target").rglob("identity-invocations"))
        self.assertEqual(len(counters), 1)
        return int(counters[0].read_text())

    def test_incremental_source_changes_never_reuse_a_clean_commit(self) -> None:
        commit = self.git("rev-parse", "HEAD")
        self.assertEqual(self.probe(), commit)
        count = self.invocations()
        self.assertEqual(self.probe(), commit)
        self.assertEqual(self.invocations(), count, "metadata checks must settle without rebuilding forever")

        self.leaf.write_text("changed captured source, not the tiny binary's source\n", encoding="utf-8")
        self.assertEqual(self.probe(), "", "dirty tracked sources must retire the old build identity")
        self.assertGreater(self.invocations(), count)

        self.leaf.write_text("original source\n", encoding="utf-8")
        self.assertEqual(self.probe(), commit)
        self.git("-c", "user.name=Identity fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "--allow-empty", "-qm", "new source generation")
        new_commit = self.git("rev-parse", "HEAD")
        self.assertNotEqual(new_commit, commit)
        self.assertEqual(self.probe(), new_commit)

    def test_archive_inside_another_repository_has_no_parent_identity(self) -> None:
        # Move this checkout's metadata outside the archive, and create a
        # separate parent repo. Git objects can be read-only on Windows; their
        # contents and permissions need not change to test upward discovery.
        # Git's normal upward discovery must not authenticate the archive.
        (self.root / ".git").rename(self.base / "retained-fixture-git")
        self.command(["git", "-C", str(self.base), "init", "-q"])
        self.command(["git", "-C", str(self.base), "-c", "user.name=Parent fixture",
                      "-c", "user.email=parent@example.invalid", "commit", "--allow-empty",
                      "-qm", "unrelated parent repository"])
        self.assertEqual(self.probe(), "")


if __name__ == "__main__":
    unittest.main()
