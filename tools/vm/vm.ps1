# SPDX-License-Identifier: GPL-3.0-only
# Host-side helpers for the Hyper-V test VM. Dot-source it: `. tools\vm\vm.ps1`.
# Needs Hyper-V Administrators membership only (no elevation). Touches only the VM.
#
# Credentials: Import-Clixml of a file outside the repo (default
# %USERPROFILE%\.pulsar-seat\vm-creds.xml) holding PSCredentials `.pstest` (admin,
# auto-logon on the console) and `.seat1` .. `.seat3` (standard users). Passwords are
# never printed; they only travel as PSCredential objects into the VM.
#
#   Reset-TestVm                          restore checkpoint `clean-logged-on`, wait for PS Direct
#   Install-TestMsi [-Msi path]           copy MSI + seatctl.ps1, msiexec /qn, print a summary
#   Invoke-Vm { ... } [-ArgumentList ..]  run a script block as pstest (session 0)
#   Invoke-Seatctl status                 run tools\vm\seatctl.ps1 in the VM (pstest, elevated token)
#   Get-VmReport [-Tail 60]               quser, Pulsar processes, service log tail, Pulsar events

$script:VmName = if ($env:PSEAT_VM) { $env:PSEAT_VM } else { "PulsarSeatTest-arm64" }
$script:VmCredFile = if ($env:PSEAT_VM_CREDS) { $env:PSEAT_VM_CREDS } else { "$env:USERPROFILE\.pulsar-seat\vm-creds.xml" }
$script:Repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$script:VmCreds = $null

function Get-VmCreds { if (-not $script:VmCreds) { $script:VmCreds = Import-Clixml $script:VmCredFile }; $script:VmCreds }

function Wait-VmReady([int] $Seconds = 240) {
    $c = (Get-VmCreds).pstest
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        try { Invoke-Command -VMName $script:VmName -Credential $c { 1 } -ErrorAction Stop | Out-Null; return } catch { Start-Sleep 3 }
    }
    throw "VM $script:VmName not reachable over PowerShell Direct"
}

function Reset-TestVm([string] $Checkpoint = "clean-logged-on") {
    Restore-VMCheckpoint -VMName $script:VmName -Name $Checkpoint -Confirm:$false
    if ((Get-VM $script:VmName).State -ne 'Running') { Start-VM $script:VmName }
    Wait-VmReady
    "restored $Checkpoint"
}

function Invoke-Vm([scriptblock] $Script, [object[]] $ArgumentList = @(), [pscredential] $Credential) {
    if (-not $Credential) { $Credential = (Get-VmCreds).pstest }
    Invoke-Command -VMName $script:VmName -Credential $Credential -ScriptBlock $Script -ArgumentList $ArgumentList
}

function Copy-ToVm([string] $Source, [string] $Destination) {
    Copy-VMFile -Name $script:VmName -SourcePath $Source -DestinationPath $Destination -FileSource Host -CreateFullPath -Force
}

function Push-Seatctl { Copy-ToVm (Join-Path $script:Repo "tools\vm\seatctl.ps1") "C:\pstest\seatctl.ps1" }

# Named seatctl.ps1 arguments go in a hashtable, e.g.
#   Invoke-Seatctl password @{ Seat = 2; Credential = (Get-VmCreds).seat2 }
#   Invoke-Seatctl start @{ Seats = @(2) }
function Invoke-Seatctl([string] $Command, [hashtable] $Named = @{}) {
    $named = $Named
    Invoke-Vm -ArgumentList $Command, $named {
        param($cmd, $named)
        Set-ExecutionPolicy -Scope Process Bypass -Force  # this process only
        & C:\pstest\seatctl.ps1 $cmd @named
    }
}

function Install-TestMsi([string] $Msi = (Join-Path $script:Repo "target\installer\dist\PulsarSeat_0.1.0_arm64.msi")) {
    Invoke-Vm { New-Item -ItemType Directory -Force C:\pstest | Out-Null }
    Copy-ToVm $Msi "C:\pstest\ps.msi"
    Push-Seatctl
    Invoke-Vm {
        $p = Start-Process msiexec -ArgumentList '/i', 'C:\pstest\ps.msi', '/qn', '/l*v', 'C:\pstest\install.log' -Wait -PassThru
        "msiexec exit=$($p.ExitCode)"
        for ($i = 0; $i -lt 30 -and -not (Test-Path \\.\pipe\PulsarSeat.Control); $i++) { Start-Sleep 1 }
        $svc = Get-CimInstance Win32_Service -Filter "Name='PulsarSeat'"
        if ($svc) { "service state=$($svc.State) pid=$($svc.ProcessId)" } else { "no service" }
        "pipe: " + (Test-Path \\.\pipe\PulsarSeat.Control)
    }
}

function Get-VmReport([int] $Tail = 60, [int] $Minutes = 15) {
    Invoke-Vm -ArgumentList $Tail, $Minutes {
        param($tail, $minutes)
        "-- quser"; (quser 2>&1) -join "`n"
        "-- processes"
        Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'pulsar|LogonUI|explorer|winlogon' } |
            ForEach-Object { "{0} pid={1} session={2} {3}" -f $_.Name, $_.ProcessId, $_.SessionId, (($_.CommandLine -replace '--plan \S+', '--plan <hex>') -replace '--token \S+', '--token <t>') }
        "-- service.log (tail $tail)"
        Get-Content "$env:ProgramData\Pulsar Seat\logs\service.log" -Tail $tail -ErrorAction SilentlyContinue
        "-- events ($minutes min)"
        Get-WinEvent -FilterHashtable @{ LogName = 'Application', 'System'; StartTime = (Get-Date).AddMinutes(-$minutes) } -ErrorAction SilentlyContinue |
            Where-Object { $_.ProviderName -match 'Pulsar' -or ($_.ProviderName -match 'Winlogon|User Profile|TermService|LocalSessionManager' -and $_.Level -le 3) } |
            Select-Object -First 30 | ForEach-Object { "[{0}] {1} {2} L{3}: {4}" -f $_.TimeCreated.ToString('T'), $_.ProviderName, $_.Id, $_.Level, (($_.Message -replace '\s+', ' ') -replace '^(.{0,300}).*', '$1') }
    }
}
