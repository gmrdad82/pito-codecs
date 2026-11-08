param([ValidateSet("install", "uninstall")][string]$Action = "install")
$ErrorActionPreference = "Stop"
$name = "pcod"
$marker = ".pito-skill"
$source = Join-Path $PSScriptRoot $name
$root = $env:USERPROFILE
$homes = @("$root\.agents\skills", "$root\.claude\skills")
if (Test-Path "$root\.codex") { $homes += "$root\.codex\skills" }
if (Test-Path "$root\.pi") { $homes += "$root\.pi\agent\skills" }
$every = @("$root\.agents\skills", "$root\.claude\skills", "$root\.codex\skills", "$root\.pi\agent\skills")
if ($Action -eq "install") {
    if (-not (Test-Path "$source\SKILL.md")) { Write-Error "install-skill: no $name skill beside this script" }
    foreach ($home in $homes) {
        $dest = Join-Path $home $name
        if ((Test-Path $dest) -and -not (Test-Path (Join-Path $dest $marker))) {
            Write-Output "install-skill: $dest is not ours, left alone"
            continue
        }
        New-Item -ItemType Directory -Force $home | Out-Null
        if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
        Copy-Item -Recurse $source $dest
        Set-Content -Path (Join-Path $dest $marker) -Value "pito-codecs" -NoNewline
    }
} else {
    foreach ($home in $every) {
        $dest = Join-Path $home $name
        if ((Test-Path $dest) -and (Test-Path (Join-Path $dest $marker))) { Remove-Item -Recurse -Force $dest }
    }
}
