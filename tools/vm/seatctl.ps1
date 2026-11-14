# SPDX-License-Identifier: GPL-3.0-only
# Pulsar Seat control pipe test client (protocol 3). Runs inside the test VM (or any PC
# with the service), Windows PowerShell 5.1, no build needed.
#
# Frame = u32 LE length + UTF-8 JSON (docs/agents/architecture.md section 5). Every
# connection sends `hello` first; `Admin` requests need an elevated token or SYSTEM.
#
# Usage (one command per run; output is JSON):
#   seatctl.ps1 hello
#   seatctl.ps1 status | backends | devices | users | preflight
#   seatctl.ps1 config [-Out cfg.json]            get_config (-Out: only the config document)
#   seatctl.ps1 validate -File cfg.json           validate_config
#   seatctl.ps1 apply -File cfg.json [-Force]     apply_config (base_revision from the file)
#   seatctl.ps1 enable | disable                  set_enabled
#   seatctl.ps1 start [-Seats 2,3]                start_seats (async; watch `status`)
#   seatctl.ps1 stop -Seat 2                      stop_seat
#   seatctl.ps1 password -Seat 2 -Credential $cred   set_seat_password (never printed)
#   seatctl.ps1 clear-password -Seat 2            set_seat_password with no password
#   seatctl.ps1 poll [-After 0] [-WaitMs 0] [-Topics status,devices]
#   seatctl.ps1 subscribe [-Topics status] [-Seconds 10]   print pushed events
#   seatctl.ps1 raw -Json '{"type":"get_status"}'  any request body
#   seatctl.ps1 seq -Json '[{...},{...}]'          several bodies on one connection
param(
    [Parameter(Position = 0)] [string] $Command = "hello",
    [string] $File, [string] $Out, [switch] $Force,
    [int[]] $Seats, [int] $Seat, [pscredential] $Credential,
    [uint64] $After = 0, [uint32] $WaitMs = 0, [string[]] $Topics = @(), [int] $Seconds = 10,
    [string] $Json, [string] $Pipe = "PulsarSeat.Control", [int] $Depth = 64
)
$ErrorActionPreference = "Stop"

if (-not ("PulsarSeatTest.PipeNative" -as [type])) {
    Add-Type -Namespace PulsarSeatTest -Name PipeNative -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern Microsoft.Win32.SafeHandles.SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr sa, uint disposition, uint flags, IntPtr template);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool GetNamedPipeServerProcessId(Microsoft.Win32.SafeHandles.SafeFileHandle h, out uint pid);
'@
}

# seat_proto::PIPE_CLIENT_ACCESS (no FILE_APPEND_DATA), SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION.
$PIPE_CLIENT_ACCESS = [uint32]0x0012018b
$SQOS_IDENTIFICATION = [uint32](0x00100000 -bor 0x00010000)

