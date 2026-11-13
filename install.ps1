# TL Supporter installer.
# Creates the setup folder, adds the program to the Windows Defender exclusions,
# downloads the latest release and installs it so that it starts with Windows.
#
# Run it from PowerShell:   powershell -ExecutionPolicy Bypass -File .\install.ps1
# Administrator rights are required. Without them nothing is changed.

$repo      = "SemkoArtem/TL_Supporter"
$setup     = "C:\TLSupporterSetup"
$target    = Join-Path $setup "TLSupporter.exe"
$installed = "C:\Program Files\TLSupporter\TLSupporter.exe"

$identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    # Nothing below runs without administrator rights. Windows asks for them, and on an account
    # without them it asks for the password of an administrator.
    Write-Host "Administrator rights are needed. Windows will ask for them now."
    try {
        Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`"" -ErrorAction Stop
    } catch {
        Write-Host "Nothing was installed: administrator rights were not given." -ForegroundColor Red
        Read-Host "Press Enter to close"
    }
    exit 1
}

$ErrorActionPreference = "Stop"
try {
    # Defender will not scan this folder, so only administrators may write to it.
    Remove-Item -Recurse -Force $setup -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $setup | Out-Null
    icacls $setup /inheritance:r /grant "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-18:(OI)(CI)F" | Out-Null
    Write-Host "Folder ready: $setup"

    Add-MpPreference -ExclusionPath $setup
    Add-MpPreference -ExclusionPath "C:\Program Files\TLSupporter"
    Write-Host "Defender exclusions added."

    # An earlier version has to go first. The team and settings stay.
    if ((Get-Service TLSupporter -ErrorAction SilentlyContinue) -and (Test-Path $installed)) {
        Write-Host "Removing the previous version..."
        Start-Process $installed -ArgumentList "--uninstall" -Wait
    }
    Get-Process TLSupporter -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 1

    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $url = "https://github.com/$repo/releases/latest/download/TLSupporter.exe"
    Write-Host "Downloading the latest version..."
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $url -OutFile $target -UseBasicParsing -Headers @{ "User-Agent" = "TLSupporter-installer" }
    if ((Get-Item $target).Length -lt 1MB) { throw "the downloaded file is too small, check the internet connection" }
    # The signature lets this PC pass the version on to PCs that were off during an update.
    try {
        Invoke-WebRequest -Uri "$url.sig" -OutFile "$target.sig" -UseBasicParsing -Headers @{ "User-Agent" = "TLSupporter-installer" }
    } catch {
        Write-Host "The signature was not downloaded, the program still works." -ForegroundColor Yellow
    }
    Unblock-File $target

    # Installs the Windows service. It starts the program now and at every Windows start.
    Write-Host "Installing and adding to autostart..."
    $run = Start-Process $target -ArgumentList "--install" -Wait -PassThru
    if ($run.ExitCode -ne 0) { throw "the installer stopped with code $($run.ExitCode)" }
    Start-Sleep -Seconds 3
    $service = Get-Service TLSupporter
    Write-Host "Service: $($service.Status), start type: $($service.StartType)"
    # The installed copy in Program Files is what runs, the setup folder is no longer needed.
    Remove-MpPreference -ExclusionPath $setup -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $setup -ErrorAction SilentlyContinue
    Write-Host "Done. The program now starts together with Windows."
} catch {
    Write-Host "Failed: $($_.Exception.Message)" -ForegroundColor Red
}
Read-Host "Press Enter to close"
