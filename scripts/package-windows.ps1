# Build the Windows deliverables for Periscrawl (all unsigned).
#
# Produces:
#   dist\Periscrawl-windows-x86_64.zip                 — portable bundle
#   dist\Periscrawl-<version>-x86_64.msi               — MSI installer (WiX v3)
#   dist\Periscrawl-<version>-x86_64-setup.exe         — setup launcher (NSIS)
#
# Installer steps are skipped gracefully when the tools are missing:
#   WIX_BIN   — directory containing candle.exe / light.exe (WiX v3)
#   NSIS_BIN  — path to makensis.exe (or the standard install location)
# Both are provisioned automatically by .github/workflows/release.yml.
#
# Run from the repository root in PowerShell 7+:
#   pwsh -File scripts/package-windows.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$Name = if ($env:PERISCRAWL_NAME) { $env:PERISCRAWL_NAME } else { "Periscrawl" }
$Version = if ($env:PERISCRAWL_VERSION) { $env:PERISCRAWL_VERSION } else { "0.2.0" }
$TargetDir = if ($env:PERISCRAWL_TARGET_DIR) { $env:PERISCRAWL_TARGET_DIR } else { "target/release" }
$Dist = "dist"
$Stage = Join-Path $Dist "$Name-windows"

Write-Host "==> Building release binaries"
cargo build --release -p periscrawl-gui -p periscrawl-cli

Write-Host "==> Staging"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item (Join-Path $TargetDir "periscrawl-gui.exe") $Stage
Copy-Item (Join-Path $TargetDir "periscrawl.exe") $Stage

@"
Periscrawl $Version (windows-x86_64)

Run periscrawl-gui.exe for the desktop app.
periscrawl.exe is the CLI (try: periscrawl.exe doctor).

Windows SmartScreen may warn on first run (unsigned build) — choose
"More info" then "Run anyway".
"@ | Set-Content (Join-Path $Stage "README.txt")

Write-Host "==> Zipping"
$Zip = Join-Path $Dist "$Name-windows-x86_64.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path $Stage -DestinationPath $Zip

# --- MSI (WiX v3) ---------------------------------------------------------
$WixBin = $env:WIX_BIN
if (-not $WixBin -and (Get-Command candle.exe -ErrorAction SilentlyContinue)) {
    $WixBin = Split-Path (Get-Command candle.exe).Source
}
if ($WixBin -and (Test-Path (Join-Path $WixBin "candle.exe"))) {
    Write-Host "==> Building MSI (WiX)"
    $WixObjDir = Join-Path $Dist "wix"
    New-Item -ItemType Directory -Force -Path $WixObjDir | Out-Null
    $DistAbs = (Resolve-Path $Dist).Path
    $Msi = Join-Path $Dist "$Name-$Version-x86_64.msi"
    if (Test-Path $Msi) { Remove-Item $Msi }
    & (Join-Path $WixBin "candle.exe") -nologo `
        "-dAppVersion=$Version" "-dDistDir=$DistAbs" `
        -out (Join-Path $WixObjDir "periscrawl.wixobj") "packaging/windows/periscrawl.wxs"
    if ($LASTEXITCODE -ne 0) { throw "candle.exe failed with exit code $LASTEXITCODE" }
    & (Join-Path $WixBin "light.exe") -nologo `
        -out $Msi (Join-Path $WixObjDir "periscrawl.wixobj")
    if ($LASTEXITCODE -ne 0) { throw "light.exe failed with exit code $LASTEXITCODE" }
    Write-Host "    $Msi"
} else {
    Write-Host "==> WiX not found (set WIX_BIN to the WiX v3 bin dir) — skipping MSI"
}

# --- Setup EXE (NSIS) -----------------------------------------------------
$Makensis = $env:NSIS_BIN
if (-not $Makensis) {
    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} "NSIS\makensis.exe"),
        (Join-Path $env:ProgramFiles "NSIS\makensis.exe")
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path $candidate)) { $Makensis = $candidate; break }
    }
}
if (-not $Makensis -and (Get-Command makensis.exe -ErrorAction SilentlyContinue)) {
    $Makensis = (Get-Command makensis.exe).Source
}
if ($Makensis -and (Test-Path $Makensis)) {
    Write-Host "==> Building setup EXE (NSIS)"
    & $Makensis "/DVERSION=$Version" "packaging/windows/periscrawl.nsi"
    if ($LASTEXITCODE -ne 0) { throw "makensis failed with exit code $LASTEXITCODE" }
} else {
    Write-Host "==> NSIS not found (set NSIS_BIN to makensis.exe) — skipping setup EXE"
}

Write-Host ""
Write-Host "Done — artifacts in ${Dist}:"
Get-ChildItem $Dist -File | Where-Object { $_.Name -match "\.(zip|msi|exe)$" } |
    ForEach-Object { Write-Host "  $($_.Name)" }
