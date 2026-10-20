#!/usr/bin/env python3
"""Generate an offline dependency/license inventory from pinned local inputs.

Only Cargo metadata is executed; package code, npm scripts, builds, installations,
network access, native payloads, and applications are never invoked. See README.md
in this directory for the scope and the meaning of --strict.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

TARGET = "x86_64-unknown-linux-gnu"
NPM_TREES = ("apps/desktop", "experimental/runtime")
SOURCE_SUFFIXES = {".map", ".py", ".js", ".mjs", ".ts", ".rs", ".c", ".h", ".cc", ".cpp", ".pm", ".pl", ".sh"}
COPYRIGHT = re.compile(r"^\s*(?:[#/*]+\s*)?(?:Copyright\s*(?:\(c\)|©|[0-9])|©|SPDX-FileCopyrightText:|PackageCopyrightText:)", re.I)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


class Audit:
    def __init__(self, root: Path):
        self.root = root
        self.texts: dict[str, dict] = {}
        self.issues: list[dict] = []

    def issue(self, code: str, package: str, detail: str, strict: bool = True):
        self.issues.append({"code": code, "package": package, "detail": detail, "fails_strict": strict})

    def text(self, data: bytes, source: str) -> str:
        # Preserve legal text, including upstream names and attribution addresses.
        # Source locations use logical package IDs, never cache/home paths.
        digest = sha256(data)
        text = data.decode("utf-8")
        row = self.texts.setdefault(digest, {"sha256": digest, "bytes": len(data), "sources": [], "text": text})
        if source not in row["sources"]:
            row["sources"].append(source)
        return digest

    def licenses(self, package_root: Path, package_id: str, declared_file: str | None = None):
        files: set[Path] = set()
        for directory, dirs, names in os.walk(package_root, followlinks=False):
            dirs[:] = sorted(x for x in dirs if x not in {"node_modules", ".git", "target"})
            for name in names:
                if name.lower().startswith(("license", "licence", "copying", "copyright", "notice")) and Path(name).suffix.lower() not in SOURCE_SUFFIXES:
                    files.add(Path(directory) / name)
        if declared_file:
            candidate = package_root / declared_file
            if candidate.is_file() and candidate.resolve().is_relative_to(package_root.resolve()):
                files.add(candidate)
            else:
                self.issue("declared_license_file_unavailable", package_id, "The manifest's license-file is unavailable inside its package.")
        refs = []
        for path in sorted(files):
            if not path.is_file():
                continue
            relative = path.relative_to(package_root).as_posix()
            if not path.resolve().is_relative_to(package_root.resolve()):
                self.issue("license_file_outside_package", package_id, relative)
                continue
            data = path.read_bytes()
            digest = self.text(data, package_id + "/" + relative)
            refs.append({"file": relative, "text_sha256": digest})
        return refs

    def supplements(self):
        path = self.root / "tools/licenses/upstream-supplements.json"
        if not path.is_file():
            return {}
        document = read_json(path)
        supplements = {}
        for source in document["sources"]:
            data = source["license_text"].encode("utf-8")
            if sha256(data) != source["license_sha256"]:
                raise ValueError("Upstream supplement license hash mismatch")
            for package in source["verified_packages"]:
                ecosystem = package.get("ecosystem", source.get("ecosystem", "npm"))
                if ecosystem not in {"npm", "cargo"}:
                    raise ValueError("Unsupported upstream supplement ecosystem")
                key = (ecosystem, package["name"], package["version"], package["declared_license"])
                records = supplements.setdefault(key, [])
                if any(previous[0]["license_url"] == source["license_url"] for previous in records):
                    raise ValueError("Duplicate upstream supplement document for a package")
                records.append((source, package))
        return supplements

    def matching_supplements(self, supplements, ecosystem, row):
        if not isinstance(row["declared_license"], str):
            if any(candidate[:3] == (ecosystem, row["name"], row["version"]) for candidate in supplements):
                raise ValueError(row["id"] + ": declared license differs from verified upstream evidence")
            return []
        key = (ecosystem, row["name"], row["version"], row["declared_license"])
        if key not in supplements and any(candidate[:3] == key[:3] for candidate in supplements):
            raise ValueError(row["id"] + ": declared license differs from verified upstream evidence")
        return supplements.get(key, [])

    def add_supplement_texts(self, row, records):
        """Preserve each original document and its evidence; one package may need several."""
        provenance = []
        for source, evidence in records:
            digest = self.text(source["license_text"].encode("utf-8"), source["license_url"])
            row["license_files"].append({"file": "upstream:" + source.get("source_file", "LICENSE"), "text_sha256": digest})
            # Keep version-specific branches and source-header evidence visible.
            # Never replace the package's declared expression with a verified OR branch.
            provenance.append({
                "source": {k: v for k, v in source.items() if k not in {"license_text", "verified_packages"}},
                "package_verification": dict(evidence),
            })
        row["upstream_supplements"] = provenance

    def validate_cargo_supplement(self, package_root, evidence, source, lock_checksum):
        """Validate local provenance without executing or extracting package contents."""
        commit = evidence.get("cargo_vcs_commit", source.get("source_commit"))
        if not commit or source.get("source_commit") not in {None, commit}:
            raise ValueError("Cargo upstream supplement has inconsistent commit evidence")
        vcs = read_json(package_root / ".cargo_vcs_info.json")
        if vcs.get("git", {}).get("sha1") != commit or vcs.get("git", {}).get("dirty"):
            raise ValueError("Cargo package vcs commit differs from verified upstream evidence")
        if vcs.get("path_in_vcs", "") != evidence["repository_directory"]:
            raise ValueError("Cargo package source directory differs from verified upstream evidence")
        original = package_root / "Cargo.toml.orig"
        if sha256(original.read_bytes()) != evidence["published_original_manifest_sha256"]:
            raise ValueError("Cargo original manifest differs from verified published evidence")
        expected = evidence["published_archive_sha256"]
        if not lock_checksum or expected != lock_checksum or evidence["cargo_lock_checksum"] != lock_checksum:
            raise ValueError("Cargo.lock checksum differs from verified published evidence")
        for file in evidence.get("cached_source_files", []):
            relative = Path(file["file"])
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError("Invalid Cargo supplement source evidence path")
            local = package_root / relative
            if not local.resolve().is_relative_to(package_root.resolve()) or sha256(local.read_bytes()) != file["sha256"]:
                raise ValueError("Cargo source header differs from verified published evidence")
        # Cargo's source cache and archive cache are siblings. If an archive has
        # been evicted, preserve that fact; Cargo.lock/vcs evidence still applies.
        archive = None
        if package_root.parent.parent.name == "src":
            candidate = package_root.parent.parent.parent / "cache" / package_root.parent.name / (package_root.name + ".crate")
            if candidate.is_file():
                archive = candidate
        if archive is not None:
            with archive.open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            if digest != lock_checksum:
                raise ValueError("Cargo cached archive checksum differs from Cargo.lock")
        return {"cargo_vcs_commit_verified": True, "original_manifest_verified": True,
                "cargo_lock_checksum_verified": True, "cached_archive_available": archive is not None,
                "cached_archive_checksum_verified": archive is not None}

    def npm(self):
        result = []
        supplements = self.supplements()
        for folder in NPM_TREES:
            directory = self.root / folder
            manifest = read_json(directory / "package.json")
            lock = read_json(directory / "package-lock.json")
            locked_root = lock["packages"][""]
            for section in ("dependencies", "devDependencies", "optionalDependencies"):
                if manifest.get(section, {}) != locked_root.get(section, {}):
                    raise ValueError(folder + ": manifest and lockfile dependency declarations differ")
            packages = []
            for location, pinned in sorted(lock["packages"].items()):
                if not location:
                    continue
                # Current locks use ordinary registry packages. Refuse future local
                # links or path escapes instead of traversing unrelated directories.
                parts = Path(location).parts
                if Path(location).is_absolute() or ".." in parts or not location.startswith("node_modules/") or pinned.get("link"):
                    raise ValueError(folder + ": unsupported lockfile package location")
                name = location.rsplit("node_modules/", 1)[-1]
                version = pinned.get("version")
                ident = "npm:" + folder + ":" + name + "@" + str(version) + ":" + location
                package_path = directory / location
                installed = (package_path / "package.json").is_file()
                license_value = pinned.get("license")
                row = {"id": ident, "name": name, "version": version, "location": location,
                       "declared_license": license_value, "scope": "development" if pinned.get("dev") else "runtime",
                       "direct_runtime": location == "node_modules/" + name and name in manifest.get("dependencies", {}),
                       "direct_development": location == "node_modules/" + name and name in manifest.get("devDependencies", {}),
                       "optional": bool(pinned.get("optional")), "os": pinned.get("os", []), "cpu": pinned.get("cpu", []),
                       "integrity": pinned.get("integrity"), "installed_metadata_verified": installed, "license_files": []}
                if installed:
                    metadata = read_json(package_path / "package.json")
                    if (metadata.get("name"), metadata.get("version"), metadata.get("license")) != (name, version, license_value):
                        raise ValueError(ident + ": installed name/version/license differs from the lockfile")
                    row["license_files"] = self.licenses(package_path, ident)
                else:
                    self.issue("optional_package_not_installed" if row["optional"] else "required_package_not_installed", ident,
                               "Only lockfile metadata is available; package license/notice files were not scanned.", strict=not row["optional"])
                if installed and not row["license_files"]:
                    records = self.matching_supplements(supplements, "npm", row)
                    if records:
                        for _, evidence in records:
                            integrity = evidence.get("registry_integrity")
                            if integrity is not None and integrity != row["integrity"]:
                                raise ValueError(ident + ": lockfile integrity differs from verified registry evidence")
                        self.add_supplement_texts(row, records)
                if installed and not row["license_files"]:
                    self.issue("license_text_missing", ident, "Installed archive has no independently named license/notice file; a license declaration alone is not its complete legal text.")
                if not license_value:
                    self.issue("license_declaration_missing", ident, "The lockfile has no license declaration.")
                packages.append(row)
            result.append({"tree": folder, "lockfile": folder + "/package-lock.json", "packages": packages,
                           "summary": {"locked_packages": len(packages), "runtime": sum(p["scope"] == "runtime" for p in packages),
                                       "development": sum(p["scope"] == "development" for p in packages),
                                       "installed_metadata_verified": sum(p["installed_metadata_verified"] for p in packages),
                                       "license_counts": dict(sorted(Counter(str(p["declared_license"]) for p in packages).items()))}})
        return result

    def cargo(self):
        command = ["cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", TARGET]
        completed = subprocess.run(command, cwd=self.root, capture_output=True, text=True, check=False)
        if completed.returncode:
            # Cargo diagnostics can contain local paths; keep public output clean.
            raise ValueError("Offline Cargo metadata failed; no files generated. Required target dependencies must already be cached. Run the displayed Cargo command locally for diagnostics.")
        metadata = json.loads(completed.stdout)
        lock = tomllib.loads((self.root / "Cargo.lock").read_text(encoding="utf-8"))
        lock_checksums = {(p["name"], p["version"], p.get("source")): p.get("checksum") for p in lock["package"]}
        supplements = self.supplements()
        roots = set(metadata["workspace_members"])
        packages = {p["id"]: p for p in metadata["packages"]}
        nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}

        def reachable(kinds):
            seen = set(roots)
            pending = list(roots)
            while pending:
                node = nodes[pending.pop()]
                for dependency in node["deps"]:
                    if any(d["kind"] in kinds for d in dependency["dep_kinds"]) and dependency["pkg"] not in seen:
                        seen.add(dependency["pkg"])
                        pending.append(dependency["pkg"])
            return seen

        normal = reachable({None})
        production = reachable({None, "build"})
        direct: dict[str, list] = {}
        for workspace in sorted(roots):
            for dependency in nodes[workspace]["deps"]:
                for kind in dependency["dep_kinds"]:
                    direct.setdefault(dependency["pkg"], []).append({"workspace": packages[workspace]["name"], "kind": kind["kind"] or "normal", "target": kind["target"]})
        entries = []
        for package in sorted(packages.values(), key=lambda p: (p["name"], p["version"])):
            if package["id"] in roots:
                continue
            ident = "cargo:" + package["name"] + "@" + package["version"]
            row = {"id": ident, "name": package["name"], "version": package["version"], "declared_license": package.get("license"),
                   "scope": "normal" if package["id"] in normal else "build" if package["id"] in production else "development",
                   "workspace_dependency_edges": sorted(direct.get(package["id"], []), key=lambda x: (x["workspace"], x["kind"], x["target"] or "")),
                   "features": sorted(nodes[package["id"]]["features"]), "license_files": self.licenses(Path(package["manifest_path"]).parent, ident, package.get("license_file"))}
            if not row["license_files"]:
                records = self.matching_supplements(supplements, "cargo", row)
                if records:
                    checks = []
                    for source, evidence in records:
                        checks.append(self.validate_cargo_supplement(
                            Path(package["manifest_path"]).parent, evidence, source,
                            lock_checksums.get((package["name"], package["version"], package.get("source"))),
                        ))
                    self.add_supplement_texts(row, records)
                    row["upstream_supplement_local_checks"] = checks
            if not row["license_files"]:
                self.issue("license_text_missing", ident, "Cached package has no independently named license/notice file; source headers and upstream originals need separate review.")
            if not row["declared_license"]:
                self.issue("license_declaration_missing", ident, "Package metadata has no license declaration.")
            entries.append(row)
        return {"target": TARGET, "command": command, "workspace_packages": sorted(packages[p]["name"] for p in roots), "packages": entries,
                "summary": {"external_packages": len(entries), "normal_graph": sum(p["scope"] == "normal" for p in entries),
                            "build_only": sum(p["scope"] == "build" for p in entries), "development_only": sum(p["scope"] == "development" for p in entries),
                            "license_counts": dict(sorted(Counter(str(p["declared_license"]) for p in entries).items()))}}

    def documents(self):
        npm = self.npm()
        cargo = self.cargo()
        inputs = ["tools/licenses/generate.py", "Cargo.lock", "Cargo.toml", "apps/desktop/src-tauri/Cargo.toml"]
        inputs += [str(p.relative_to(self.root).as_posix()) for p in sorted((self.root / "crates").glob("*/Cargo.toml"))]
        inputs += [folder + "/" + name for folder in NPM_TREES for name in ("package.json", "package-lock.json")]
        supplements = self.root / "tools/licenses/upstream-supplements.json"
        if supplements.is_file():
            inputs.append("tools/licenses/upstream-supplements.json")
        # Check only local path leakage. Upstream copyright names/addresses are
        # required legal attributions and must never be erased by this generator.
        for record in self.texts.values():
            for local in {str(self.root), str(Path.home())}:
                if local in record["text"]:
                    raise ValueError("A license text contains a local machine path; review it before publication")
        inventory = {"schema_version": 1, "generator": "tools/licenses/generate.py", "license_text_bundle": "LICENSES/DEPENDENCIES.txt",
                     "scope": {"cargo_target": TARGET, "other_cargo_targets_scanned": False, "npm_trees": list(NPM_TREES),
                               "purpose": "Source dependency attribution inventory, not a complete binary distribution compliance certification.",
                               "limits": ["Package dependency graphs do not prove which code is emitted into final binaries or JavaScript bundles.",
                                          "Cargo normal graph includes proc-macro crates; build-only crates may also contribute generated code.",
                                          "Named license/notice files are collected recursively, including vendored libraries; their containing package license does not replace nested terms.",
                                          "Embedded payloads, source-file-only headers, operating-system libraries, other platforms, downloaded game/JVM content, and non-package assets require separate review.",
                                          "Absent optional npm packages have only lockfile declarations; no license text or installed metadata is claimed for them.",
                                          "License declarations, automated copyright-line extraction, and strict success do not establish ownership or legal compatibility."]},
                     "inputs": [{"path": p, "sha256": sha256((self.root / p).read_bytes())} for p in sorted(set(inputs))],
                     "npm": npm, "cargo": cargo, "issues": sorted(self.issues, key=lambda x: (x["code"], x["package"])),
                     "license_texts": [{k: row[k] for k in ("sha256", "bytes", "sources")} for _, row in sorted(self.texts.items())]}
        for row in inventory["license_texts"]:
            row["sources"].sort()
            row["copyright_lines"] = [line.strip() for line in self.texts[row["sha256"]]["text"].splitlines() if COPYRIGHT.match(line)]
        bundle = ["THIRD-PARTY DEPENDENCY LICENSE AND NOTICE TEXTS\n",
                  "Generated by tools/licenses/generate.py. The companion THIRDPARTY.generated.json\nrecords package versions, scope, text hashes and explicit evidence gaps.\n",
                  "This file preserves upstream terms. A package's declared license does not\nreplace nested licenses. No binary or all-platform compliance certification is made.\n"]
        for digest, row in sorted(self.texts.items()):
            bundle.append("\n" + "=" * 78 + "\nSHA-256: " + digest + "\nSources:\n" + "".join("  " + s + "\n" for s in sorted(row["sources"])) + "\n" + row["text"] + ("" if row["text"].endswith("\n") else "\n"))
        return inventory, "\n".join(bundle)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="compare regenerated bytes with checked-in outputs without writing")
    parser.add_argument("--strict", action="store_true", help="fail on missing required packages or missing license declarations/text; absent optional packages are informational")
    arguments = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    audit = Audit(root)
    try:
        inventory, bundle = audit.documents()
    except (ValueError, OSError, KeyError, UnicodeError, json.JSONDecodeError) as error:
        # Avoid printing exception values such as paths or untrusted metadata.
        if isinstance(error, ValueError) and not isinstance(error, (UnicodeError, json.JSONDecodeError)):
            print("License inventory failed: " + str(error), file=sys.stderr)
        else:
            print("License inventory failed while reading package metadata/license texts; no outputs changed.", file=sys.stderr)
        return 1
    outputs = {"THIRDPARTY.generated.json": json.dumps(inventory, ensure_ascii=False, indent=2) + "\n", "LICENSES/DEPENDENCIES.txt": bundle}
    if arguments.check:
        stale = [name for name, text in outputs.items() if not (root / name).is_file() or (root / name).read_bytes() != text.encode("utf-8")]
        if stale:
            print("Generated license files are stale: " + ", ".join(stale), file=sys.stderr)
            return 1
    else:
        for name, text in outputs.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
    counts = Counter(issue["code"] for issue in inventory["issues"])
    print(json.dumps({"cargo_linux_packages": inventory["cargo"]["summary"]["external_packages"], "npm_locked_packages": sum(x["summary"]["locked_packages"] for x in inventory["npm"]), "unique_license_texts": len(inventory["license_texts"]), "issues": dict(sorted(counts.items()))}, sort_keys=True))
    if arguments.strict and any(issue["fails_strict"] for issue in inventory["issues"]):
        print("Strict evidence check failed; inspect the inventory's issues. This is not a legal compliance decision.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
