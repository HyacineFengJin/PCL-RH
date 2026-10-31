$ErrorActionPreference = 'Stop'
$projectRoot = $PSScriptRoot
$binary = Join-Path $projectRoot '.pcl-rust/bin/pcl-desktop.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw 'Build the launcher first: python tools/build-native.py'
}
$env:PCL_RH_HOME = $projectRoot
$env:PCL_LINUX_HOME = $projectRoot
Set-Location -LiteralPath $projectRoot
& $binary @args
exit $LASTEXITCODE
