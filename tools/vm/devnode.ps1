# SPDX-License-Identifier: GPL-3.0-only
# Root-enumerated test devnodes for the Hyper-V test VM (TEST ONLY, VM driver rounds).
# Runs inside the VM as an administrator (PowerShell Direct session of pstest). Windows
# PowerShell 5.1, no extra binary: the documented SetupAPI / newdev calls a devnode
# installer makes (SetupDiCreateDeviceInfoW + SPDRP_HARDWAREID + DIF_REGISTERDEVICE, then
# UpdateDriverForPlugAndPlayDevicesW), the same steps drivers/seat-idd/README.md
# "Install" lists for the product's root devnode.
#
#   devnode.ps1 -Action Add -HardwareId Root\PulsarSeatIdd -Inf C:\pstest\drv\seat-idd\seat-idd.inf
#   devnode.ps1 -Action Add -HardwareId PulsarSplit_IndirectDisplay -Inf ...   (remote IDD role)
#   devnode.ps1 -Action Remove -HardwareId Root\PulsarSeatIdd                 (every devnode with it)
#   devnode.ps1 -Action List -HardwareId Root\PulsarSeatIdd
param(
    [ValidateSet("Add", "Remove", "List")] [string] $Action = "List",
    [Parameter(Mandatory = $true)] [string] $HardwareId,
    [string] $Inf = "",
    # Device setup class of the INF (Display for seat-idd).
    [string] $ClassGuid = "{4d36e968-e325-11ce-bfc1-08002be10318}",
    [string] $ClassName = "Display"
)
$ErrorActionPreference = "Stop"

if (-not ("PulsarSeatTest.DevNode" -as [type])) {
    Add-Type -Namespace PulsarSeatTest -Name DevNode -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)]
public struct SP_DEVINFO_DATA { public uint cbSize; public Guid ClassGuid; public uint DevInst; public IntPtr Reserved; }
[DllImport("setupapi.dll", SetLastError = true)]
public static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid classGuid, IntPtr hwnd);
[DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern bool SetupDiCreateDeviceInfoW(IntPtr set, string name, ref Guid classGuid, string desc, IntPtr hwnd, uint flags, ref SP_DEVINFO_DATA data);
[DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set, ref SP_DEVINFO_DATA data, uint prop, byte[] buf, uint size);
[DllImport("setupapi.dll", SetLastError = true)]
public static extern bool SetupDiCallClassInstaller(uint dif, IntPtr set, ref SP_DEVINFO_DATA data);
[DllImport("setupapi.dll", SetLastError = true)]
public static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
[DllImport("newdev.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr hwnd, string hwid, string inf, uint flags, out bool reboot);
'@
}

function Get-Nodes {
    @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object {
            $ids = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName DEVPKEY_Device_HardwareIds -ErrorAction SilentlyContinue).Data
            $ids -and ($ids -contains $HardwareId)
        })
}

switch ($Action) {
    "List" {
        Get-Nodes | ForEach-Object { "{0} status={1} problem={2}" -f $_.InstanceId, $_.Status, $_.Problem }
    }
    "Add" {
        if (-not (Test-Path $Inf)) { throw "INF not found: $Inf" }
        $g = [Guid]$ClassGuid
        $set = [PulsarSeatTest.DevNode]::SetupDiCreateDeviceInfoList([ref]$g, [IntPtr]::Zero)
        if ($set -eq [IntPtr]-1) { throw "SetupDiCreateDeviceInfoList failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
        try {
            $d = New-Object PulsarSeatTest.DevNode+SP_DEVINFO_DATA
            $d.cbSize = [Runtime.InteropServices.Marshal]::SizeOf($d)
            # DICD_GENERATE_ID = 1: ROOT\<ClassName>\NNNN
            if (-not [PulsarSeatTest.DevNode]::SetupDiCreateDeviceInfoW($set, $ClassName, [ref]$g, $null, [IntPtr]::Zero, 1, [ref]$d)) {
                throw "SetupDiCreateDeviceInfoW failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
            }
            $bytes = [Text.Encoding]::Unicode.GetBytes("$HardwareId`0`0")
            # SPDRP_HARDWAREID = 1
            if (-not [PulsarSeatTest.DevNode]::SetupDiSetDeviceRegistryPropertyW($set, [ref]$d, 1, $bytes, $bytes.Length)) {
                throw "SPDRP_HARDWAREID failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
            }
            # DIF_REGISTERDEVICE = 0x19
            if (-not [PulsarSeatTest.DevNode]::SetupDiCallClassInstaller(0x19, $set, [ref]$d)) {
                throw "DIF_REGISTERDEVICE failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
            }
        } finally { [void][PulsarSeatTest.DevNode]::SetupDiDestroyDeviceInfoList($set) }
        $reboot = $false
        # INSTALLFLAG_FORCE = 1
        if (-not [PulsarSeatTest.DevNode]::UpdateDriverForPlugAndPlayDevicesW([IntPtr]::Zero, $HardwareId, (Resolve-Path $Inf).Path, 1, [ref]$reboot)) {
            throw "UpdateDriverForPlugAndPlayDevicesW failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
        }
        "added $HardwareId (reboot needed: $reboot)"
        Get-Nodes | ForEach-Object { "{0} status={1} problem={2}" -f $_.InstanceId, $_.Status, $_.Problem }
    }
    "Remove" {
        foreach ($n in Get-Nodes) {
            $o = pnputil /remove-device "$($n.InstanceId)" 2>&1
            "remove $($n.InstanceId): exit $LASTEXITCODE $(($o | Select-Object -Last 1))"
        }
    }
}
