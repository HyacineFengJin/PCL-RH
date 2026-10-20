#!/usr/bin/env python3
"""Check attribution inputs or prepare a deterministic license sidecar.

This deliberately does not run the dependency scanner: a first build or a CI
host may not have the Linux Cargo cache or the same optional npm installation.
The full local scan remains a separate release check.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
DOCUMENTS = (
    "LICENSE", "NOTICE", "COPYRIGHT.md", "THIRD_PARTY_NOTICES.md",
    "THIRDPARTY.generated.json", "LICENSES/DEPENDENCIES.txt", "LICENSES/CORDIS.txt",
    "apps/desktop/src/assets/fonts/LICENSE", "apps/desktop/src/assets/fonts/NOTICE",
    "apps/desktop/src/assets/game-icons/NOTICE", "apps/desktop/src/assets/credits/NOTICE",
    "apps/desktop/src/assets/game-icons/SOURCES.json",
    "apps/desktop/src/assets/game-icons/FABRIC-CC0.md",
    "apps/desktop/src/assets/game-icons/NEOFORGE-CC-BY.txt",
    "apps/desktop/src/assets/game-icons/MINECRAFT-LICENSE.md",
    "apps/desktop/src/assets/game-icons/HMCL-GPL.txt",
    "experimental/NOTICE.md", "experimental/runtime/vendor/maker/LICENSE",
    "experimental/runtime/vendor/porter/LICENSE",
)


def source_bytes(relative: str) -> bytes:
    path = ROOT / relative
    if path.resolve() != path or not path.is_relative_to(ROOT) or not path.is_file():
        raise ValueError("Missing, linked or invalid attribution input: " + relative)
    return path.read_bytes()


def checked_documents():
    contents = {name: source_bytes(name) for name in DOCUMENTS}
    for data in contents.values():
        data.decode("utf-8")
    inventory = json.loads(contents["THIRDPARTY.generated.json"])
    if inventory["schema_version"] != 1:
        raise ValueError("Unsupported dependency inventory version")
    for item in inventory["inputs"]:
        if hashlib.sha256(source_bytes(item["path"])).hexdigest() != item["sha256"]:
            raise ValueError("Stale dependency inventory input: " + item["path"])
    artwork_root = "apps/desktop/src/assets/game-icons/"
    artwork = json.loads(contents[artwork_root + "SOURCES.json"])
    for item in artwork["assets"]:
        name = item["file"]
        if not isinstance(name, str) or Path(name).name != name:
            raise ValueError("Invalid artwork provenance filename")
        if hashlib.sha256(source_bytes(artwork_root + name)).hexdigest() != item["sha256"]:
            raise ValueError("Artwork differs from its recorded source: " + name)
    return contents, dict(sorted(Counter(issue["code"] for issue in inventory["issues"]).items()))


def write_bundle(destination: Path, contents: dict[str, bytes], issues: dict):
    manifest = {
        "schema_version": 1,
        "scope": "Collected source attribution documents; not a binary compliance certification.",
        "evidence_issues": issues,
        "files": [{"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
                  for name, data in contents.items()],
    }
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".license-notices-", dir=destination.parent)
    os.close(descriptor)
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            entries = {**contents, "MANIFEST.json": (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode()}
            for name, data in sorted(entries.items()):
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = 0o100644 << 16
                archive.writestr(info, data, compresslevel=9)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify documents and pinned input hashes without writing")
    parser.add_argument("--output", type=Path, default=ROOT / "target/pcl-rh-license-notices.zip")
    args = parser.parse_args()
    try:
        contents, issues = checked_documents()
        if not args.check:
            write_bundle(args.output, contents, issues)
        print(json.dumps({"documents": len(contents), "evidence_issues": issues, "bundle_written": not args.check}))
    except (ValueError, OSError, KeyError, UnicodeError) as error:
        raise SystemExit("Attribution check failed: " + str(error)) from None


if __name__ == "__main__":
    main()
