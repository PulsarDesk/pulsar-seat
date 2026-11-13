/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat input filter - driver entry, device add, unit bookkeeping.
 *
 * Built with WDK 10.0.28000 (unsigned); NEVER LOADED until Microsoft-signed
 * (docs/agents/plan.md). Untested on hardware.
 */
#include "seatinput.h"

SEATINPUT_GLOBALS g;

/* Documented kernel variable: non-zero in Safe Mode (Microsoft Learn, "Determining
 * Whether the Operating System Is Running in Safe Mode"). */
extern PULONG InitSafeBootMode;

static const GUID SeatClassKeyboard = { 0x4d36e96b, 0xe325, 0x11ce, { 0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18 } };
static const GUID SeatClassMouse    = { 0x4d36e96f, 0xe325, 0x11ce, { 0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18 } };
static const GUID SeatClassHid      = { 0x745a17a0, 0x74d3, 0x11d0, { 0xb6, 0xfe, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b } };
/* DEVPKEY_Device_InstanceId (devpkey.h), defined here: devpkey.h only declares it unless
 * initguid.h precedes it, which would also instantiate every other GUID of the headers. */
static const DEVPROPKEY SeatKeyInstanceId = { { 0x78c34fc8, 0x104a, 0x4aca, { 0x9e, 0xa4, 0x52, 0x4d, 0x52, 0x99, 0x6e, 0x57 } }, 256 };

#ifdef ALLOC_PRAGMA
#pragma alloc_text(INIT, DriverEntry)
#pragma alloc_text(PAGE, SeatEvtDeviceAdd)
#pragma alloc_text(PAGE, SeatEvtDeviceCleanup)
#endif

NTSTATUS
DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    NTSTATUS status;
    WDFKEY key;
    ULONG passThrough = 0;
    ULONG i;
    DECLARE_CONST_UNICODE_STRING(passName, L"PassThrough");

    InitializeListHead(&g.Units);
    KeInitializeSpinLock(&g.RingLock);
    g.CfgGapMs = 1000;
    g.CfgGapPackets = 256;
    /* Built-in unfiltered usage: lamp arrays (spec 50 section 4.1.3 rule 1). */
    g.Unfiltered[0].Page = 0x59;
    g.Unfiltered[0].Usage = 0x01;
    g.UnfilteredCount = 1;
    for (i = 0; i < SEATINPUT_MAX_SEATS; i++) {
        g.SessionOfSeat[i] = (LONG)SEATINPUT_NO_SESSION;
    }

    WDF_DRIVER_CONFIG_INIT(&config, SeatEvtDeviceAdd);
    config.DriverPoolTag = SEATINPUT_POOL_TAG;
    status = WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES, &config, &g.Driver);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = WdfWaitLockCreate(WDF_NO_OBJECT_ATTRIBUTES, &g.UnitLock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Recovery: Safe Mode, or Parameters\PassThrough != 0 (set by the uninstaller or a
     * recovery step), makes the filter pass everything to the console. */
    g.PassThrough = (InitSafeBootMode != NULL && *InitSafeBootMode > 0) ? TRUE : FALSE;
    if (NT_SUCCESS(WdfDriverOpenParametersRegistryKey(g.Driver, KEY_READ, WDF_NO_OBJECT_ATTRIBUTES, &key))) {
        if (NT_SUCCESS(WdfRegistryQueryULong(key, &passName, &passThrough)) && passThrough != 0) {
            g.PassThrough = TRUE;
        }
        WdfRegistryClose(key);
    }
    return STATUS_SUCCESS;
}

static SEAT_ROLE
SeatRoleFromClass(_In_ PCWSTR ClassGuidString)
{
    UNICODE_STRING s;
    GUID guid;

    RtlInitUnicodeString(&s, ClassGuidString);
    if (!NT_SUCCESS(RtlGUIDFromString(&s, &guid))) {
        return SeatRoleNone;
    }
    if (IsEqualGUID(&guid, &SeatClassKeyboard)) {
        return SeatRoleKeyboard;
    }
    if (IsEqualGUID(&guid, &SeatClassMouse)) {
        return SeatRoleMouse;
    }
    if (IsEqualGUID(&guid, &SeatClassHid)) {
        return SeatRoleHid;
    }
    return SeatRoleNone;
}

