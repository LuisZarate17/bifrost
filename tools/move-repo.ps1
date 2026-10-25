<#
Moves the repo folder from ...\repos\odin to ...\repos\bifrost, along with everything
that holds its path. Step 2 of Stage I's I0. A dry run unless -Apply.

Run it from a plain PowerShell window with Claude Code, VS Code, Vidar, Mimir and
Ollama closed. A folder a program has open cannot be renamed, and at the last
rename (2026-09-25) it was Ollama that held it. A shell whose current folder is
inside the repo holds it too, so cd somewhere else first:

  cd $HOME
  powershell -ExecutionPolicy Bypass -File C:\Users\you\source\repos\odin\tools\move-repo.ps1
  powershell -ExecutionPolicy Bypass -File C:\Users\you\source\repos\odin\tools\move-repo.ps1 -Apply

In order, stopping at the first failure:
  1. Checks. Nothing that holds the folder is running. The new folder and the new
     Claude Code memory folder do not exist yet. The data folder has already moved to
     %APPDATA%\bifrost. git has no worktree besides the repo itself.
  2. Renames the repo folder. If this fails, nothing else has changed.
  3. Renames Claude Code's memory folder for this repo, so the memories follow it.
     It refuses if a folder for the new path already exists.
  4. Rewrites the repo path inside %APPDATA%\bifrost\config.json: the chosen folder,
     the recent folders, and the per-folder checks and notes switches. It copies the
     file to config.json.before-move first, and touches nothing but the path.
  5. In the new folder:
     - npm install, because the workspace links hold absolute paths;
     - cargo clean, because Tauri's build caches hold them too;
     - cargo build, for the CLI and the probes that clean removed;
     - both app builds, and the shortcuts.

What it does not do:
  - Undo records made before the move point into the old path, and will not apply
    after it.
  - VS Code's per-folder state starts fresh.
  - Claude Code asks once to trust the new folder.
#>
param([switch]$Apply)

$ErrorActionPreference = 'Stop'

$old = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$parent = Split-Path $old -Parent
$new = Join-Path $parent 'bifrost'
$verb = if ($Apply) { '' } else { '(dry run) would ' }

function Step($text) { Write-Host "`n== $text" }
function Stop-With($text) { Write-Host "STOP: $text" -ForegroundColor Red; exit 1 }

# --- 1. Checks -------------------------------------------------------------
Step 'Checks'

if ((Split-Path $old -Leaf) -ne 'odin') {
    Stop-With "expected the repo at ...\odin, found $old. Has it already moved?"
}

# Every problem is listed before anything stops, so one dry run shows them all.
$problems = @()
if (Test-Path $new) { $problems += "$new already exists" }

$holders = Get-Process -ErrorAction SilentlyContinue |
    Where-Object { $_.ProcessName -in @('Code', 'claude', 'vidar', 'mimir', 'hugin', 'heimdall', 'ollama', 'ollama app') }
if ($holders) {
    $names = ($holders | Select-Object -ExpandProperty ProcessName -Unique) -join ', '
    $problems += "close these first: $names"
}

$projects = Join-Path $HOME '.claude\projects'
$oldKey = $old -replace '[^A-Za-z0-9]', '-'
$memOld = Get-ChildItem $projects -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -ieq $oldKey } | Select-Object -First 1
if ($memOld) {
    $memNewName = $memOld.Name -replace '-odin$', '-bifrost'
    $memNew = Join-Path $projects $memNewName
    if (Test-Path $memNew) { $problems += "Claude Code already has a folder for the new path: $memNew" }
    Write-Host "memory:   $($memOld.FullName) -> $memNewName"
} else {
    Write-Host "memory:   no Claude Code folder for $old; nothing to move"
}

