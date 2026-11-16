/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * One IddCx driver, two roles (drivers/README.md, merged 2026-11-14 so the driver is
 * signed once):
 *
 * - local:  root devnode Root\PulsarSeatIdd plus one software devnode PulsarSeatIdd_Adapter
 *           per further render GPU, virtual monitors per local seat, control interface
 *           IDD_INTERFACE_GUID (driver.cpp, swapchain.cpp, seat_idd_ioctl.h);
 * - remote: remote-session device PulsarSplit_IndirectDisplay created by a Remote Desktop
 *           protocol stack, one monitor per remote seat session, control interface
 *           {B5FCB55F-1E09-4C81-A58F-0362792CFE99} (remote/, PulsarIddShared.h).
 *
 * DriverEntry is here; the device add picks the role from the devnode's hardware ids.
 * Every devnode runs in its own WUDFHost (UmdfHostProcessSharing = ProcessSharingDisabled
 * in both INF install sections), so the roles never share a host process.
 * Built unsigned (WDK 10.0.28000), never loaded, untested on hardware.
 */
#include "driver.h"
#include "roles.h"

/* Case-insensitive comparison of one hardware id (ASCII ids only). */
static BOOL HwidEquals(const WCHAR *a, const WCHAR *b)
{
    for (;; a++, b++) {
        WCHAR x = *a, y = *b;
        if (x >= L'a' && x <= L'z') x = (WCHAR)(x - L'a' + L'A');
        if (y >= L'a' && y <= L'z') y = (WCHAR)(y - L'a' + L'A');
        if (x != y) return FALSE;
        if (x == 0) return TRUE;
    }
}

PSIDD_ROLE PsiddRoleOfHardwareIds(const WCHAR *multiSz, size_t cch)
{
    size_t i = 0;
    while (i < cch && multiSz[i] != 0) {
        const WCHAR *id = &multiSz[i];
        size_t len = 0;
        while (i + len < cch && id[len] != 0) len++;
        if (i + len >= cch) break; /* not terminated inside the buffer */
        if (HwidEquals(id, PSIDD_HWID_LOCAL) || HwidEquals(id, PSIDD_HWID_LOCAL_ADAPTER)) return PsiddRoleLocal;
        if (HwidEquals(id, PSIDD_HWID_REMOTE)) return PsiddRoleRemote;
        i += len + 1;
    }
    return PsiddRoleUnknown;
}

static PSIDD_ROLE RoleOf(PWDFDEVICE_INIT DeviceInit)
{
    WCHAR buf[512];
    ULONG len = 0;
    RtlZeroMemory(buf, sizeof(buf));
    /* Hardware ids as REG_MULTI_SZ (WdfFdoInitQueryProperty, UMDF 2.0+). A list longer
     * than the buffer is not ours. */
    NTSTATUS st = WdfFdoInitQueryProperty(DeviceInit, DevicePropertyHardwareID, sizeof(buf) - 2 * sizeof(WCHAR), buf, &len);
    if (!NT_SUCCESS(st)) {
        return PsiddRoleUnknown;
    }
    return PsiddRoleOfHardwareIds(buf, ARRAYSIZE(buf));
}

static EVT_WDF_DRIVER_DEVICE_ADD PsiddRoleDeviceAdd;

static NTSTATUS PsiddRoleDeviceAdd(WDFDRIVER Driver, PWDFDEVICE_INIT DeviceInit)
{
    switch (RoleOf(DeviceInit)) {
    case PsiddRoleLocal:
        return PsiddDeviceAdd(Driver, DeviceInit);
    case PsiddRoleRemote:
        return pulsar::RemoteDeviceAdd(Driver, DeviceInit);
    default:
        /* An INF edit added a hardware id without a role: refuse the device. */
        return STATUS_NOT_SUPPORTED;
    }
}

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_OBJECT_ATTRIBUTES attributes;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    WDF_DRIVER_CONFIG_INIT(&config, PsiddRoleDeviceAdd);
    return WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, WDF_NO_HANDLE);
}
