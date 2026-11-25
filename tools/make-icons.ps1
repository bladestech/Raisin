# Regenerates assets/ from Raisin.png (the logo). Run from the repo root:
#   powershell -ExecutionPolicy Bypass -File tools\make-icons.ps1
# Outputs:
#   assets\raisin.ico   multi-size app icon (16-256 px), embedded into the exe
#   assets\logo.bgra    premultiplied BGRA pixels for the settings window header
param(
    [string]$Source = "Raisin.png",
    [int]$LogoPx = 128
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$root = Split-Path $PSScriptRoot -Parent
$src = [System.Drawing.Bitmap]::FromFile((Join-Path $root $Source))
New-Item -ItemType Directory -Force (Join-Path $root "assets") | Out-Null

# Trim transparent padding so the R fills small icons.
$data = $src.LockBits((New-Object System.Drawing.Rectangle 0, 0, $src.Width, $src.Height),
    [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$bytes = New-Object byte[] ($data.Stride * $src.Height)
[System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
$src.UnlockBits($data)
$minX = $src.Width; $minY = $src.Height; $maxX = 0; $maxY = 0
for ($y = 0; $y -lt $src.Height; $y += 2) {
    $row = $y * $data.Stride
    for ($x = 0; $x -lt $src.Width; $x += 2) {
        if ($bytes[$row + $x * 4 + 3] -gt 16) {
            if ($x -lt $minX) { $minX = $x }; if ($x -gt $maxX) { $maxX = $x }
            if ($y -lt $minY) { $minY = $y }; if ($y -gt $maxY) { $maxY = $y }
        }
    }
}
# Square crop around the content, with a little breathing room.
$side = [Math]::Max($maxX - $minX, $maxY - $minY) * 1.04
$cx = ($minX + $maxX) / 2; $cy = ($minY + $maxY) / 2
$crop = New-Object System.Drawing.RectangleF ($cx - $side / 2), ($cy - $side / 2), $side, $side
"content box: x $minX-$maxX, y $minY-$maxY -> crop side $([int]$side)"

function Render([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($src, (New-Object System.Drawing.RectangleF 0, 0, $size, $size), $crop, [System.Drawing.GraphicsUnit]::Pixel)
    $g.Dispose()
    return $bmp
}

# ICO entries: 32-bit DIBs for small sizes (read by everything), PNG for 256.
function Dib([System.Drawing.Bitmap]$b) {
    $s = $b.Width
    $d = $b.LockBits((New-Object System.Drawing.Rectangle 0, 0, $s, $s),
        [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $px = New-Object byte[] ($s * $s * 4)
    for ($y = 0; $y -lt $s; $y++) {
        # DIB rows are bottom-up.
        [System.Runtime.InteropServices.Marshal]::Copy([IntPtr]($d.Scan0.ToInt64() + $y * $d.Stride), $px, ($s - 1 - $y) * $s * 4, $s * 4)
    }
    $b.UnlockBits($d)
    $maskRow = [Math]::Ceiling($s / 32) * 4
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    $bw.Write([uint32]40); $bw.Write([int32]$s); $bw.Write([int32]($s * 2))  # height covers XOR + AND
    $bw.Write([uint16]1); $bw.Write([uint16]32); $bw.Write([uint32]0)
    $bw.Write([uint32]($px.Length + $maskRow * $s)); $bw.Write([int32]0); $bw.Write([int32]0)
    $bw.Write([uint32]0); $bw.Write([uint32]0)
    $bw.Write($px)
    $bw.Write((New-Object byte[] ($maskRow * $s)))  # AND mask unused with alpha
    return , $ms.ToArray()
}
$sizes = 16, 20, 24, 32, 40, 48, 64, 128, 256
$pngs = foreach ($s in $sizes) {
    $b = Render $s
    if ($s -ge 256) {
        $ms = New-Object System.IO.MemoryStream
        $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $bytes = $ms.ToArray()
    } else {
        $bytes = Dib $b
    }
    $b.Dispose()
    , $bytes
}
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $dim = if ($s -ge 256) { 0 } else { $s }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$pngs[$i].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write($p) }
[System.IO.File]::WriteAllBytes((Join-Path $root "assets\raisin.ico"), $out.ToArray())
"wrote assets\raisin.ico ($($sizes -join ', ') px)"

# Raw premultiplied BGRA for Direct2D (no image decoder needed at runtime).
$b = Render $LogoPx
$d = $b.LockBits((New-Object System.Drawing.Rectangle 0, 0, $LogoPx, $LogoPx),
    [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppPArgb)
$raw = New-Object byte[] ($LogoPx * $LogoPx * 4)
for ($y = 0; $y -lt $LogoPx; $y++) {
    [System.Runtime.InteropServices.Marshal]::Copy([IntPtr]($d.Scan0.ToInt64() + $y * $d.Stride), $raw, $y * $LogoPx * 4, $LogoPx * 4)
}
$b.UnlockBits($d); $b.Dispose()
[System.IO.File]::WriteAllBytes((Join-Path $root "assets\logo.bgra"), $raw)
"wrote assets\logo.bgra ($LogoPx x $LogoPx)"
$src.Dispose()
