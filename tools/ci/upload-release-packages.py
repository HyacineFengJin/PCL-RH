#!/usr/bin/env python3
"""Copy pinned CI packages to an existing draft, without rebuilding or publishing.

The maintainer uploads release-manifest.json first and supplies its SHA256.
Every package is bound to that manifest, its completed producer and GitHub's
artifact digest. This avoids routing large CI outputs through a local uplink.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
import zipfile

REPOSITORY = "HyacineFengJin/PCL-RH"
MAX_PACKAGE = 512 * 1024**2
RUNNERS = {"linux-x86_64": "ubuntu-24.04", "windows-x86_64": "windows-2025",
           "macos-aarch64": "macos-15", "macos-x86_64": "macos-15-intel"}
FORMS = {(platform, "portable") for platform in RUNNERS} | {
    ("linux-x86_64", "deb"), ("windows-x86_64", "per-user-installer"),
    ("macos-aarch64", "app-archive"), ("macos-x86_64", "app-archive"),
    ("macos-aarch64", "app-dmg"), ("macos-x86_64", "app-dmg")}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def api(endpoint, *, pages=False):
    args = ["gh", "api"] + (["--paginate", "--slurp"] if pages else [])
    return json.loads(subprocess.check_output(args + [f"repos/{REPOSITORY}/" + endpoint], timeout=60))


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def draft(tag):
    releases = [item for page in api("releases?per_page=100", pages=True) for item in page
                if item["tag_name"] == tag]
    require(len(releases) == 1 and releases[0]["draft"] is True, "Expected one existing draft release")
    return releases[0]


def selected_asset(release, name):
    items = [a for a in release["assets"] if a["name"] == name]
    require(len(items) <= 1, "Duplicate release asset")
    return items[0] if items else None


def matches(asset, size, sha):
    return (asset is not None and asset["state"] == "uploaded" and asset["size"] == size
            and asset.get("digest") == "sha256:" + sha)


def packages(document, tag):
    require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag) is not None, "Expected a stable release tag")
    require(document["schema_version"] == 1 and document["tag"] == tag
            and document["version"] == tag[1:], "Manifest version differs from selected release")
    for key in ("source_commit", "release_commit"):
        require(re.fullmatch(r"[0-9a-f]{40}", document[key]) is not None, "Invalid source commit")
    require(type(document["producer_run"]) is int and document["producer_run"] > 0, "Invalid producer")
    result = [row for row in document["assets"] if row["form"] != "attribution-and-original-build-manifests"]
    require(len(result) == len(FORMS) and {(r["platform"], r["form"]) for r in result} == FORMS,
            "Expected exactly the ten native distribution packages")
    require(len({r["name"] for r in result}) == len(result), "Duplicate package name")
    for row in result:
        for key in ("name", "source_artifact_file"):
            require(re.fullmatch(r"[A-Za-z0-9_.+~-]+", row[key]) is not None
                    and row[key] not in (".", ".."), "Unsafe package filename")
        require(type(row["bytes"]) is int and 0 < row["bytes"] <= MAX_PACKAGE, "Invalid package size")
        require(type(row["source_run"]) is int and row["source_run"] > 0, "Invalid package producer")
        require(re.fullmatch(r"[0-9a-f]{64}", row["sha256"]) is not None, "Invalid package digest")
        require(row["name"].startswith("pcl-rh-" + tag[1:] + "-")
                or (row["form"] == "deb" and row["name"].startswith("pcl-rh_" + tag[1:] + "-")),
                "Package name differs from selected version")
    return result


def producer(row, document):
    platform, form = row["platform"], row["form"]
    if form == "portable":
        require(row["source_run"] == document["producer_run"], "Portable belongs to another producer")
        return "official-native.yml", "portable-preview-" + RUNNERS[platform] + "-release", document["source_commit"]
    tool = document["distribution_tool_commits"][platform.split("-", 1)[0]]
    if form == "per-user-installer":
        if row["source_run"] == document["producer_run"]:
            return "official-native.yml", "windows-user-installer-release", document["source_commit"]
        return "windows-installer.yml", "windows-user-installer-retest", tool
    if form == "deb":
        return "linux-distribution.yml", "linux-debian-package", tool
    prefix = "macos-app-" if form == "app-archive" else "macos-app-dmg-"
    return "macos-distribution.yml", prefix + RUNNERS[platform], tool


def artifact(row, document):
    workflow, name, commit = producer(row, document)
    run_id = row["source_run"]
    run = api(f"actions/runs/{run_id}")
    repository = run["repository"]
    require(repository["full_name"] == run["head_repository"]["full_name"] == REPOSITORY
            and repository["id"] == run["head_repository"]["id"]
            and run["head_branch"] == "master" and run["head_sha"] == commit
            and run["path"] == ".github/workflows/" + workflow
            and run["event"] == "workflow_dispatch" and run["status"] == "completed",
            "Package producer is not the pinned completed workflow")
    items = [a for page in api(f"actions/runs/{run_id}/artifacts?per_page=100", pages=True)
             for a in page["artifacts"] if a["name"] == name and not a["expired"]]
    require(len(items) == 1, "Expected one unexpired producer artifact")
    item = items[0]
    witness = item["workflow_run"]
    require(witness["id"] == run_id and witness["head_sha"] == commit
            and witness["repository_id"] == witness["head_repository_id"] == repository["id"]
            and 0 < item["size_in_bytes"] <= MAX_PACKAGE
            and re.fullmatch(r"sha256:[0-9a-f]{64}", item.get("digest", "")) is not None,
            "Artifact identity or digest is invalid")
    return item


def copy_member(archive, destination, row):
    with zipfile.ZipFile(archive) as source:
        entries = source.infolist()
        require(len(entries) <= 1000 and sum(e.file_size for e in entries) <= 2 * 1024**3,
                "Artifact exceeds extraction bounds")
        members = [e for e in entries if e.orig_filename == row["source_artifact_file"]]
        require(len(members) == 1 and not members[0].is_dir() and members[0].file_size == row["bytes"],
                "Expected one package of the pinned size")
        total = 0
        with source.open(members[0]) as incoming, destination.open("xb") as outgoing:
            while chunk := incoming.read(1024 * 1024):
                total += len(chunk)
                require(total <= row["bytes"], "Package exceeds pinned size")
                outgoing.write(chunk)
    require(total == row["bytes"] and digest(destination) == row["sha256"], "Package digest differs from manifest")


def upload(tag, manifest_sha256, output):
    require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag) is not None, "Invalid release tag")
    require(re.fullmatch(r"[0-9a-f]{64}", manifest_sha256) is not None, "Invalid manifest digest")
    release = draft(tag)
    manifest_asset = selected_asset(release, "release-manifest.json")
    require(manifest_asset is not None and 0 < manifest_asset["size"] <= 256 * 1024
            and matches(manifest_asset, manifest_asset["size"], manifest_sha256), "Draft manifest is not pinned")
    output.parent.mkdir(parents=True, exist_ok=True)
    evidence = []
    with tempfile.TemporaryDirectory(prefix="release-packages-", dir=output.parent) as temporary:
        scratch = Path(temporary)
        path = scratch / "release-manifest.json"
        with path.open("xb") as stream:
            subprocess.run(["gh", "api", "-H", "Accept: application/octet-stream",
                            f"repos/{REPOSITORY}/releases/assets/{manifest_asset['id']}"],
                           stdout=stream, check=True, timeout=60)
        require(path.stat().st_size == manifest_asset["size"] and digest(path) == manifest_sha256,
                "Downloaded manifest differs from pinned bytes")
        document = json.loads(path.read_bytes())
        rows = packages(document, tag)
        reference = api("git/ref/tags/" + tag)["object"]
        for _ in range(4):
            if reference["type"] == "commit":
                break
            require(reference["type"] == "tag", "Unexpected tag object")
            reference = api("git/tags/" + reference["sha"])["object"]
        require(reference["type"] == "commit" and reference["sha"] == document["release_commit"],
                "Release tag differs from the manifest")
        for row in rows:
            current = draft(tag)
            require(current["id"] == release["id"] and matches(selected_asset(current, "release-manifest.json"),
                    manifest_asset["size"], manifest_sha256), "Draft changed during upload")
            existing = selected_asset(current, row["name"])
            if existing is not None:
                require(matches(existing, row["bytes"], row["sha256"]), "Existing asset differs; refusing to overwrite")
                print("Already verified: " + row["name"], flush=True)
                continue
            item = artifact(row, document)
            archive = scratch / "artifact.zip"
            with archive.open("wb") as stream:
                subprocess.run(["gh", "api", f"repos/{REPOSITORY}/actions/artifacts/{item['id']}/zip"],
                               stdout=stream, check=True, timeout=180)
            require(archive.stat().st_size <= MAX_PACKAGE and "sha256:" + digest(archive) == item["digest"],
                    "Artifact download differs from GitHub digest")
            package = scratch / row["name"]
            copy_member(archive, package, row)
            # Publishing and replacement are deliberately separate maintainer actions.
            subprocess.run(["gh", "release", "upload", tag, str(package), "--repo", REPOSITORY],
                           check=True, timeout=300)
            uploaded = selected_asset(draft(tag), row["name"])
            require(matches(uploaded, row["bytes"], row["sha256"]), "Uploaded package failed readback")
            evidence.append({"name": row["name"], "sha256": row["sha256"], "artifact_id": item["id"]})
            package.unlink()
            archive.unlink()
            print("Uploaded and verified: " + row["name"], flush=True)
    output.write_text(json.dumps({"tag": tag, "manifest_sha256": manifest_sha256,
                                  "published": False, "uploaded": evidence}, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    upload(args.tag, args.manifest_sha256, args.output)
