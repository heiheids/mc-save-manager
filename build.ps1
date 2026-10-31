# 本地构建脚本（PowerShell）：本机没有 MSVC/MinGW 的 gcc，使用 rust-lld + 自包含 CRT。
# 用法：pwsh -File build.ps1 -Args test
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Args
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$tc = Join-Path $root ".toolchain"

$env:RUSTUP_HOME = Join-Path $tc "rustup"
$env:CARGO_HOME = Join-Path $tc "cargo"
$env:PATH = (Join-Path $tc "cargo\bin") + ";" + $env:PATH

# 沙箱会把系统 %TEMP% 隔离掉，链接器需要可写的临时目录
$tmp = Join-Path $root ".build\tmp"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$env:TMP = $tmp
$env:TEMP = $tmp

# 目标平台自带的 rust-lld（无外部 gcc 依赖）
$lld = Join-Path $tc "rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\rust-lld.exe"
$env:RUSTFLAGS = "-C linker=$lld -C link-self-contained=yes"

Set-Location $root
if ($Args.Count -eq 0) {
    cargo build --release
} else {
    cargo @Args
}
