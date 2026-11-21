#requires -Version 7
# Builds the FigIran web build (static site) into dist\web with the portal address baked in, so the page can sign in.
#   .\build-web.ps1 -PortalUrl http://192.168.10.5:4000
# Needs: Rust + the wasm32-unknown-unknown target, trunk (%USERPROFILE%\tools\trunk\bin), and the same MSVC/xwin libraries as build-app.bat.
# Trunk downloads wasm-opt on the fly, which fails on a network without github.com access: so trunk skips it (a temporary copy of
# index.html) and the binaryen npm package (%USERPROFILE%\tools\binaryen) optimises the .wasm afterwards with the same flags.
param([Parameter(Mandatory)][string]$PortalUrl)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$figcraft = $root   # this repository
$app = Join-Path $figcraft 'apps\figiran-web'
$msvc = 'C:\Program Files\Microsoft Visual Studio\18\Enterprise\VC\Tools\MSVC\14.50.35717'
$xwin = Join-Path $env:USERPROFILE 'xwin\splat'
$env:PATH = "$msvc\bin\Hostx64\x64;$env:USERPROFILE\.cargo\bin;$env:USERPROFILE\tools\trunk\bin;$env:PATH"
$env:LIB = "$msvc\lib\x64;$xwin\crt\lib\x86_64;$xwin\sdk\lib\um\x86_64;$xwin\sdk\lib\ucrt\x86_64"
$env:INCLUDE = "$xwin\crt\include;$xwin\sdk\include\ucrt;$xwin\sdk\include\um;$xwin\sdk\include\shared"
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = "$msvc\bin\Hostx64\x64\link.exe"
$env:CARGO_TARGET_DIR = Join-Path $root 'build-cache'
$env:FIGIRAN_PORTAL_URL = $PortalUrl

$html = Get-Content (Join-Path $app 'index.html') -Raw
$params = [regex]::Match($html, 'data-wasm-opt-params="([^"]*)"').Groups[1].Value
$temp = Join-Path $app 'index.build-web.html'
try {
    Set-Content $temp ([regex]::Replace($html, 'data-wasm-opt="z" data-wasm-opt-params="[^"]*"', 'data-wasm-opt="0"')) -NoNewline
    Push-Location $app
    try { & trunk build --release=true $temp; if ($LASTEXITCODE -ne 0) { throw "trunk build failed ($LASTEXITCODE)" } } finally { Pop-Location }
} finally { Remove-Item $temp -ErrorAction SilentlyContinue }

# trunk named the page after the temporary file: the site's page is index.html.
$dist = Join-Path $figcraft 'dist\web'
if (Test-Path (Join-Path $dist 'index.build-web.html')) { Move-Item (Join-Path $dist 'index.build-web.html') (Join-Path $dist 'index.html') -Force }
$wasm = Get-ChildItem $dist -Filter '*_bg.wasm' | Select-Object -First 1
$opt = Join-Path $env:USERPROFILE 'tools\binaryen\node_modules\binaryen\bin\wasm-opt'
if ((Test-Path $opt) -and $wasm) {
    $out = "$($wasm.FullName).opt"
    & node $opt $wasm.FullName -Oz @($params -split '\s+' | Where-Object { $_ }) -o $out
    if ($LASTEXITCODE -ne 0) { throw "wasm-opt failed ($LASTEXITCODE)" }
    Move-Item $out $wasm.FullName -Force
}
Write-Host "Web build ready: $dist (portal $PortalUrl)"
Get-ChildItem $dist | ForEach-Object { '{0,12:N0}  {1}' -f $_.Length, $_.Name }
