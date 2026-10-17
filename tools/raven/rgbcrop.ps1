# Write the top-left w × h of a PNG as raw RGB bytes.
#   rgbcrop.ps1 <png> <out.rgb> <w> <h>
param([string]$png, [string]$out, [int]$cw, [int]$chh)
Add-Type -AssemblyName System.Drawing
$bmp = [Drawing.Bitmap]::FromFile($png)
$data = $bmp.LockBits((New-Object Drawing.Rectangle 0, 0, $cw, $chh), [Drawing.Imaging.ImageLockMode]::ReadOnly, [Drawing.Imaging.PixelFormat]::Format24bppRgb)
$src = New-Object byte[] ($data.Stride * $chh); [Runtime.InteropServices.Marshal]::Copy($data.Scan0, $src, 0, $src.Length); $bmp.UnlockBits($data); $bmp.Dispose()
$dst = New-Object byte[] ($cw * $chh * 3)
for ($y = 0; $y -lt $chh; $y++) { for ($x = 0; $x -lt $cw; $x++) { $o = $y * $data.Stride + $x * 3; $d = ($y * $cw + $x) * 3; $dst[$d] = $src[$o + 2]; $dst[$d + 1] = $src[$o + 1]; $dst[$d + 2] = $src[$o] } }
[IO.File]::WriteAllBytes($out, $dst); "$cw x $chh"
