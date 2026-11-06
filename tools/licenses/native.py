#!/usr/bin/env python3
"""Collect attribution for a native host's locked production dependencies.

Runs only git, rustc -vV and offline Cargo metadata. Package code, package-manager
scripts, downloads and builds are never invoked. The source inventory remains a
separate document: this supplement follows the native Cargo graph and the
installed runtime tree that package-native.py copies into its archive.
"""
from __future__ import annotations

import argparse
from collections import Counter
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib

SPEC = importlib.util.spec_from_file_location("native_license_inventory", Path(__file__).with_name("generate.py"))
inventory = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(inventory)

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = "experimental/runtime"
ROOT_PACKAGES = {"pcl-desktop", "pcl-cli"}
OUTPUT_JSON = "native-attribution.json"
OUTPUT_TEXT = "NATIVE-DEPENDENCIES.txt"
REQUIRED_INPUTS = {
    "tools/licenses/native.py", "tools/licenses/generate.py", "Cargo.toml", "Cargo.lock",
    "apps/desktop/package.json", "apps/desktop/package-lock.json",
    "experimental/runtime/package.json", "experimental/runtime/package-lock.json",
}


def command(args, root):
    result = subprocess.run(args, cwd=root, capture_output=True, check=False)
    if result.returncode:
        # Compiler/Cargo/git diagnostics may contain private cache or home paths.
        raise ValueError("Native attribution command failed: " + " ".join(args[:2]))
    return result.stdout


def committed_inputs(root: Path):
    """Bind every dependency declaration, lock and scanner input to one HEAD."""
    commit = command(["git", "rev-parse", "--verify", "HEAD"], root).decode("ascii").strip()
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit):
        raise ValueError("Invalid source commit")
    entries = {}
    for entry in command(["git", "ls-tree", "-r", "-z", "HEAD"], root).split(b"\0"):
        if not entry:
            continue
        header, name = entry.split(b"\t", 1)
        mode, kind, oid = header.decode("ascii").split()
        path = name.decode("utf-8")
        selected = (path in REQUIRED_INPUTS or path == "tools/licenses/upstream-supplements.json"
                    or path.endswith("/Cargo.toml")
                    or path in {".cargo/config", ".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml"})
        if selected:
            if mode not in {"100644", "100755"} or kind != "blob":
                raise ValueError("Linked or unsupported attribution input: " + path)
            entries[path] = oid
    if REQUIRED_INPUTS - entries.keys():
        raise ValueError("Required native attribution inputs are not committed")
    result = []
    for path, oid in sorted(entries.items()):
        local = root / path
        if local.resolve() != local or not local.is_file():
            raise ValueError("Missing or linked attribution input: " + path)
        data = local.read_bytes()
        if command(["git", "cat-file", "blob", oid], root) != data:
            raise ValueError("Attribution input differs from HEAD: " + path)
        result.append({"path": path, "git_blob": oid, "sha256": inventory.sha256(data), "bytes": len(data)})
    return commit, result


def rust_host(root):
    data = command(["rustc", "-vV"], root)
    fields = dict(line.split(": ", 1) for line in data.decode("utf-8").splitlines() if ": " in line)
    host = fields.get("host", "")
    if not re.fullmatch(r"[A-Za-z0-9_]+(?:-[A-Za-z0-9_.]+){2,}", host):
        raise ValueError("rustc -vV did not report a valid native host")
    return host, {"host": host, "release": fields.get("release"), "commit_hash": fields.get("commit-hash"),
                  "verbose_version_sha256": inventory.sha256(data)}


def installed_locations(tree: Path):
    """Enumerate dependency roots, rather than unrelated nested package.json files."""
    found = set()
    pending = [tree / "node_modules"]
    while pending:
        directory = pending.pop()
        if not directory.is_dir():
            continue
        if directory.is_symlink():
            raise ValueError("Linked runtime dependency directory")
        for candidate in sorted(directory.iterdir()):
            if candidate.name.startswith("."):
                continue
            if candidate.is_symlink():
                raise ValueError("Linked runtime package or scope directory")
            packages = sorted(candidate.iterdir()) if candidate.name.startswith("@") and candidate.is_dir() else [candidate]
            for package in packages:
                metadata = package / "package.json"
                if (package.is_symlink() or not package.is_dir() or not metadata.is_file()
                        or not metadata.resolve().is_relative_to(tree.resolve())):
                    raise ValueError("Invalid installed runtime package root")
                found.add(package.relative_to(tree).as_posix())
                pending.append(package / "node_modules")
    return found


