# TL Supporter uninstaller.
# Removes the program from this PC completely: the Windows service, the program, the firewall rule,
# the Defender exclusions and all its data, including the team this PC belonged to.
#
# Run it from PowerShell:   powershell -ExecutionPolicy Bypass -File .\uninstall.ps1
# Only list what is installed, without removing anything:   ... -File .\uninstall.ps1 -Check
# Removing needs administrator rights. Without them nothing is changed.

param([switch]$Check)

$service    = "TLSupporter"
$programDir = Join-Path $env:ProgramFiles "TLSupporter"
$secureDir  = Join-Path $env:ProgramFiles "TLSupporterData"
$dataDir    = Join-Path $env:ProgramData "TLSupporter"
$setupDir   = "C:\TLSupporterSetup"
$exe        = Join-Path $programDir "TLSupporter.exe"
$exclusions = @($setupDir, $programDir)
$policyKey  = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System"

function Show-Installed {
    $svc = Get-Service $service -ErrorAction SilentlyContinue
    Write-Host ("Service:            " + $(if ($svc) { "$($svc.Status), start type $($svc.StartType)" } else { "not installed" }))
    Write-Host ("Running copies:     " + @(Get-Process TLSupporter -ErrorAction SilentlyContinue).Count)
    foreach ($dir in @($programDir, $secureDir, $dataDir, $setupDir)) {
        Write-Host ("Folder:             " + $dir + " - " + $(if (Test-Path $dir) { "present" } else { "absent" }))
    }
    netsh advfirewall firewall show rule name=TLSupporter | Out-Null
    Write-Host ("Firewall rule:      " + $(if ($LASTEXITCODE -eq 0) { "present" } else { "absent" }))
    try {
        $current = @((Get-MpPreference -ErrorAction Stop).ExclusionPath)
        # Without administrator rights Windows hides the list behind a short note.
        if ($current | Where-Object { "$_" -like "N/A*" }) { throw "hidden" }
        $ours = @($exclusions | Where-Object { $current -contains $_ })
        Write-Host ("Defender exclusions: " + $(if ($ours.Count) { $ours -join ", " } else { "none" }))
    } catch {
        Write-Host "Defender exclusions: visible only with administrator rights"
    }
    $sas = (Get-ItemProperty $policyKey -Name SoftwareSASGeneration -ErrorAction SilentlyContinue).SoftwareSASGeneration
    Write-Host ("Ctrl+Alt+Del policy: " + $(if ($null -ne $sas) { "set to $sas" } else { "not set" }))
}

if ($Check) {
    Write-Host "TL Supporter on this PC:"
    Show-Installed
    exit 0
}

$identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    # Nothing below runs without administrator rights. Windows asks for them, and on an account
    # without them it asks for the password of an administrator.
    Write-Host "Administrator rights are needed. Windows will ask for them now."
    try {
        Start-Process powershell.exe -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`"" -ErrorAction Stop
    } catch {
        Write-Host "Nothing was removed: administrator rights were not given." -ForegroundColor Red
        Read-Host "Press Enter to close"
    }
    exit 1
}

Write-Host "TL Supporter on this PC:"
Show-Installed
Write-Host ""
Write-Host "Everything above will be removed, including the team data of this PC." -ForegroundColor Yellow
Write-Host "Afterwards a lead should also revoke this PC's access in the console."
$answer = Read-Host "Type YES to remove TL Supporter"
if ($answer -cne "YES") {
    Write-Host "Nothing was removed."
    Read-Host "Press Enter to close"
    exit 0
}

$failed = @()
function Step($text, [scriptblock]$action) {
    try {
        & $action
        Write-Host "Done:   $text"
    } catch {
        Write-Host "Failed: $text ($($_.Exception.Message))" -ForegroundColor Red
        $script:failed += $text
    }
}

# The program removes its own service, firewall rule and folder when it is there.
if (Test-Path $exe) {
    Step "program's own uninstall" { Start-Process $exe -ArgumentList "--uninstall" -Wait -WindowStyle Hidden }
}
# Whatever is left, for example when the program file is already gone.
if (Get-Service $service -ErrorAction SilentlyContinue) {
    Step "stop and delete the service" {
        sc.exe stop $service | Out-Null
        Start-Sleep -Seconds 2
        sc.exe delete $service | Out-Null
        Start-Sleep -Seconds 1
        if (Get-Service $service -ErrorAction SilentlyContinue) { throw "the service is still registered" }
    }
}
Step "close running copies" {
    Get-Process TLSupporter -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 1
}
Step "remove the firewall rule" {
    netsh advfirewall firewall delete rule name=TLSupporter | Out-Null
}
foreach ($dir in @($programDir, $secureDir, $dataDir, $setupDir)) {
    if (Test-Path $dir) {
        Step "delete $dir" {
            Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
            if (Test-Path $dir) {
                Start-Sleep -Seconds 2
                Remove-Item -Recurse -Force $dir
            }
        }
    }
}
Step "remove the Defender exclusions" {
    foreach ($path in $exclusions) { Remove-MpPreference -ExclusionPath $path -ErrorAction SilentlyContinue }
}
# Set by the installer so that a lead could press Ctrl+Alt+Del. Only our value is taken away.
if ((Get-ItemProperty $policyKey -Name SoftwareSASGeneration -ErrorAction SilentlyContinue).SoftwareSASGeneration -eq 1) {
    Step "remove the Ctrl+Alt+Del policy" { Remove-ItemProperty $policyKey -Name SoftwareSASGeneration }
}

Write-Host ""
if ($failed.Count) {
    Write-Host "Some steps failed. Restart Windows and run this script again." -ForegroundColor Red
} else {
    Write-Host "TL Supporter was removed from this PC." -ForegroundColor Green
}
Read-Host "Press Enter to close"