$data = Join-Path $env:APPDATA 'bifrost'
$legacy = Join-Path $env:APPDATA 'odin'
if (-not (Test-Path $data)) {
    if ((Test-Path $legacy) -and -not (Test-Path (Join-Path $legacy 'MOVED.txt'))) {
        $problems += "the data is still in $legacy. Start the new Mimir once so it moves to $data"
    } else {
        Write-Host "config:   no $data yet; nothing to rewrite"
    }
}
$config = Join-Path $data 'config.json'

Push-Location $old
try {
    $worktrees = @(git worktree list --porcelain | Where-Object { $_ -like 'worktree *' })
} finally { Pop-Location }
if ($worktrees.Count -gt 1) {
    $problems += "git has other worktrees, which would lose their link to the repo: run 'git worktree list', then 'git worktree prune' or remove them"
}

# The path as it appears inside JSON text, every backslash doubled. Matched only
# when followed by a backslash or the closing quote, so ...\odin-something stays.
$oldJson = [regex]::Escape($old.Replace('\', '\\'))
$newJson = $new.Replace('\', '\\')
$pattern = "(?i)$oldJson(?=\\\\|`")"
if (Test-Path $config) {
    $text = [System.IO.File]::ReadAllText($config)
    $hits = [regex]::Matches($text, $pattern).Count
    Write-Host "config:   $hits mention(s) of $old in $config"
}
Write-Host "repo:     $old -> $new"
if ($problems) {
    $problems | ForEach-Object { Write-Host "problem:  $_" -ForegroundColor Yellow }
    if ($Apply) { Stop-With 'nothing was changed.' }
} else {
    Write-Host 'checks passed'
}

# --- 2. The repo folder ----------------------------------------------------
Step "${verb}rename the repo folder"
if ($Apply) {
    # Both kinds of current folder: PowerShell's own, and the process's, which
    # would otherwise still hold the repo if this was started from inside it.
    Set-Location $parent
    [System.IO.Directory]::SetCurrentDirectory($parent)
    try { Rename-Item -LiteralPath $old -NewName 'bifrost' }
    catch { Stop-With "the rename failed, so nothing has changed. Something still holds the folder: $($_.Exception.Message)" }
    Write-Host "renamed to $new"
}

# --- 3. Claude Code's memory -----------------------------------------------
if ($memOld) {
    Step "${verb}rename Claude Code's memory folder"
    if ($Apply) {
        Rename-Item -LiteralPath $memOld.FullName -NewName $memNewName
        Write-Host "renamed to $memNew"
    }
}

# --- 4. The config ---------------------------------------------------------
if (Test-Path $config) {
    Step "${verb}rewrite the repo path in config.json"
    if ($Apply) {
        Copy-Item -LiteralPath $config -Destination "$config.before-move" -Force
        $text = [System.IO.File]::ReadAllText($config)
        $out = [regex]::Replace($text, $pattern, $newJson.Replace('$', '$$'))
        [System.IO.File]::WriteAllText($config, $out, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "rewritten; the old copy is $config.before-move"
    }
}

# --- 5. Rebuild in the new place -------------------------------------------
$commands = @(
    @('npm', 'install', '--prefer-offline', '--no-audit', '--no-fund'),
    @('cargo', 'clean'),
    @('cargo', 'build'),
    @('npm', 'run', 'app:vidar'),
    @('npm', 'run', 'app:mimir'),
    @('node', 'tools/shortcuts.mjs')
)
foreach ($c in $commands) {
    $line = $c -join ' '
    Step "${verb}run: $line"
    if ($Apply) {
        Push-Location $new
        try {
            & $c[0] $c[1..($c.Count - 1)]
            if ($LASTEXITCODE -ne 0) { Stop-With "'$line' exited with $LASTEXITCODE. The folder has moved; fix this and run the rest by hand." }
        } finally { Pop-Location }
    }
}

if ($Apply) {
    Write-Host "`nDone. Open $new in VS Code, and start Claude Code there."
} else {
    Write-Host "`nDry run only. Close everything listed above and run again with -Apply."
}
