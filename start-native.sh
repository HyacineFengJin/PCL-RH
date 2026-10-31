#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export PCL_RH_HOME="$project_dir"
export PCL_LINUX_HOME="$project_dir"
binary="$project_dir/pcl-desktop"
if [ ! -e "$binary" ] && [ ! -L "$binary" ]; then
    binary="$project_dir/.pcl-rust/bin/pcl-desktop"
fi
if [ ! -x "$binary" ]; then
    printf '请完整解压便携包，或先运行 python tools/build-native.py\n' >&2
    exit 1
fi
cd "$project_dir"
exec "$binary" "$@"
