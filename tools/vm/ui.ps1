# SPDX-License-Identifier: GPL-3.0-only
# VM UI driver over Hyper-V WMI (keyboard, mouse, screenshot). Touches only the VM.
# Host side (Hyper-V Administrators, no elevation). Screenshots land in -OutDir
# (default <repo>\target\vm\shots, gitignored) as <name>.png. Keys are VK codes in hex;
# a "+" chord presses in order and releases in reverse. The synthetic mouse does not
# register clicks in the basic session: drive the UI with the keyboard.
# Usage: ui.ps1 -Do "key:0x5B+0x52; type:notepad; key:0x0D; wait:2; shot:name"
# `cred:<name>` types the password of credential <name> (pstest, seat1, ...) from the
# credential file (see vm.ps1) into the VM; it is never printed.
param([string] $Do, [string] $VmName = "PulsarSeatTest-arm64", [string] $OutDir = (Join-Path $PSScriptRoot "..\..\target\vm\shots"),
    [string] $CredFile = $(if ($env:PSEAT_VM_CREDS) { $env:PSEAT_VM_CREDS } else { "$env:USERPROFILE\.pulsar-seat\vm-creds.xml" }))
Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force $OutDir | Out-Null
$dir = (Resolve-Path $OutDir).Path
$id = (Get-VM $VmName).Id.Guid
$ns = "root\virtualization\v2"
$kb = Get-CimInstance -Namespace $ns -ClassName Msvm_Keyboard | Where-Object SystemName -ieq $id
$ms = Get-CimInstance -Namespace $ns -ClassName Msvm_SyntheticMouse | Where-Object SystemName -ieq $id
function Shot([string] $name, [int] $W = 1024, [int] $H = 768) {
    $vm = Get-CimInstance -Namespace $ns -ClassName Msvm_ComputerSystem -Filter "Name='$id'"
    $sd = Get-CimAssociatedInstance -InputObject $vm -ResultClassName Msvm_VirtualSystemSettingData | Where-Object VirtualSystemType -eq 'Microsoft:Hyper-V:System:Realized'
    $svc = Get-CimInstance -Namespace $ns -ClassName Msvm_VirtualSystemManagementService
    $r = Invoke-CimMethod -InputObject $svc -MethodName GetVirtualSystemThumbnailImage -Arguments @{ TargetSystem = $sd; WidthPixels = [uint16]$W; HeightPixels = [uint16]$H }
    [byte[]]$b = $r.ImageData
    $bmp = New-Object System.Drawing.Bitmap $W, $H, ([System.Drawing.Imaging.PixelFormat]::Format16bppRgb565)
    $bd = $bmp.LockBits((New-Object System.Drawing.Rectangle 0, 0, $W, $H), [System.Drawing.Imaging.ImageLockMode]::WriteOnly, $bmp.PixelFormat)
    [System.Runtime.InteropServices.Marshal]::Copy($b, [int]0, [IntPtr]$bd.Scan0, [int][Math]::Min($b.Length, $bd.Stride * $H))
    $bmp.UnlockBits($bd); $p = Join-Path $dir "$name.png"; $bmp.Save($p, [System.Drawing.Imaging.ImageFormat]::Png); "shot $p"
}
function Key([string] $spec) {
    $codes = $spec.Split('+') | ForEach-Object { [uint32]([Convert]::ToInt32($_.Trim(), 16)) }
    if ($codes.Count -eq 1) { Invoke-CimMethod -InputObject $kb -MethodName TypeKey -Arguments @{ keyCode = $codes[0] } | Out-Null; return }
    foreach ($k in $codes) { Invoke-CimMethod -InputObject $kb -MethodName PressKey -Arguments @{ keyCode = $k } | Out-Null }
    [array]::Reverse($codes); foreach ($k in $codes) { Invoke-CimMethod -InputObject $kb -MethodName ReleaseKey -Arguments @{ keyCode = $k } | Out-Null }
}
function Click([int] $x, [int] $y, [int] $W = 1024, [int] $H = 768, [int] $btn = 1) {
    # Shot coordinates (W x H) -> VM screen; SetAbsolutePosition takes guest pixels.
    $vr = (Get-VMVideo $VmName); $gw = $vr.HorizontalResolution; $gh = $vr.VerticalResolution
    if (-not $gw) { $gw = 1024; $gh = 768 }
    $gx = [int]($x * $gw / $W); $gy = [int]($y * $gh / $H)
    Invoke-CimMethod -InputObject $ms -MethodName SetAbsolutePosition -Arguments @{ horizontalPosition = [int32]$gx; verticalPosition = [int32]$gy } | Out-Null
    Start-Sleep -Milliseconds 200
    Invoke-CimMethod -InputObject $ms -MethodName ClickButton -Arguments @{ buttonIndex = [uint32]$btn } | Out-Null
}
foreach ($step in ($Do -split ';')) {
    $step = $step.Trim(); if (-not $step) { continue }
    $k, $v = $step.Split(':', 2)
    switch ($k) {
        'key'   { Key $v }
        'type'  { Invoke-CimMethod -InputObject $kb -MethodName TypeText -Arguments @{ asciiText = $v } | Out-Null }
        'cred'  { Invoke-CimMethod -InputObject $kb -MethodName TypeText -Arguments @{ asciiText = (Import-Clixml $CredFile).$v.GetNetworkCredential().Password } | Out-Null }
        'wait'  { Start-Sleep -Seconds ([double]$v) }
        'click' { $x, $y = $v.Split(','); Click ([int]$x) ([int]$y) }
        'dbl'   { $x, $y = $v.Split(','); Click ([int]$x) ([int]$y); Start-Sleep -Milliseconds 80; Click ([int]$x) ([int]$y) }
        'rclick'{ $x, $y = $v.Split(','); Click ([int]$x) ([int]$y) -btn 2 }
        'shot'  { Shot $v }
    }
}