class NativeAudit(inventory.Audit):
    def cargo(self):
        args = ["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
                "--filter-platform", inventory.TARGET, "--features", "pcl-desktop/custom-protocol"]
        metadata = json.loads(command(args, self.root))
        packages = {package["id"]: package for package in metadata["packages"]}
        nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
        workspace = set(metadata["workspace_members"])
        roots = {ident for ident in workspace if packages[ident]["name"] in ROOT_PACKAGES}
        if {packages[ident]["name"] for ident in roots} != ROOT_PACKAGES:
            raise ValueError("Native Cargo graph is missing a packaged executable root")

        def reachable(kinds):
            seen, pending = set(roots), list(roots)
            while pending:
                for dependency in nodes[pending.pop()]["deps"]:
                    ident = dependency["pkg"]
                    if ident not in seen and any(edge["kind"] in kinds for edge in dependency["dep_kinds"]):
                        seen.add(ident)
                        pending.append(ident)
            return seen

        normal, production = reachable({None}), reachable({None, "build"})
        locked = tomllib.loads((self.root / "Cargo.lock").read_text(encoding="utf-8"))
        locks = {(package["name"], package["version"], package.get("source")): package for package in locked["package"]}
        supplements = self.supplements()
        rows = []
        for package in sorted(packages.values(), key=lambda p: (p["name"], p["version"])):
            ident = package["id"]
            if ident not in production or ident in workspace:
                continue
            logical = "cargo:" + package["name"] + "@" + package["version"]
            lock = locks.get((package["name"], package["version"], package.get("source")))
            if lock is None:
                raise ValueError(logical + ": Cargo metadata identity is not in Cargo.lock")
            directory = Path(package["manifest_path"]).parent
            manifest = (directory / "Cargo.toml").read_bytes()
            row = {"id": logical, "name": package["name"], "version": package["version"],
                   "declared_license": package.get("license"), "scope": "normal" if ident in normal else "build",
                   "source": package.get("source"), "cargo_lock_checksum": lock.get("checksum"),
                   "manifest_sha256": inventory.sha256(manifest), "features": sorted(nodes[ident]["features"]),
                   "package_metadata": {key: package.get(key) for key in (
                       "authors", "description", "documentation", "homepage", "repository", "edition", "rust_version", "links")},
                   "license_files": self.licenses(directory, logical, package.get("license_file"))}
            if not row["license_files"]:
                records = self.matching_supplements(supplements, "cargo", row)
                if records:
                    row["upstream_supplement_local_checks"] = [
                        self.validate_cargo_supplement(directory, evidence, source, lock.get("checksum"))
                        for source, evidence in records]
                    self.add_supplement_texts(row, records)
            checksum_file = directory / ".cargo-checksum.json"
            if checksum_file.is_file():
                checksum_data = checksum_file.read_bytes()
                checksums = json.loads(checksum_data)
                if checksums.get("package") != lock.get("checksum"):
                    raise ValueError(logical + ": cache checksum differs from Cargo.lock")
                files = checksums.get("files", {})
                checked = {"Cargo.toml": row["manifest_sha256"]}
                checked.update({ref["file"]: ref["text_sha256"] for ref in row["license_files"]
                                if not ref["file"].startswith("upstream:")})
                if any(files.get(name) != digest for name, digest in checked.items()):
                    raise ValueError(logical + ": cached manifest or license text differs from Cargo source checksums")
                row["cargo_checksum_manifest_sha256"] = inventory.sha256(checksum_data)
            rows.append(row)
        if len({row["id"] for row in rows}) != len(rows):
            raise ValueError("Ambiguous Cargo name/version from multiple sources")
        return {"target": inventory.TARGET, "command": args, "root_packages": sorted(ROOT_PACKAGES), "packages": rows,
                "summary": {"external_packages": len(rows), "normal_graph": sum(row["scope"] == "normal" for row in rows),
                            "build_only": sum(row["scope"] == "build" for row in rows)}}

    def npm(self):
        inventory.NPM_TREES = (RUNTIME,)
        locations = installed_locations(self.root / RUNTIME)
        tree = super().npm()[0]
        locked_locations = {row["location"] for row in tree["packages"]}
        if locations - locked_locations:
            raise ValueError("Installed runtime packages are absent from the committed npm lock")
        rows = [row for row in tree["packages"] if row["installed_metadata_verified"]]
        for row in rows:
            data = (self.root / RUNTIME / row["location"] / "package.json").read_bytes()
            metadata = json.loads(data)
            if (metadata.get("name"), metadata.get("version"), metadata.get("license")) != (
                    row["name"], row["version"], row["declared_license"]):
                raise ValueError(row["id"] + ": installed metadata changed during collection")
            row["package_metadata"] = metadata
            row["package_json_sha256"] = inventory.sha256(data)
        missing_required = {row["id"] for row in tree["packages"]
                            if not row["installed_metadata_verified"] and not row["optional"] and row["scope"] == "runtime"}
        selected = {row["id"] for row in rows}
        self.issues = [issue for issue in self.issues if not issue["package"].startswith("npm:")
                       or issue["package"] in selected | missing_required]
        return [{"tree": RUNTIME, "lockfile": RUNTIME + "/package-lock.json", "packages": rows,
                 "summary": {"installed_packages": len(rows), "not_installed_optional": sum(
                     row["optional"] and not row["installed_metadata_verified"] for row in tree["packages"])}}]


