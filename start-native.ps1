$ErrorActionPreference = 'Stop'
$projectRoot = $PSScriptRoot
$flatBinary = Join-Path $projectRoot 'pcl-desktop.exe'
$flatItems = @(Get-ChildItem -LiteralPath $projectRoot -Force -ErrorAction Stop |
    Where-Object { $_.Name -eq 'pcl-desktop.exe' })
if ($flatItems.Count -gt 1) {
    throw 'The portable pcl-desktop.exe name is ambiguous; retain the files and repair the archive.'
}
if ($flatItems.Count -eq 1) {
    $flatItem = $flatItems[0]
    if ($flatItem.PSIsContainer -or
        ($flatItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $flatItem.Length -eq 0) {
        throw 'The portable pcl-desktop.exe is invalid; retain it and repair the archive before starting.'
    }
    $binary = $flatBinary
} else {
    # Compatibility with existing nested archives. A present but invalid flat
    # entry must never silently launch a different, older executable instead.
    $binary = Join-Path $projectRoot '.pcl-rust/bin/pcl-desktop.exe'
}
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw 'Build the launcher first: python tools/build-native.py'
}
$env:PCL_RH_HOME = $projectRoot
$env:PCL_LINUX_HOME = $projectRoot
Set-Location -LiteralPath $projectRoot
& $binary @args
exit $LASTEXITCODE
