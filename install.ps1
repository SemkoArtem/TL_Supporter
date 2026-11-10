# TL Supporter installer.
# Creates the setup folder, adds the program to the Windows Defender exclusions,
# downloads the current version and installs it so that it starts with Windows.
#
# Run it from PowerShell:   powershell -ExecutionPolicy Bypass -File .\install.ps1
#
# The repository is private, so downloading needs a GitHub token with read access to it.
# The token is used for this one download and is not stored anywhere.

$repo      = "SemkoArtem/TL_Supporter"
$file      = "download/TLSupporter.exe"
$branch    = "main"
$setup     = "C:\TLSupporterSetup"
$target    = Join-Path $setup "TLSupporter.exe"
$installed = "C:\Program Files\TLSupporter\TLSupporter.exe"

$identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "Asking for administrator rights..."
    Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    exit
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

    $secret = Read-Host "GitHub token" -AsSecureString
    $pointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secret)
    try {
        $token = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($pointer)
    } finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($pointer)
    }

    # An earlier version has to go first. The team and settings stay.
    if ((Get-Service TLSupporter -ErrorAction SilentlyContinue) -and (Test-Path $installed)) {
        Write-Host "Removing the previous version..."
        Start-Process $installed -ArgumentList "--uninstall" -Wait
    }
    Get-Process TLSupporter -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 1

    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $url = "https://api.github.com/repos/$repo/contents/$($file)?ref=$branch"
    $headers = @{
        Authorization = "Bearer $token"
        Accept        = "application/vnd.github.raw"
        "User-Agent"  = "TLSupporter-installer"
    }
    Write-Host "Downloading the program..."
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $url -Headers $headers -OutFile $target -UseBasicParsing
    $token = $null
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