def documents(root: Path):
    root = root.resolve()
    source_commit, inputs = committed_inputs(root)
    target, compiler = rust_host(root)
    inventory.TARGET = target
    audit = NativeAudit(root)
    cargo, npm = audit.cargo(), audit.npm()
    rows = cargo["packages"] + [row for tree in npm for row in tree["packages"]]
    failures = []
    used_texts = set()
    for row in rows:
        if not isinstance(row["declared_license"], str) or not row["declared_license"].strip():
            failures.append(row["id"] + ": license_declaration_missing")
        refs = row["license_files"] + row.get("canonical_license_texts", [])
        if not refs:
            failures.append(row["id"] + ": license_text_missing")
        for ref in refs:
            digest = ref.get("text_sha256")
            if digest not in audit.texts or not audit.texts[digest]["bytes"]:
                failures.append(row["id"] + ": missing license text reference")
            else:
                used_texts.add(digest)
    blocking = {"required_package_not_installed", "declared_license_file_unavailable", "license_file_outside_package"}
    failures.extend(issue["package"] + ": " + issue["code"] for issue in audit.issues if issue["code"] in blocking)
    if failures:
        raise ValueError("Native attribution evidence missing: " + "; ".join(sorted(set(failures))))
    texts = [audit.texts[digest] for digest in sorted(used_texts)]
    bundle = ["NATIVE THIRD-PARTY DEPENDENCY LICENSE AND NOTICE TEXTS\n",
              "Companion native-attribution.json records this host, source commit, locked dependencies and evidence warnings.\n",
              "Original notices and separately identified canonical terms retain their provenance.\n"
              "This dependency graph is not proof of emitted binary code or a legal compliance certification.\n"]
    for row in texts:
        bundle.append("\n" + "=" * 78 + "\nSHA-256: " + row["sha256"] + "\nSources:\n"
                      + "".join("  " + source + "\n" for source in sorted(row["sources"]))
                      + "\n" + row["text"] + ("" if row["text"].endswith("\n") else "\n"))
    text = "\n".join(bundle)
    result = {"schema_version": 1, "generator": "tools/licenses/native.py", "source_commit": source_commit,
              "target": target, "compiler": compiler, "inputs": inputs,
              "license_text_bundle": OUTPUT_TEXT, "license_text_bundle_sha256": inventory.sha256(text.encode("utf-8")),
              "scope": {"cargo": "Packaged executable roots with native normal/build dependencies and custom-protocol enabled.",
                        "npm": "Installed, lock-verified packages in experimental/runtime; desktop build dependencies are excluded.",
                        "limits": ["Dependency graphs do not prove which code is emitted into a final executable.",
                                   "Build dependencies may contribute generated code; system libraries, external Node/Java and downloaded game content are not collected.",
                                   "Original evidence warnings remain visible even when canonical declared-license terms are available."]},
              "cargo": cargo, "npm": npm, "issues": sorted(audit.issues, key=lambda issue: (issue["package"], issue["code"])),
              "license_texts": [{"sha256": row["sha256"], "bytes": row["bytes"], "sources": sorted(row["sources"])} for row in texts]}
    # Never rewrite legal texts or upstream names. Abort on actual machine-path
    # leakage; all selected source locations otherwise use logical relative IDs.
    public = json.dumps(result, ensure_ascii=False) + text
    for local in {str(root), root.as_posix(), str(Path.home()), Path.home().as_posix()}:
        if len(local) > 1 and local in public:
            raise ValueError("Native attribution contains a local machine path")
    if committed_inputs(root) != (source_commit, inputs):
        raise ValueError("Source commit or attribution inputs changed during collection")
    return result, text


def write_outputs(output: Path, document, text):
    data = {OUTPUT_JSON: (json.dumps(document, ensure_ascii=False, indent=2) + "\n").encode("utf-8"),
            OUTPUT_TEXT: text.encode("utf-8")}
    output.mkdir(parents=True, exist_ok=True)
    for name, contents in data.items():
        descriptor, temporary = tempfile.mkstemp(prefix=".native-attribution-", dir=output)
        try:
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(contents)
            os.replace(temporary, output / name)
        finally:
            Path(temporary).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="directory for the two native attribution documents")
    args = parser.parse_args()
    try:
        document, text = documents(ROOT)
        write_outputs(args.output, document, text)
    except (ValueError, OSError, KeyError, UnicodeError) as error:
        message = str(error) if isinstance(error, ValueError) and not isinstance(error, UnicodeError) else "Could not read native attribution inputs"
        print("Native attribution failed: " + message, file=sys.stderr)
        return 1
    print(json.dumps({"source_commit": document["source_commit"], "target": document["target"],
                      "cargo_packages": len(document["cargo"]["packages"]),
                      "npm_packages": sum(len(tree["packages"]) for tree in document["npm"]),
                      "issues": dict(Counter(issue["code"] for issue in document["issues"]))}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
