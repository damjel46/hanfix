# hanfix-ime one-step installer (Windows 10/11).
#
#   install.ps1              install: UNIM (bundled MSI, if missing) + hanfix-ime DLLs + keyboard entry
#   install.ps1 -Restore     back to plain UNIM (keeps UNIM installed)
#   install.ps1 -Uninstall   remove hanfix-ime and UNIM completely
#
# Double-click the .bat files instead of running this directly (extract the zip first). Asks for
# administrator rights once (Program Files and the input method registration are system-wide).
param([switch]$Restore, [switch]$Uninstall)
$ErrorActionPreference = 'Stop'

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $flags = @()
    if ($Restore) { $flags += '-Restore' }
    if ($Uninstall) { $flags += '-Uninstall' }
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"") + $flags
    try {
        Start-Process powershell -Verb RunAs -ArgumentList $argList
    } catch {
        Write-Host 'Administrator approval was declined. Nothing was changed.' -ForegroundColor Red
        Read-Host 'Press Enter to close'
    }
    exit
}

$installDir = 'C:\Program Files\UNIM'
$unimVersion = '0.4.4'
$msi = Join-Path $PSScriptRoot 'unim-0.4.4-x64.msi'
$tip = '0412:{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}{B2C3D4E5-F6A7-8901-BCDE-F12345678901}'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$expected = @{
    'unim_tsf.dll'       = '62821EBF98CFCEB8DD8B7E63364F39DBC23E45ABDDA90137FFD22AFDC77A0D29'
    'unim_tsf32.dll'     = 'A92C3399B02DFB0683C6701EA464C6D406ED4AA407F2D8AF6FDEB747952CF15F'
    'unim-0.4.4-x64.msi' = '23A462247E726F44C7D108B107D4A5544E8E47DA77F83A870F8E559EBED52699'
}
# The DLLs inside the official UNIM 0.4.4 MSI.
$unimHashes = @{
    'unim_tsf.dll'   = '1687B1D9A801009651B9681B4D8D5FD00CA5489F8802F4900A070AD7EE22E961'
    'unim_tsf32.dll' = '9BAD03B6FAC8E7BA29DB5549BC2F1B50410B7F913BE30E0192F68443CEF7B14C'
}

function Assert-Bundle {
    foreach ($name in $expected.Keys) {
        $path = Join-Path $PSScriptRoot $name
        if (-not (Test-Path $path)) { throw "missing $name - extract the whole zip first" }
        if ((Get-FileHash $path -Algorithm SHA256).Hash -ne $expected[$name]) { throw "$name is damaged (checksum mismatch) - download the zip again" }
    }
}

function Get-UnimProduct {
    $keys = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
    Get-ItemProperty $keys -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -like 'UNIM*' } | Select-Object -First 1
}

function Swap-Dlls([bool]$toOriginal) {
    if ($toOriginal) {
        # Check both backups before touching either DLL, so a restore never ends half done.
        foreach ($name in 'unim_tsf.dll', 'unim_tsf32.dll') {
            $dst = Join-Path $installDir $name
            $orig = "$dst.unim-orig"
            if ((Test-Path $dst) -and (Get-FileHash $dst -Algorithm SHA256).Hash -eq $unimHashes[$name]) { continue }
            if (-not (Test-Path $orig) -or (Get-FileHash $orig -Algorithm SHA256).Hash -ne $unimHashes[$name]) {
                throw "no original UNIM backup for $name. Run the uninstaller, then reinstall UNIM (or repair it in Settings > Apps)."
            }
        }
    }
    foreach ($name in 'unim_tsf.dll', 'unim_tsf32.dll') {
        $dst = Join-Path $installDir $name
        $src = Join-Path $PSScriptRoot $name
        if (-not (Test-Path $dst)) { throw "UNIM files not found: $dst" }
        $orig = "$dst.unim-orig"
        # Only a genuine UNIM 0.4.4 DLL becomes the restore point; an older hanfix build must never
        # overwrite it.
        $installed = (Get-FileHash $dst -Algorithm SHA256).Hash
        if ($installed -eq $unimHashes[$name]) {
            if ($toOriginal) { continue }
            Copy-Item $dst $orig -Force
        }
        $from = if ($toOriginal) { $orig } else { $src }
        if (-not (Test-Path $from)) { throw "missing: $from" }
        # A DLL that apps have loaded cannot be overwritten, but it can be renamed.
        $old = "$dst.old-$stamp"
        Rename-Item $dst ([IO.Path]::GetFileName($old))
        try {
            Copy-Item $from $dst
        } catch {
            # Never leave the input method without its DLL.
            if (Test-Path $dst) { Remove-Item $dst -Force }
            Rename-Item $old $name
            throw
        }
    }
    Get-ChildItem $installDir -Filter '*.old-*' | ForEach-Object { try { Remove-Item $_.FullName -ErrorAction Stop } catch { } }
}

