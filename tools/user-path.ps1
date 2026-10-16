# Put a folder on the user PATH, keeping every entry as it is: unexpanded (%USERPROFILE%\... stays
# so), in its order, in the same registry type. Then tell running programs the environment changed,
# so a terminal opened from now on finds what is in the folder.
#
#   user-path.ps1 -Folder <dir>           add it if it is not there
#   user-path.ps1 -Folder <dir> -Check    only say whether it is there
#
# Prints one line of JSON: { there, changed, before, after }.
#
# Not [Environment]::Get/SetEnvironmentVariable('Path', ..., 'User'): the get expands every
# %VAR% in the value and the set writes it back as a plain string, which would quietly rewrite
# every entry. This only ever appends, so it can never leave the PATH shorter than it found it.
param([Parameter(Mandatory = $true)][string]$Folder, [switch]$Check)
$ErrorActionPreference = 'Stop'

$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
if (-not $key) { throw 'HKCU\Environment cannot be opened' }
$raw = $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$kind = if ($null -ne $raw) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }

# An entry is the folder when it names the same place: expanded, without a trailing slash, in
# any case — the way Windows itself would find a program through it.
function Same([string]$entry) {
    $a = [Environment]::ExpandEnvironmentVariables($entry.Trim()).TrimEnd('\', '/')
    $b = [Environment]::ExpandEnvironmentVariables($Folder.Trim()).TrimEnd('\', '/')
    return $a -ieq $b
}
$entries = if ($raw) { $raw -split ';' } else { @() }
$there = @($entries | Where-Object { $_.Trim() -and (Same $_) }).Count -gt 0

if ($Check -or $there) {
    [pscustomobject]@{ there = $there; changed = $false; before = $raw; after = $raw } | ConvertTo-Json -Compress
    return
}

$next = if ([string]::IsNullOrEmpty($raw)) { $Folder } elseif ($raw.EndsWith(';')) { "$raw$Folder" } else { "$raw;$Folder" }
$key.SetValue('Path', $next, $kind)
$key.Close()

# WM_SETTINGCHANGE with "Environment", as the System dialog sends it: Explorer reloads, so the
# shortcuts and terminals it starts from now on see the new PATH.
Add-Type -Namespace Bifrost -Name Env -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
'@
$result = [UIntPtr]::Zero
[void][Bifrost.Env]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)

[pscustomobject]@{ there = $true; changed = $true; before = $raw; after = $next } | ConvertTo-Json -Compress
