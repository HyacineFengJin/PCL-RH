#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$project_dir"
# Maintainer builds can reuse a local private file without putting the key in
# command arguments. Explicit build credentials always take precedence.
if [ "${PCL_RH_CURSEFORGE_API_KEY+x}" != x ] && [ "${PCL_RH_CURSEFORGE_API_KEY_FILE+x}" != x ] && [ -f "$project_dir/.pcl-rust/maintainer/curseforge-api-key" ]; then
    export PCL_RH_CURSEFORGE_API_KEY_FILE="$project_dir/.pcl-rust/maintainer/curseforge-api-key"
fi
./prepare-icon.sh
npm ci --prefix apps/desktop --no-audit --no-fund
# Pi stays in its own locked dependency tree; package lifecycle scripts are not needed.
npm ci --prefix experimental/runtime --ignore-scripts --no-audit --no-fund
npm run build --prefix apps/desktop
# A bounded compile avoids excessive memory use on laptops.
cargo build --locked -p pcl-desktop -p pcl-cli --features pcl-desktop/custom-protocol -j "${PCL_BUILD_JOBS:-2}"
mkdir -p .pcl-rust/bin
cp target/debug/pcl-desktop .pcl-rust/bin/pcl-desktop.new
chmod +x .pcl-rust/bin/pcl-desktop.new
mv .pcl-rust/bin/pcl-desktop.new .pcl-rust/bin/pcl-desktop
printf '已构建原生桌面应用。运行 ./start-native.sh\n'
