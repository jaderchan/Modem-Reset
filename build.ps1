# Build script: produces target\release\ModemReset.exe and stages it into dist\
#
#   .\build.ps1            build
#   .\build.ps1 -Run       build, then launch the tray app
#
# NOTE: keep this file ASCII-only. Windows PowerShell 5.1 reads .ps1 as the
# system ANSI code page, so non-ASCII text without a BOM breaks the parser.

param([switch]$Run)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

# Keep local build paths out of the binary. Cargo feeds registry sources to
# rustc as absolute paths, and those end up in panic location strings unless
# remapped. CARGO_ENCODED_RUSTFLAGS overrides [build] rustflags, so +crt-static
# has to be repeated here. Elements are joined with 0x1f so paths with spaces
# survive.
$cargoHome  = if ($env:CARGO_HOME)  { $env:CARGO_HOME }  else { Join-Path $env:USERPROFILE '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
$cargoHome  = $cargoHome.TrimEnd('\', '/')
$rustupHome = $rustupHome.TrimEnd('\', '/')
$sep = [char]0x1f
$env:CARGO_ENCODED_RUSTFLAGS = @(
    '-C', 'target-feature=+crt-static',
    "--remap-path-prefix=$cargoHome=/cargo",
    "--remap-path-prefix=$rustupHome=/rustup"
) -join $sep

Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    $exe  = Join-Path $root 'target\release\ModemReset.exe'
    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force -Path $dist | Out-Null

    Copy-Item $exe $dist -Force
    $ini = Join-Path $dist 'modem.ini'
    if (-not (Test-Path $ini)) {
        Copy-Item (Join-Path $root 'modem.ini.example') $ini
        Write-Host 'Created dist\modem.ini - fill in the passwords, set [reboot] confirmed = yes'
    }

    $size = [math]::Round((Get-Item (Join-Path $dist 'ModemReset.exe')).Length / 1MB, 2)
    Write-Host "Done: dist\ModemReset.exe ($size MB)"

    if ($Run) { Start-Process (Join-Path $dist 'ModemReset.exe') }
}
finally {
    Pop-Location
}
