# Build dist\CalLink-<version>-windows-x64.zip (callink.exe, callink-cli.exe, docs).
# Run on Windows with Rust (MSVC toolchain) installed.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Version = (Select-String -Path "$Root\Cargo.toml" -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
cargo build --release --manifest-path "$Root\Cargo.toml" --bins
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$Stage = "$Root\target\windows\CalLink"
Remove-Item -Recurse -Force "$Root\target\windows" -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Stage | Out-Null
Copy-Item "$Root\target\release\callink.exe", "$Root\target\release\callink-cli.exe" $Stage
Copy-Item "$Root\README.md", "$Root\LICENSE" $Stage
Copy-Item "$Root\packaging\windows\USB-DRIVER.txt" $Stage

New-Item -ItemType Directory -Force "$Root\dist" | Out-Null
$Zip = "$Root\dist\CalLink-$Version-windows-x64.zip"
Remove-Item $Zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$Stage" -DestinationPath $Zip
Write-Output "built $Zip"
