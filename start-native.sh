#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export PCL_RH_HOME="$project_dir"
export PCL_LINUX_HOME="$project_dir"
binary="$project_dir/.pcl-rust/bin/pcl-desktop"
if [ ! -x "$binary" ]; then
    printf '请先在项目中运行 ./build-native.sh\n' >&2
    exit 1
fi
cd "$project_dir"
exec "$binary" "$@"