function Open-Pipe {
    $name = "\\.\pipe\$Pipe"
    $h = [PulsarSeatTest.PipeNative]::CreateFileW($name, $PIPE_CLIENT_ACCESS, 0, [IntPtr]::Zero, 3, $SQOS_IDENTIFICATION, [IntPtr]::Zero)
    if ($h.IsInvalid) { throw "open $name failed: win32 $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
    $spid = [uint32]0; [void][PulsarSeatTest.PipeNative]::GetNamedPipeServerProcessId($h, [ref]$spid)
    $svc = Get-CimInstance Win32_Service -Filter "Name='PulsarSeat'" -ErrorAction SilentlyContinue
    if ($svc -and $svc.ProcessId -and $svc.ProcessId -ne $spid) { throw "pipe server pid $spid is not the PulsarSeat service ($($svc.ProcessId))" }
    New-Object IO.FileStream($h, [IO.FileAccess]::ReadWrite, 4096, $false)
}

function Read-Exact([IO.Stream] $s, [int] $n) {
    $buf = New-Object byte[] $n; $got = 0
    while ($got -lt $n) { $r = $s.Read($buf, $got, $n - $got); if ($r -le 0) { throw "pipe closed" }; $got += $r }
    $buf
}

function Write-Frame([IO.Stream] $s, [string] $text) {
    $p = [Text.Encoding]::UTF8.GetBytes($text)
    $all = New-Object byte[] (4 + $p.Length)
    [BitConverter]::GetBytes([uint32]$p.Length).CopyTo($all, 0); $p.CopyTo($all, 4)
    $s.Write($all, 0, $all.Length); $s.Flush()
    [Array]::Clear($p, 0, $p.Length); [Array]::Clear($all, 0, $all.Length)
}

function Read-Frame([IO.Stream] $s) {
    $len = [BitConverter]::ToUInt32((Read-Exact $s 4), 0)
    if ($len -gt 1MB) { throw "frame of $len bytes exceeds the limit" }
    [Text.Encoding]::UTF8.GetString((Read-Exact $s ([int]$len)))
}

$script:nextId = 0
# Sends one body (a JSON string), returns the parsed `result` ({Ok:..} or {Err:..}).
function Invoke-Body([IO.Stream] $s, [string] $bodyJson) {
    $id = ++$script:nextId
    Write-Frame $s ('{"version":3,"id":' + $id + ',"body":' + $bodyJson + '}')
    while ($true) {
        $f = Read-Frame $s | ConvertFrom-Json
        if ($f.response -and $f.response.id -eq $id) { return $f.response.result }
        if ($f.event) { $script:events += , $f.event }
    }
}
$script:events = @()

function Hello([IO.Stream] $s) {
    $r = Invoke-Body $s '{"type":"hello","data":{"client":"cli","client_version":"vm-test","protocol_version":3}}'
    if ($r.Err) { throw ("hello refused: " + ($r.Err | ConvertTo-Json -Compress)) }
    $r
}

function Body([string] $type, $data) {
    if ($null -eq $data) { return (@{ type = $type } | ConvertTo-Json -Compress) }
    @{ type = $type; data = $data } | ConvertTo-Json -Compress -Depth $Depth
}

function Emit($r) {
    if ($r.Err) { $r | ConvertTo-Json -Depth $Depth; $global:LASTEXITCODE = 2; return }
    $r.Ok | ConvertTo-Json -Depth $Depth
}

$s = Open-Pipe
try {
    $hello = Hello $s
    switch ($Command) {
        'hello'     { Emit $hello }
        'status'    { Emit (Invoke-Body $s (Body 'get_status')) }
        'backends'  { Emit (Invoke-Body $s (Body 'get_backends')) }
        'devices'   { Emit (Invoke-Body $s (Body 'list_devices')) }
        'users'     { Emit (Invoke-Body $s (Body 'list_local_users')) }
        'preflight' { Emit (Invoke-Body $s (Body 'preflight')) }
        'config' {
            $r = Invoke-Body $s (Body 'get_config')
            # ConfigView: { config, ... }; -Out keeps only the config document.
            if ($Out -and $r.Ok) { $r.Ok.data.config | ConvertTo-Json -Depth $Depth | Set-Content -Encoding utf8 $Out; "saved $Out revision=$($r.Ok.data.config.revision)" } else { Emit $r }
        }
        'validate' {
            $cfg = Get-Content -Raw $File | ConvertFrom-Json
            Emit (Invoke-Body $s (Body 'validate_config' @{ config = $cfg }))
        }
        'apply' {
            $cfg = Get-Content -Raw $File | ConvertFrom-Json
            Emit (Invoke-Body $s (Body 'apply_config' @{ config = $cfg; base_revision = [uint64]$cfg.revision; force = [bool]$Force }))
        }
        'enable'    { Emit (Invoke-Body $s (Body 'set_enabled' @{ enabled = $true })) }
        'disable'   { Emit (Invoke-Body $s (Body 'set_enabled' @{ enabled = $false })) }
        'start' {
            $d = if ($Seats) { '{"seats":[' + ($Seats -join ',') + ']}' } else { '{"seats":null}' }
            Emit (Invoke-Body $s ('{"type":"start_seats","data":' + $d + '}'))
        }
        'stop'      { Emit (Invoke-Body $s (Body 'stop_seat' @{ seat = $Seat })) }
        'password' {
            if (-not $Credential) { throw "-Credential required" }
            # In memory only; never printed or logged.
            $pw = $Credential.GetNetworkCredential().Password
            $b = Body 'set_seat_password' @{ seat = $Seat; password = $pw }
            $pw = $null
            Emit (Invoke-Body $s $b); $b = $null
        }
        'clear-password' { Emit (Invoke-Body $s ('{"type":"set_seat_password","data":{"seat":' + $Seat + ',"password":null}}')) }
        'poll' {
            $t = '[' + (($Topics | ForEach-Object { '"' + $_ + '"' }) -join ',') + ']'
            Emit (Invoke-Body $s ('{"type":"poll_events","data":{"after":' + $After + ',"wait_ms":' + $WaitMs + ',"topics":' + $t + '}}'))
        }
        'subscribe' {
            $t = '[' + (($Topics | ForEach-Object { '"' + $_ + '"' }) -join ',') + ']'
            Emit (Invoke-Body $s ('{"type":"subscribe","data":{"topics":' + $t + '}}'))
            foreach ($e in $script:events) { $e | ConvertTo-Json -Depth $Depth -Compress }
            $deadline = (Get-Date).AddSeconds($Seconds)
            # Blocking reads; a reader task keeps the deadline honest.
            $task = $null
            while ((Get-Date) -lt $deadline) {
                if (-not $task) { $hdr = New-Object byte[] 4; $task = $s.ReadAsync($hdr, 0, 4) }
                if ($task.Wait(500)) {
                    $n = $task.Result; $task = $null
                    if ($n -le 0) { "pipe closed"; break }
                    if ($n -lt 4) { $rest = Read-Exact $s (4 - $n); [Array]::Copy($rest, 0, $hdr, $n, 4 - $n) }
                    $len = [BitConverter]::ToUInt32($hdr, 0)
                    [Text.Encoding]::UTF8.GetString((Read-Exact $s ([int]$len)))
                }
            }
        }
        'raw'       { Emit (Invoke-Body $s $Json) }
        'seq' {
            foreach ($b in (ConvertFrom-Json $Json)) { Emit (Invoke-Body $s ($b | ConvertTo-Json -Compress -Depth $Depth)) }
        }
        default     { throw "unknown command $Command" }
    }
} finally { $s.Dispose() }
