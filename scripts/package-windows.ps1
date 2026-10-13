# Build a distributable Windows bundle for RustySEO.
#
# Produces dist\RustySEO-windows\ with the GUI + CLI binaries and a zip next
# to it. Run from the repository root in PowerShell 7+:
#   pwsh -File scripts/package-windows.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$Name = "RustySEO"
$Version = "0.2.0"
$Dist = "dist"
$Stage = Join-Path $Dist "$Name-windows"

Write-Host "==> Building release binaries"
cargo build --release -p rustyseo-gui -p rustyseo-cli

Write-Host "==> Staging"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item "target/release/rustyseo-gui.exe" $Stage
Copy-Item "target/release/rustyseo.exe" $Stage

@"
RustySEO $Version (windows-x86_64)

Run rustyseo-gui.exe for the desktop app.
rustyseo.exe is the CLI (try: rustyseo.exe doctor).

Windows SmartScreen may warn on first run (unsigned build) — choose
"More info" then "Run anyway".
"@ | Set-Content (Join-Path $Stage "README.txt")

Write-Host "==> Zipping"
$Zip = Join-Path $Dist "$Name-windows-x86_64.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path $Stage -DestinationPath $Zip

Write-Host ""
Write-Host "Done: $Zip"
Write-Host "Share the zip with teammates; extract and run rustyseo-gui.exe."
