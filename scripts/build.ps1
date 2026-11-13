param()
$ErrorActionPreference = "Stop"

$root = Split-Path $PSScriptRoot -Parent
$env:CARGO_HTTP_MULTIPLEXING = "false"

Write-Host "==> build Rust plugin (release) =="
cargo build --manifest-path "$root\Cargo.toml" -p npp-acp --lib --release

Write-Host "==> build UI (Vite+) =="
pnpm -C "$root\crates\npp-acp-ui" exec vpr build

Write-Host "==> assemble dist =="
$dist = "$root\dist\npp-acp"
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Force "$dist\ui" | Out-Null
Copy-Item "$root\target\release\npp_acp.dll" "$dist\npp-acp.dll" -Force
Copy-Item "$root\crates\npp-acp-ui\dist\*" "$dist\ui\" -Recurse -Force

$zip = "$root\dist\npp-acp.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $dist -DestinationPath $zip

Write-Host "OK: $dist"
Write-Host "OK: $zip"
