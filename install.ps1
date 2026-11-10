# TL Supporter installer.
# Creates the setup folder, adds the program to the Windows Defender exclusions,
# downloads the current version and starts it.
#
# Run it from PowerShell:   powershell -ExecutionPolicy Bypass -File .\install.ps1
#
# The repository is private, so downloading needs a GitHub token with read access to it.
# The token is used for this one download and is not stored anywhere.

$repo   = "SemkoArtem/TL_Supporter"
$file   = "download/TLSupporter.exe"
$branch = "main"
$setup  = "C:\TLSupporterSetup"
$target = Join-Path $setup "TLSupporter.exe"

$identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "Asking for administrator rights..."
    Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    exit
}

$ErrorActionPreference = "Stop"
try {
    New-Item -ItemType Directory -Force $setup | Out-Null
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
    Write-Host "Starting the program..."
    Start-Process $target
    Write-Host "Done."
} catch {
    Write-Host "Failed: $($_.Exception.Message)" -ForegroundColor Red
}
Read-Host "Press Enter to close"
