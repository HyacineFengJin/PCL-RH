#!/usr/bin/env python3
"""Fetch one authenticated official portable candidate for native distribution QA.

The producer's workflow identity and the unique, unexpired artifact's repository,
commit and SHA256 bind the bytes before extraction. A failed later installer
check does not invalidate an already verified portable artifact. This downloads
only; actual native executable checks remain a separate required step.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import platform
import re
import stat
import subprocess
import tempfile
import zipfile


def require(condition, message):
    if not condition:
        raise ValueError(message)


def native_target() -> tuple[str, str, str]:
    system = platform.system()
    arch = platform.machine().lower()
    targets = {
        ('Linux', 'x86_64'): ('linux', 'x86_64', 'ubuntu-24.04'),
        ('Windows', 'amd64'): ('windows', 'x86_64', 'windows-2025'),
        ('Windows', 'x86_64'): ('windows', 'x86_64', 'windows-2025'),
        ('Darwin', 'arm64'): ('macos', 'aarch64', 'macos-15'),
        ('Darwin', 'x86_64'): ('macos', 'x86_64', 'macos-15-intel'),
    }
    require((system, arch) in targets, 'Unsupported native candidate host')
    name, cpu, runner = targets[(system, arch)]
    return name, cpu, f'portable-preview-{runner}-release'


def fetch(run_id: str, destination: Path, evidence: Path) -> None:
    repository = 'HyacineFengJin/PCL-RH'
    system, arch, artifact_name = native_target()
    if re.fullmatch(r'[1-9][0-9]{0,19}', run_id) is None:
        raise SystemExit('source_run must be a positive numeric workflow run ID')

    def api(endpoint, *, pages=False):
        command = ['gh', 'api', '-H', 'Accept: application/vnd.github+json',
                   '-H', 'X-GitHub-Api-Version: 2022-11-28']
        if pages:
            command += ['--paginate', '--slurp']
        return json.loads(subprocess.check_output(command + [endpoint], timeout=60))

    run = api(f'repos/{repository}/actions/runs/{run_id}')
    owner = run.get('repository') or {}
    head = run.get('head_repository') or {}
    commit = run.get('head_sha', '')
    require(run.get('id') == int(run_id)
            and owner.get('full_name') == repository and head.get('full_name') == repository
            and type(owner.get('id')) is int and owner['id'] == head.get('id')
            and run.get('head_branch') == 'master' and run.get('event') == 'workflow_dispatch'
            and run.get('path') == '.github/workflows/official-native.yml'
            and run.get('status') == 'completed'
            and re.fullmatch(r'[0-9a-f]{40}', commit) is not None,
            'The source must be a completed official-native dispatch from this repository master')
    # A portable package can be valid even when a later installer check
    # or another platform failed. Its exact artifact must still be unique.
    pages = api(f'repos/{repository}/actions/runs/{run_id}/artifacts'
                f'?name={artifact_name}&per_page=100', pages=True)
    artifacts = [item for page in pages for item in page['artifacts']
                 if item.get('name') == artifact_name and item.get('expired') is False]
    require(len(artifacts) == 1, 'Expected exactly one native release portable artifact')
    artifact = artifacts[0]
    require(type(artifact.get('size_in_bytes')) is int
            and 0 < artifact['size_in_bytes'] <= 512 * 1024**2,
            'Selected artifact exceeds the download size bound')
    witness = artifact.get('workflow_run') or {}
    require(artifact.get('expired') is False
            and datetime.fromisoformat(artifact['expires_at'].replace('Z', '+00:00')) > datetime.now(timezone.utc)
            and type(artifact.get('id')) is int and artifact['id'] > 0
            and witness.get('id') == int(run_id)
            and witness.get('repository_id') == owner['id']
            and witness.get('head_repository_id') == owner['id']
            and witness.get('head_branch') == 'master' and witness.get('head_sha') == commit,
            'The artifact is expired or belongs to another producer')
    expected = artifact.get('digest', '')
    require(isinstance(expected, str) and re.fullmatch(r'sha256:[0-9a-f]{64}', expected) is not None,
            'The source artifact must have a GitHub SHA256 digest')
    require(not destination.exists(), 'Refusing to replace an existing downloaded payload')
    destination.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix='pcl-installer-artifact-', dir=os.environ['RUNNER_TEMP']) as temporary:
        archive = Path(temporary) / 'portable-artifact.zip'
        with archive.open('xb') as output:
            subprocess.run(['gh', 'api', f'repos/{repository}/actions/artifacts/{artifact["id"]}/zip'],
                           stdout=output, check=True, timeout=180)
        require(archive.stat().st_size <= 512 * 1024**2, 'Artifact archive exceeds download bounds')
        with archive.open('rb') as source:
            require('sha256:' + hashlib.file_digest(source, 'sha256').hexdigest() == expected,
                    'Downloaded artifact differs from the selected GitHub digest')
        with zipfile.ZipFile(archive) as source:
            entries = source.infolist()
            require(len(entries) <= 1000 and sum(entry.file_size for entry in entries) <= 2 * 1024**3,
                    'Artifact contents exceed extraction bounds')
            names = set()
            for entry in entries:
                # ZipInfo.filename normalizes backslashes on Windows and
                # truncates NULs. Validate the original spelling before the
                # extractor can erase evidence of aliases or control bytes.
                name = entry.orig_filename.rstrip('/')
                parts = name.split('/')
                reserved = {'CON', 'PRN', 'AUX', 'NUL', 'CONIN$', 'CONOUT$'}
                mode = entry.external_attr >> 16
                require(name and not name.startswith('/') and not PureWindowsPath(name).drive
                        and '\\' not in name and all(part not in ('', '.', '..') for part in parts)
                        and not any(ord(c) < 32 or c in ':*?<>|"{};' for c in name)
                        and all(not part.endswith(('.', ' ')) for part in parts)
                        and all(part.split('.', 1)[0].upper() not in reserved
                                and re.fullmatch(r'(?:COM|LPT)[1-9¹²³]', part.split('.', 1)[0].upper()) is None
                                for part in parts)
                        and name.casefold() not in names
                        and stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR),
                        'Unsafe or duplicate artifact member')
                names.add(name.casefold())
            source.extractall(destination)
    payload = json.loads((destination / 'package-manifest.json').read_text(encoding='utf-8'))
    require(payload.get('platform') == system and payload.get('arch') == arch
            and payload.get('profile') == 'release' and payload.get('commit') == commit,
            'The downloaded manifest does not describe the selected native release producer')
    require(not evidence.exists(), 'Refusing to replace existing producer evidence')
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(json.dumps({'source_run': int(run_id), 'source_commit': commit,
                                   'source_conclusion': run.get('conclusion'),
                                   'artifact_id': artifact['id'], 'artifact_digest': expected,
                                   'distribution_tool_commit': os.environ['GITHUB_SHA']}, indent=2) + '\n',
                        encoding='utf-8')
    # Preserve GITHUB_SHA as the distribution-tool revision, not the producer.
    with open(os.environ['GITHUB_ENV'], 'a', encoding='utf-8') as environment:
        environment.write('PCL_NATIVE_SOURCE_COMMIT=' + commit + '\n')
    print(json.dumps({'source_run': int(run_id), 'source_commit': commit, 'artifact_id': artifact['id']}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-run', required=True)
    parser.add_argument('--destination', type=Path, default=Path('target/native-packages'))
    parser.add_argument('--evidence', type=Path, default=Path('target/native-installers/source-run.json'))
    args = parser.parse_args()
    fetch(args.source_run, args.destination, args.evidence)
