#!/usr/bin/env bash
# Ephemeral GitHub Linux runner dependencies, never a local setup command.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux || "$(id -u)" == 0 ]]; then
  printf '%s\n' 'This setup runs only as the unprivileged GitHub Linux runner.' >&2
  exit 2
fi

# Official release, including its maintainer-published SHA256:
# https://github.com/containers/bubblewrap/releases/tag/v0.11.1
# The tagged Meson build requires Meson >=0.49.0 and libcap.
# https://github.com/containers/bubblewrap/blob/v0.11.1/meson.build
readonly bwrap_version=0.11.1
readonly bwrap_sha256=c1b7455a1283b1295879a46d5f001dfd088c0bb0f238abb5e128b3583a246f71
tools_work="$(mktemp -d "${RUNNER_TEMP:?}/pcl-rh-code-tools.XXXXXXXX")"
readonly tools_work
trap 'rm -rf -- "$tools_work"' EXIT

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential ca-certificates curl xz-utils pkg-config meson ninja-build \
  libcap-dev libselinux1-dev openjdk-17-jdk-headless

readonly archive="$tools_work/bubblewrap-$bwrap_version.tar.xz"
curl --fail --location --retry 3 --connect-timeout 30 --max-time 120 \
  --proto '=https' --proto-redir '=https' --tlsv1.2 \
  "https://github.com/containers/bubblewrap/releases/download/v$bwrap_version/bubblewrap-$bwrap_version.tar.xz" \
  --output "$archive"
printf '%s  %s\n' "$bwrap_sha256" "$archive" | sha256sum --check --strict
tar --extract --xz --file "$archive" --directory "$tools_work" \
  --no-same-owner --no-same-permissions
meson setup "$tools_work/build" "$tools_work/bubblewrap-$bwrap_version" \
  --prefix=/usr --buildtype=release --wrap-mode=nodownload \
  -Dman=disabled -Dbash_completion=disabled -Dzsh_completion=disabled \
  -Dtests=false -Dselinux=enabled
meson compile -C "$tools_work/build" bwrap
# Only this trusted dependency installation uses sudo. The Rust sandbox and
# synthetic author-code fixtures below run as the ordinary runner, without SUID.
sudo install -o root -g root -m 0755 "$tools_work/build/bwrap" /usr/bin/bwrap
test ! -u /usr/bin/bwrap
test ! -g /usr/bin/bwrap
readlink -f /usr/bin/bwrap
installed_bwrap_version="$(/usr/bin/bwrap --version)"
readonly installed_bwrap_version
printf '%s\n' "$installed_bwrap_version"
test "$installed_bwrap_version" = "bubblewrap $bwrap_version"
/usr/bin/bwrap --help | tee "$tools_work/bwrap-help.txt"
python3 - "$tools_work/bwrap-help.txt" <<'PY'
from pathlib import Path
import sys

help_text = Path(sys.argv[1]).read_text()
if "--disable-userns" not in help_text:
    raise SystemExit("Installed bubblewrap lacks required --disable-userns")
PY

jdk_root="/usr/lib/jvm/java-17-openjdk-$(dpkg --print-architecture)"
readonly jdk_root
for tool in java javac jar; do
  test -x "$jdk_root/bin/$tool"
  sudo update-alternatives --set "$tool" "$jdk_root/bin/$tool"
  resolved_tool="$(readlink -f "/usr/bin/$tool")"
  expected_tool="$(readlink -f "$jdk_root/bin/$tool")"
  printf '%s -> %s\n' "/usr/bin/$tool" "$resolved_tool"
  test "$resolved_tool" = "$expected_tool"
done
java_version="$(/usr/bin/java -version 2>&1)"
javac_version="$(/usr/bin/javac -version 2>&1)"
jar_version="$(/usr/bin/jar --version 2>&1)"
readonly java_version javac_version jar_version
printf '%s\n' "$java_version" "$javac_version" "$jar_version"
[[ "$java_version" == *'version "17.'* ]]
[[ "$javac_version" == 'javac 17.'* ]]
[[ "$jar_version" == 'jar 17.'* ]]
# Tool availability is preparation only. The workflow still runs the unchanged
# real bubblewrap preflight and synthetic compiler/process tests as admission.
