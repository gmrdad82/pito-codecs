param([ValidateSet("install", "uninstall", "status")][string]$Action = "install")
$ErrorActionPreference = "Stop"
$pcod = $null
foreach ($c in @((Join-Path $PSScriptRoot "..\pcod.exe"), (Join-Path $PSScriptRoot "..\bin\pcod.exe"))) {
    if (Test-Path $c) { $pcod = $c; break }
}
if (-not $pcod) {
    $found = Get-Command pcod -ErrorAction SilentlyContinue
    if ($found) { $pcod = $found.Source }
}
if (-not $pcod) { Write-Error "install-skill: no pcod beside this folder or on PATH"; exit 1 }
& $pcod skill $Action
exit $LASTEXITCODE