function Set-Keyboard([bool]$present) {
    $list = Get-WinUserLanguageList
    $ko = $list | Where-Object LanguageTag -eq 'ko'
    if ($present) {
        if (-not $ko) { $list.Add('ko'); $ko = $list | Where-Object LanguageTag -eq 'ko' }
        if ($ko.InputMethodTips -notcontains $tip) { $ko.InputMethodTips.Add($tip); Set-WinUserLanguageList $list -Force }
    } elseif ($ko -and $ko.InputMethodTips -contains $tip) {
        [void]$ko.InputMethodTips.Remove($tip)
        Set-WinUserLanguageList $list -Force
    }
}

try {
    if ($Uninstall) {
        Write-Host 'Removing hanfix-ime and UNIM...'
        Set-Keyboard $false
        $product = Get-UnimProduct
        if ($product) {
            $p = Start-Process msiexec.exe -ArgumentList @('/x', $product.PSChildName, '/qn', '/norestart') -Wait -PassThru
            if ($p.ExitCode -notin 0, 3010) { throw "UNIM uninstall failed (msiexec $($p.ExitCode)). Remove UNIM in Settings > Apps." }
        }
        if (Test-Path $installDir) {
            Get-ChildItem $installDir -Include '*.unim-orig', '*.old-*' -Recurse | ForEach-Object { try { Remove-Item $_.FullName -ErrorAction Stop } catch { } }
            if (-not (Get-ChildItem $installDir -Force)) { Remove-Item $installDir }
        }
        Write-Host 'Removed. Sign out and in to finish.'
    } elseif ($Restore) {
        Swap-Dlls $true
        Write-Host 'Restored the original UNIM. Restart apps to use it.'
    } else {
        Assert-Bundle
        $product = Get-UnimProduct
        if ($product -and $product.DisplayVersion -ne $unimVersion) {
            throw "UNIM $($product.DisplayVersion) is installed, but this hanfix-ime build is for UNIM $unimVersion. Nothing was changed."
        }
        if (-not (Test-Path (Join-Path $installDir 'unim_tsf.dll'))) {
            Write-Host "Installing UNIM $unimVersion (bundled)..."
            $p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$msi`"", '/qn', '/norestart') -Wait -PassThru
            if ($p.ExitCode -notin 0, 3010) { throw "UNIM install failed (msiexec $($p.ExitCode))" }
        }
        Write-Host 'Installing hanfix-ime...'
        Swap-Dlls $false
        Set-Keyboard $true
        # Learn only from an explicit Ctrl+Z (UNIM's automatic rollback learning also fires on retyping).
        $cfg = Join-Path $env:APPDATA 'unim\config.yaml'
        if (Test-Path $cfg) {
            Copy-Item $cfg "$cfg.bak-$stamp"
            $text = [IO.File]::ReadAllText($cfg)
            $text = $text -replace '(?m)^(\s*)rollback_detection:\s*\w+', '${1}rollback_detection: false'
            [IO.File]::WriteAllText($cfg, $text, [Text.UTF8Encoding]::new($false))
        }
        Write-Host ''
        Write-Host 'Done. Sign out and in (or restart your apps), then pick UNIM with Win+Space.'
    }
} catch {
    Write-Host "FAILED: $_" -ForegroundColor Red
}
Read-Host 'Press Enter to close'
