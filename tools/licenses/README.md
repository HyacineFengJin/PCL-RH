# Offline dependency license inventory

From the repository root, run:

```sh
python3 tools/licenses/generate.py
python3 tools/licenses/generate.py --check
python3 tools/licenses/generate.py --check --strict
```

Python 3.11+, Cargo, locally cached Linux Cargo dependencies, and existing npm
installations are required. The script never downloads or installs packages,
executes package scripts, or builds the application. Its only child process is:

```sh
cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-unknown-linux-gnu
```

`THIRDPARTY.generated.json` records exact versions, direct dependency edges,
scopes, source-input hashes, license declarations, and explicit evidence gaps.
`LICENSES/DEPENDENCIES.txt` preserves complete independently named license and
notice files, including those nested inside vendored package sources. Identical
texts share one SHA-256 reference. Upstream copyright notices remain intact.
Generated sources use logical package paths rather than local cache paths.
The collected text bundle keeps upstream line endings and whitespace; its
narrow Git attributes prevent normalization or whitespace cleanup of originals.

The npm lockfiles cover both desktop and experimental runtime dependencies.
Installed package names, versions, and license declarations must match their
lockfile entries; mismatches abort before outputs are written. Missing optional
packages are recorded separately from missing required packages and missing
license texts. Optional packages still need their texts checked if shipped on a
platform where they are installed.

Cargo scope is the Linux dependency graph. `normal` includes proc-macro crates;
`build` means reachable only through a build dependency; `development` means
reachable only through development dependencies. This classification does not
establish which code appears in a final executable or generated output. Other
Cargo targets, system libraries, source-file-only headers, native or bundled
payloads, downloaded content, and non-package assets require separate review.
A containing package's MIT declaration does not replace nested library terms.

Default generation succeeds with explicit gaps so the inventory remains useful
for a source release. `--check` compares bytes without writing. `--strict` returns
exit status 2 if required installed packages, license declarations, or independent
license/notice texts are missing. Absent optional packages are informational.
Strict success means those evidence checks passed; it does not certify copyright
ownership, compatibility, final binary compliance, or completeness across all
platforms. Do not edit generated outputs by hand.

## Verified upstream supplements

Some published npm archives omit their upstream license file.
`upstream-supplements.json` preserves separately verified, exact-version upstream
texts and evidence. Generation reads this file locally and never fetches URLs.
Sources are pinned to the corresponding official release commits and retain
original legal texts and manifest evidence. The Pi supplement uses the official
`v1.0.4` tag's commit. Other npm records include exact release provenance; when
registry integrity is recorded, it must also match the lockfile. Installed
name/version/license must match before a supplement is used. An author's name
alone is never a copyright substitute.

Cargo supplements verify the local VCS commit, original manifest, Cargo.lock
checksum and cached archive when present. One package can reference several
documents. A verified branch of an OR expression does not prove that the other
branch's text was collected. Source-header evidence can identify an official,
versioned license text without inventing a copyright owner.

For dependency updates, inspect each missing archive and its exact upstream
version. Add a supplement only after verifying the relevant repository, package
identity, version, license scope, source commit, and original legal text. Retain
missing evidence when it cannot be verified; never substitute a repository's
current branch for an older package version. Regenerate and review the resulting
inventory and license text changes before distribution.

## Application and release copies

The About page includes an offline reader for the fixed attribution documents.
Vite loads each text on demand and the production custom-protocol build embeds
those assets in the executable. No arbitrary local file reader or remote fetch
is used, and the large dependency texts are not part of the initial JavaScript.

For a source-only check or CI without installed dependencies:

```sh
python3 tools/licenses/release.py --check
```

This checks the 14 bundled documents and recorded dependency-input hashes. It
does not replace a fresh `generate.py --check` or upgrade evidence gaps to passes.
The license workflow runs this check and the scanner's evidence regressions.

To prepare a separate attribution archive alongside an application release:

```sh
python3 tools/licenses/release.py
```

The default output is `target/pcl-rh-license-notices.zip`. It preserves the
documents' original bytes and includes a path/hash manifest and evidence-gap
counts. The archive is deterministic for the same inputs. Other platform or
binary-specific obligations still require review of that actual distribution.
