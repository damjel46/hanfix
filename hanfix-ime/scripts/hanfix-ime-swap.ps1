# hanfix-ime: swap the installed UNIM TSF DLLs for the hanfix-ime build (or restore UNIM).
#
#   powershell -ExecutionPolicy Bypass -File hanfix-ime-swap.ps1            # install hanfix-ime DLLs
#   powershell -ExecutionPolicy Bypass -File hanfix-ime-swap.ps1 -Restore   # put UNIM's original DLLs back
#
# Needs administrator rights (Program Files). A DLL that apps have loaded cannot be overwritten,
# but it can be renamed: the old file is moved aside and the new one copied in; apps opened after
# this use the new DLL (restart running apps, or sign out/in, to switch everything over).
param([switch]$Restore)
$ErrorActionPreference = 'Stop'

$installDir = 'C:\Program Files\UNIM'
$repo = Split-Path -Parent $PSScriptRoot
$pairs = @(
    @{ Src = Join-Path $repo 'target\release\unim_tsf.dll'; Dst = Join-Path $installDir 'unim_tsf.dll' },
    @{ Src = Join-Path $repo 'target\i686-pc-windows-msvc\release\unim_tsf.dll'; Dst = Join-Path $installDir 'unim_tsf32.dll' }
)
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'

foreach ($p in $pairs) {
    $dst = $p.Dst
    $orig = "$dst.unim-orig"
    if (-not (Test-Path $dst)) { throw "UNIM is not installed here: $dst" }
    if (-not (Test-Path $orig)) {
        Copy-Item $dst $orig
        Write-Host "backed up original: $orig"
    }
    $from = if ($Restore) { $orig } else { $p.Src }
    if (-not (Test-Path $from)) { throw "missing: $from (build it first)" }
    # Loaded DLLs can be renamed but not overwritten.
    Rename-Item $dst "$([IO.Path]::GetFileName($dst)).old-$stamp"
    Copy-Item $from $dst
    Write-Host "$(if ($Restore) {'restored'} else {'installed'}): $dst"
}

# Old copies that nothing has loaded any more can go.
Get-ChildItem $installDir -Filter '*.old-*' | ForEach-Object {
    try { Remove-Item $_.FullName -ErrorAction Stop } catch { }
}
Write-Host 'done. Restart apps (or sign out and in) so they load the new DLL.'
