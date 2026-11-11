# Assemble the raw BGRA dumps from `cargo run --example preview` into one zoomed
# contact sheet, so small icon sizes can be eyeballed for mushiness.
#
#   cargo run --release --example preview
#   powershell -ExecutionPolicy Bypass -File examples\preview.ps1
#   start target\icon-preview\sheet.png
#
# Keep this file ASCII-only: Windows PowerShell 5.1 parses .ps1 as the ANSI code
# page, so non-ASCII text without a BOM breaks the parser.

Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$dir  = Join-Path $root 'target\icon-preview'
$files = Get-ChildItem $dir -Filter *.bgra -ErrorAction SilentlyContinue |
         Sort-Object { [int]$_.BaseName }
if (-not $files) {
    throw "no .bgra in $dir - run 'cargo run --release --example preview' first"
}

$cell = 128
$pad  = 8
$w = ($cell + $pad) * $files.Count + $pad
$h = $cell + $pad * 2

$sheet = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($sheet)
$g.Clear([System.Drawing.Color]::FromArgb(210, 214, 220))
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$g.PixelOffsetMode   = [System.Drawing.Drawing2D.PixelOffsetMode]::Half

$x0 = $pad
foreach ($f in $files) {
    $n = [int]$f.BaseName
    $bytes = [IO.File]::ReadAllBytes($f.FullName)

    # render() emits top-down BGRA, so pixel (x, y) is at ((y * n) + x) * 4
    $raw = New-Object System.Drawing.Bitmap $n, $n
    for ($y = 0; $y -lt $n; $y++) {
        for ($x = 0; $x -lt $n; $x++) {
            $i = ($y * $n + $x) * 4
            $raw.SetPixel($x, $y, [System.Drawing.Color]::FromArgb(
                $bytes[$i + 3], $bytes[$i + 2], $bytes[$i + 1], $bytes[$i]))
        }
    }

    $g.DrawImage($raw, $x0, $pad, $cell, $cell)
    $raw.Dispose()
    $x0 += $cell + $pad
}

$g.Dispose()
$out = Join-Path $dir 'sheet.png'
$sheet.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$sheet.Dispose()
Write-Host "wrote $out"