/* Remote-desktop input bus devices are never filtered (spec 50 section 2.1). */
static BOOLEAN
SeatIsExcluded(_In_ PCWSTR InstanceId)
{
    static const WCHAR prefix[] = L"TERMINPUT_BUS\\";
    return _wcsnicmp(InstanceId, prefix, (sizeof(prefix) / sizeof(WCHAR)) - 1) == 0 ? TRUE : FALSE;
}

ULONG
SeatOwnerSeat(_In_ LONG Mask)
{
    ULONG m = (ULONG)Mask;
    ULONG i;

    for (i = 0; i < SEATINPUT_MAX_SEATS; i++) {
        if (m & (1u << i)) {
            return i + 1;
        }
    }
    return 0;
}

static VOID
SeatQueryIds(_In_ PWDFDEVICE_INIT DeviceInit, _In_ DEVICE_REGISTRY_PROPERTY Property, _Inout_ PUSHORT UsagePage,
             _Inout_ PUSHORT Usage)
{
    WDFMEMORY mem;
    size_t bytes = 0;
    PCWSTR buf;

    if (!NT_SUCCESS(WdfFdoInitAllocAndQueryProperty(DeviceInit, Property, NonPagedPoolNx, WDF_NO_OBJECT_ATTRIBUTES, &mem))) {
        return;
    }
    buf = (PCWSTR)WdfMemoryGetBuffer(mem, &bytes);
    if (buf != NULL && bytes >= sizeof(WCHAR) && bytes <= MAXULONG) {
        SeatHidParseUsage(UsagePage, Usage, buf, (ULONG)bytes);
    }
    WdfObjectDelete(mem);
}

