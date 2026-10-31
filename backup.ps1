# Source backup for the save-editor project.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File backup.ps1 -Desc "what you are about to do"
#
# Creates:
#   <root>\<backupDirName>\<zipName>.zip     -- zipped source tree
#   <root>\<backupDirName>\<logName>         -- append-only log of backups
#
# NOTE: This file is intentionally ASCII-only. Windows PowerShell 5.1 reads
#       .ps1 files using the system ANSI code page, so non-ASCII literals in
#       the script body would be corrupted. All Chinese text is built from
#       code points at runtime instead.

param(
    [Parameter(Mandatory = $true)][string]$Desc,
    [string]$Root = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Root)) {
    # This script sits at the project root, so PSScriptRoot *is* the root.
    # If it is ever moved into a subdirectory, walk up until Cargo.toml is found.
    $candidate = $PSScriptRoot
    if ([string]::IsNullOrWhiteSpace($candidate)) { $candidate = (Get-Location).Path }
    $probe = $candidate
    for ($n = 0; $n -lt 4; $n++) {
        if (Test-Path (Join-Path $probe "Cargo.toml")) { break }
        $parent = Split-Path -Parent $probe
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $probe) { break }
        $probe = $parent
    }
    $Root = $probe
}
$Root = (Resolve-Path $Root).Path

# --- Chinese strings built from code points (ASCII-safe source) ---
function U([int[]]$cp) { -join ($cp | ForEach-Object { [char]$_ }) }

$dirName = (U @(0x5B58, 0x6863, 0x7BA1, 0x7406, 0x5668, 0x5DE5, 0x7A0B, 0x5907, 0x4EFD))  # save-editor project backup
$logName = (U @(0x5907, 0x4EFD, 0x8BB0, 0x5F55)) + ".md"                                  # backup log
$titleText = (U @(0x5B58, 0x6863, 0x7BA1, 0x7406, 0x5668, 0x5DE5, 0x7A0B, 0x5907, 0x4EFD, 0x8BB0, 0x5F55))
$hTime = (U @(0x65F6, 0x95F4))
$hAction = (U @(0x64CD, 0x4F5C))
$hFile = (U @(0x5907, 0x4EFD, 0x6587, 0x4EF6))
$hCount = (U @(0x6587, 0x4EF6, 0x6570))
$hSize = (U @(0x5927, 0x5C0F))

$backupDir = Join-Path $Root $dirName
if (-not (Test-Path $backupDir)) {
    New-Item -ItemType Directory -Path $backupDir | Out-Null
}

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$now = Get-Date -Format "yyyy-MM-dd HH:mm:ss"

# sanitize description for filesystem use
$safe = $Desc -replace '[\\/:*?"<>|\r\n]', '_'
if ($safe.Length -gt 40) { $safe = $safe.Substring(0, 40) }
$zipName = "backup_${stamp}_${safe}.zip"
$zipPath = Join-Path $backupDir $zipName

# source items to include (build outputs are never included)
$include = @(
    "crates",
    "docs",
    ".cargo",
    "Cargo.toml",
    "Cargo.lock",
    "build.cmd",
    "backup.ps1",
    "backup.cmd",
    "README.md"
)

$items = @()
foreach ($i in $include) {
    $p = Join-Path $Root $i
    if (Test-Path $p) { $items += $p }
}
if ($items.Count -eq 0) {
    Write-Error "No source files found to back up."
    exit 1
}

$files = @()
foreach ($it in $items) {
    if (Test-Path $it -PathType Container) {
        $files += Get-ChildItem $it -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -notmatch '\\target\\|\\\.build\\|\\\.toolchain\\' }
    }
    else {
        $files += Get-Item $it
    }
}
$fileCount = $files.Count
if ($fileCount -eq 0) {
    Write-Error "No source files found to back up."
    exit 1
}

if (Test-Path $zipPath) { Remove-Item $zipPath -Force }

Add-Type -AssemblyName System.IO.Compression | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

$zip = [System.IO.Compression.ZipFile]::Open($zipPath, 1)  # 1 = ZipArchiveMode.Create
try {
    foreach ($f in $files) {
        $rel = $f.FullName.Substring($Root.Length).TrimStart('\', '/')
        $entryName = $rel -replace '\\', '/'
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $zip, $f.FullName, $entryName,
            [System.IO.Compression.CompressionLevel]::Optimal)
    }
}
finally {
    $zip.Dispose()
}

$zipSize = (Get-Item $zipPath).Length

# --- append to the log ---
$logPath = Join-Path $backupDir $logName
$utf8bom = New-Object System.Text.UTF8Encoding($true)
if (-not (Test-Path $logPath)) {
    $lines = @(
        "# $titleText",
        "",
        "| $hTime | $hAction | $hFile | $hCount | $hSize |",
        "| --- | --- | --- | --- | --- |"
    )
    [System.IO.File]::WriteAllLines($logPath, $lines, $utf8bom)
}

$zipMB = [math]::Round($zipSize / 1MB, 2)
$line = "| $now | $Desc | $zipName | $fileCount | $zipMB MB |"
$existing = [System.IO.File]::ReadAllText($logPath)
[System.IO.File]::WriteAllText($logPath, $existing + $line + "`r`n", $utf8bom)

Write-Output "OK"
Write-Output "  zip   : $zipPath"
Write-Output "  files : $fileCount"
Write-Output "  size  : $zipMB MB"
Write-Output "  log   : $logPath"
