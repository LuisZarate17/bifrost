# Make apps/hugin/assets/raven.six from raven.svg: draw it 60 pixels tall (three rows of Windows
# Terminal's 10 x 20 Sixel cells, 90 pixels wide) on the terminal's black with headless Edge,
# then encode it as Sixel with that black left undrawn, so the terminal's own shows through.
#   powershell -File tools/raven/make.ps1
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$assets = Join-Path $here "..\..\apps\hugin\assets" | Resolve-Path
$work = Join-Path $env:TEMP "hugin-raven"
New-Item -ItemType Directory -Force $work | Out-Null
$svg = Get-Content (Join-Path $assets "raven.svg") -Raw
$svg = $svg -replace '<svg ', '<svg height="60" width="85" style="display:block" '
Set-Content (Join-Path $work "raven.html") "<!doctype html><meta charset=`"utf-8`"><style>body{margin:0;background:#0c0c0c}</style><body>$svg</body>" -Encoding utf8
$png = Join-Path $work "raven.png"
Remove-Item $png -ErrorAction SilentlyContinue
Start-Process "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" -Wait -ArgumentList @(
  "--headless=new", "--disable-gpu", "--hide-scrollbars", "--user-data-dir=$work\edge", "--screenshot=$png", "--window-size=400,200",
  ("file:///" + (Join-Path $work "raven.html").Replace([char]92, [char]47)))
& (Join-Path $here "rgbcrop.ps1") $png (Join-Path $work "raven.rgb") 90 60 | Out-Null
node (Join-Path $here "sixel.mjs") (Join-Path $work "raven.rgb") 90 60 (Join-Path $assets "raven.six") "#0c0c0c"
