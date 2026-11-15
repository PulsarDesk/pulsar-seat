# SPDX-License-Identifier: GPL-3.0-only
# U1 / U3 scenario steps for the Hyper-V test VM (spec/local/35 §5.1). Dot-source after
# vm.ps1:  . tools\vm\vm.ps1; . tools\vm\scenarios.ps1
#
#   Set-U1Config     fast_user_switch; seat 2 = local user seat1, seat 3 = seat2; passwords
#                    stored through the service (never printed); product enabled
#   Start-U1Seat 2   lock the console (Win+L), start_seats [2], screenshot
#   Set-U3Config     desktops; seat1_desktop=false; kiosk shells on seats 2 and 3 (charmap +
#                    cmd, cmd); keyboard and mouse on seat 2. The VM has ONE monitor, which
#                    seat 1 must keep (seat_one_no_display), so seats 2/3 wait for a display.
#   Set-U3Seat1Config  desktops, seat1_desktop=false, kiosk shell on seat 1 (one monitor ok)
#   Get-SeatSummary  one line per seat from get_status
#
# Each Set-* edits the config the service returns (get_config) inside the VM, so device
# keys and defaults always match the installed build.

function Get-SeatSummary {
    $j = Invoke-Seatctl status | ConvertFrom-Json
    $j.data.seats | ForEach-Object { "seat {0}: {1} session={2} user={3} {4}" -f $_.seat, $_.state.state, $_.session_id, $_.state.user, ($_.degraded -join ',') }
}

# Runs $Edit (a script block over the config object $cfg, plus $Arg) inside the VM and
# applies the result.
function Update-VmConfig([scriptblock] $Edit, $Arg = $null) {
    Invoke-Seatctl config @{ Out = 'C:\pstest\cfg-cur.json' } | Out-Null
    Invoke-Vm -ArgumentList $Edit.ToString(), $Arg {
        param($edit, $arg)
        $cfg = Get-Content -Raw C:\pstest\cfg-cur.json | ConvertFrom-Json
        & ([scriptblock]::Create($edit)) $cfg $arg
        $cfg | ConvertTo-Json -Depth 64 | Set-Content -Encoding utf8 C:\pstest\cfg-new.json
    }
    Invoke-Seatctl apply @{ File = 'C:\pstest\cfg-new.json' }
}

function Set-U1Config {
    Update-VmConfig {
        param($cfg)
        function seat($cfg, $n, $user) {
            $s = $cfg.seats[0] | ConvertTo-Json -Depth 32 | ConvertFrom-Json
            $s.number = $n; $s.name = "$n"; $s.autostart = $false
            $s.account = [pscustomobject]@{ kind = 'local'; user = $user }
            $s
        }
        $cfg.seats = @($cfg.seats[0], (seat $cfg 2 'seat1'), (seat $cfg 3 'seat2'))
        $cfg.settings.backends.session = 'fast_user_switch'
    }
    $c = Get-VmCreds
    Invoke-Seatctl password @{ Seat = 2; Credential = $c.seat1 }
    Invoke-Seatctl password @{ Seat = 3; Credential = $c.seat2 }
    Invoke-Seatctl enable
}

function Start-U1Seat([int] $Seat = 2, [int] $Wait = 20) {
    & (Join-Path $PSScriptRoot 'ui.ps1') -Do "key:0x5B+0x4C; wait:3"
    Invoke-Seatctl start @{ Seats = @($Seat) }
    Start-Sleep $Wait
    & (Join-Path $PSScriptRoot 'ui.ps1') -Do "shot:u1-seat$Seat"
}

function Set-U3Config {
    Update-VmConfig {
        param($cfg)
        function app($t, $c) { [pscustomobject]@{ title = $t; command = $c; args = @(); autostart = $true; monitor = 0 } }
        function seat($cfg, $n, $apps) {
            $s = $cfg.seats[0] | ConvertTo-Json -Depth 32 | ConvertFrom-Json
            $s.number = $n; $s.name = "$n"; $s.autostart = $false
            $s.local = [pscustomobject]@{ kiosk_shell = [pscustomobject]@{ apps = $apps; idle_reset_minutes = 0 } }
            $s
        }
        $cmd = 'C:\Windows\System32\cmd.exe'
        $cfg.seats = @($cfg.seats[0],
            (seat $cfg 2 @((app 'Karakter' 'C:\Windows\System32\charmap.exe'), (app 'Komut' $cmd))),
            (seat $cfg 3 @((app 'Komut' $cmd))))
        $cfg.settings.backends.session = 'desktops'
        $cfg.settings.kiosk.seat1_desktop = $false
    }
    # Keyboard and mouse to seat 2 (mask 2); the monitor stays on seat 1.
    $devs = (Invoke-Seatctl devices | ConvertFrom-Json).data | Where-Object { $_.device.kind -in 'keyboard', 'mouse' }
    $list = @($devs | ForEach-Object { @{ key = $_.device.key; kind = $_.device.kind; mask = 2; name = $_.device.name } })
    Update-VmConfig { param($cfg, $arg) $cfg.devices = @($arg | ForEach-Object { [pscustomobject]$_ }) } $list
    Invoke-Seatctl enable
}

# U3 with seat 1 as the kiosk seat (maintainer decision 2026-11-15): desktops,
# seat1_desktop=false, kiosk shell on seat 1 (charmap + cmd). The VM's only monitor,
# keyboard and mouse stay on seat 1, so the shell gets all of them.
function Set-U3Seat1Config {
    Update-VmConfig {
        param($cfg)
        function app($t, $c) { [pscustomobject]@{ title = $t; command = $c; args = @(); autostart = $true; monitor = 0 } }
        $s1 = $cfg.seats[0]
        $shell = [pscustomobject]@{ apps = @((app 'Karakter' 'C:\Windows\System32\charmap.exe'), (app 'Komut' 'C:\Windows\System32\cmd.exe')); idle_reset_minutes = 0 }
        if ($s1.PSObject.Properties['local'] -and $s1.local) { $s1.local | Add-Member -Force kiosk_shell $shell }
        else { $s1 | Add-Member -Force local ([pscustomobject]@{ kiosk_shell = $shell }) }
        $cfg.seats = @($s1)
        $cfg.devices = @()
        $cfg.settings.backends.session = 'desktops'
        $cfg.settings.kiosk.seat1_desktop = $false
    }
    Invoke-Seatctl enable
}