NTSTATUS
SeatEvtDeviceAdd(_In_ WDFDRIVER Driver, _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    NTSTATUS status;
    WDFDEVICE device;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_DEVICE_PROPERTY_DATA prop;
    DEVPROPTYPE type;
    DEVPROPKEY maskKey = { SEATINPUT_DEVPROP_GUID_INIT, SEATINPUT_DEVPROP_MASK_PID };
    WCHAR classGuid[64];
    WCHAR instanceId[SEATINPUT_MAX_INSTANCE_ID];
    ULONG len = 0;
    ULONG mask = 1; /* unknown units belong to the console (spec 50 section 4.1.7) */
    SEAT_ROLE role = SeatRoleNone;
    PUNIT_CONTEXT unit;
    USHORT usagePage = 0;
    USHORT usage = 0;

    UNREFERENCED_PARAMETER(Driver);
    PAGED_CODE();

    WdfFdoInitSetFilter(DeviceInit);

    RtlZeroMemory(classGuid, sizeof(classGuid));
    if (NT_SUCCESS(WdfFdoInitQueryProperty(DeviceInit, DevicePropertyClassGuid, sizeof(classGuid) - sizeof(WCHAR), classGuid, &len))) {
        classGuid[RTL_NUMBER_OF(classGuid) - 1] = L'\0';
        role = SeatRoleFromClass(classGuid);
    }

    RtlZeroMemory(instanceId, sizeof(instanceId));
    WDF_DEVICE_PROPERTY_DATA_INIT(&prop, &SeatKeyInstanceId);
    if (NT_SUCCESS(WdfFdoInitQueryPropertyEx(DeviceInit, &prop, sizeof(instanceId) - sizeof(WCHAR), instanceId, &len, &type))) {
        instanceId[RTL_NUMBER_OF(instanceId) - 1] = L'\0';
        if (SeatIsExcluded(instanceId)) {
            role = SeatRoleExcluded;
        }
    } else {
        RtlZeroMemory(instanceId, sizeof(instanceId));
    }

    WDF_DEVICE_PROPERTY_DATA_INIT(&prop, &maskKey);
    if (!NT_SUCCESS(WdfFdoInitQueryPropertyEx(DeviceInit, &prop, sizeof(mask), &mask, &len, &type)) || type != DEVPROP_TYPE_UINT32) {
        mask = 1;
    }

    if (role == SeatRoleHid) {
        SeatQueryIds(DeviceInit, DevicePropertyCompatibleIDs, &usagePage, &usage);
        if (usagePage == 0) {
            SeatQueryIds(DeviceInit, DevicePropertyHardwareID, &usagePage, &usage);
        }
        WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, SeatHidEvtFileCreate, WDF_NO_EVENT_CALLBACK, WDF_NO_EVENT_CALLBACK);
        /* The function driver (hidclass) owns FsContext / FsContext2. */
        fileConfig.FileObjectClass = WdfFileObjectWdfCannotUseFsContexts;
        fileConfig.AutoForwardCleanupClose = WdfTrue;
        WdfDeviceInitSetFileObjectConfig(DeviceInit, &fileConfig, WDF_NO_OBJECT_ATTRIBUTES);
    }

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, UNIT_CONTEXT);
    attributes.EvtCleanupCallback = SeatEvtDeviceCleanup;
    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    unit = UnitGetContext(device);
    unit->Device = device;
    unit->Role = role;
    unit->Mask = (LONG)mask;
    unit->UsagePage = usagePage;
    unit->Usage = usage;
    unit->UnitId = (ULONG)InterlockedIncrement(&g.NextUnitId);
    RtlCopyMemory(unit->InstanceId, instanceId, sizeof(unit->InstanceId));
    unit->InstanceId[SEATINPUT_MAX_INSTANCE_ID - 1] = L'\0';

    if (role == SeatRoleKeyboard || role == SeatRoleMouse) {
        WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
        queueConfig.EvtIoInternalDeviceControl = SeatEvtInternalIoctl;
        status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, WDF_NO_HANDLE);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    WdfWaitLockAcquire(g.UnitLock, NULL);
    InsertTailList(&g.Units, &unit->Link);
    g.UnitCount++;
    if (g.Control == NULL) {
        /* A missing control device only means the service cannot attach: the filter then
         * stays fail-open (or, after an attach, in gap mode), the PC stays usable at seat 1.
         * Retried on every later device add. */
        status = SeatControlCreate();
        if (!NT_SUCCESS(status)) {
            KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_WARNING_LEVEL,
                       "PulsarSeatInput: control device not created (0x%08X)\n", status));
        }
    } else {
        /* A deletion deferred while the service held its handle is no longer wanted. */
        g.ControlDeletePending = FALSE;
    }
    WdfWaitLockRelease(g.UnitLock);
    return STATUS_SUCCESS;
}

VOID
SeatEvtDeviceCleanup(_In_ WDFOBJECT Object)
{
    PUNIT_CONTEXT unit = UnitGetContext((WDFDEVICE)Object);

    PAGED_CODE();

    WdfWaitLockAcquire(g.UnitLock, NULL);
    if (unit->Link.Flink != NULL) {
        RemoveEntryList(&unit->Link);
        unit->Link.Flink = NULL;
        if (g.UnitCount > 0) {
            g.UnitCount--;
        }
    }
    if (g.UnitCount == 0) {
        /* The driver cannot unload while a control device exists. */
        SeatControlDelete();
    }
    WdfWaitLockRelease(g.UnitLock);
}

VOID
SeatForwardRequest(_In_ WDFREQUEST Request, _In_ WDFIOTARGET Target)
{
    WDF_REQUEST_SEND_OPTIONS options;

    WDF_REQUEST_SEND_OPTIONS_INIT(&options, WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET);
    if (!WdfRequestSend(Request, Target, &options)) {
        WdfRequestComplete(Request, WdfRequestGetStatus(Request));
    }
}
