# Only disposable Actions runners prepare this prerequisite automatically.
# Distributed installers never download or install a WebView2 runtime.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
  throw 'WebView2 preparation is restricted to a disposable Windows Actions runner'
}

$clients = @(
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
  'HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
)
foreach ($client in $clients) {
  $version = (Get-ItemProperty -LiteralPath $client -ErrorAction SilentlyContinue).pv
  if ($version -match '^\d+\.\d+\.\d+\.\d+$' -and [version]$version -gt [version]'0.0.0.0') {
    Write-Output "Using installed WebView2 $version"
    exit 0
  }
}
$bootstrap = Join-Path $env:RUNNER_TEMP 'MicrosoftEdgeWebview2Setup.exe'
Invoke-WebRequest 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile $bootstrap
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
  throw 'WebView2 bootstrapper must have a valid Microsoft signature'
}
$runtime = Start-Process -FilePath $bootstrap -ArgumentList '/silent', '/install' -PassThru
if (-not $runtime.WaitForExit(180000)) { throw 'WebView2 preparation exceeded its deadline' }
if ($runtime.ExitCode -ne 0) { throw "WebView2 preparation failed: $($runtime.ExitCode)" }
