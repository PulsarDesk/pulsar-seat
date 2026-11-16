/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat source-IP callout - driver entry, unload, control device (contract:
 * include/seatwfp_ioctl.h).
 *
 * - Non-PnP KMDF driver (WdfDriverInitNonPnpDriver), installed by a primitive INF
 *   (class WFPCALLOUTS, DefaultInstall). No device stack is filtered.
 * - Control device \Device\PulsarSeatWfp; DACL = the Pulsar Seat service SID only
 *   (SEATWFP_DEVICE_SDDL), exclusive: one service handle at a time.
 * - Fail open: until the service pushes a map, and again after its handle closes (service
 *   stop / crash), the map is empty and no socket is touched. Session ids are reused by
 *   Windows, so a stale map must not outlive the service that knows the seats.
 * - Recovery: Safe Mode, or Parameters\PassThrough != 0, registers no filter at all.
 *
 * Untested on hardware.
 */
#include "seatwfp.h"
#include <wdmsec.h>

SEATWFP_GLOBALS g;

/* Documented kernel variable: non-zero in Safe Mode (Microsoft Learn, "Determining
 * Whether the Operating System Is Running in Safe Mode"). */
extern PULONG InitSafeBootMode;

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_UNLOAD SeatWfpEvtUnload;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL SeatWfpEvtIoctl;
EVT_WDF_DEVICE_FILE_CREATE SeatWfpEvtFileCreate;
EVT_WDF_FILE_CLEANUP SeatWfpEvtCleanup;

#ifdef ALLOC_PRAGMA
#pragma alloc_text(INIT, DriverEntry)
#pragma alloc_text(PAGE, SeatWfpEvtUnload)
#pragma alloc_text(PAGE, SeatWfpControlCreate)
#pragma alloc_text(PAGE, SeatWfpEvtFileCreate)
#pragma alloc_text(PAGE, SeatWfpEvtCleanup)
#endif

_Use_decl_annotations_
VOID
SeatWfpMapReplace(PUCHAR NewMap, ULONG Bytes)
{
    KIRQL irql;
    PUCHAR old;
    const SEATWFP_MAP_HEADER* h = (const SEATWFP_MAP_HEADER*)NewMap;

    irql = ExAcquireSpinLockExclusive(&g.MapLock);
    old = g.Map;
    g.Map = NewMap;
    g.MapBytes = NewMap != NULL ? Bytes : 0;
    g.MapGeneration = NewMap != NULL ? h->Generation : 0;
    g.MapSeats = NewMap != NULL ? h->SeatCount : 0;
    ExReleaseSpinLockExclusive(&g.MapLock, irql);
    if (old != NULL) {
        ExFreePoolWithTag(old, SEATWFP_POOL_TAG);
    }
}

static ULONG
SeatWfpFlags(VOID)
{
    ULONG flags = SeatWfpEngineFlags();
    if (g.PassThrough) {
        flags |= SEATWFP_VF_PASS_THROUGH;
    }
    if (ReadULongAcquire(&g.MapSeats) != 0) {
        flags |= SEATWFP_VF_MAP;
    }
    return flags;
}

_Use_decl_annotations_
NTSTATUS
DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    NTSTATUS status;
    WDFKEY key;
    ULONG passThrough = 0;
    DECLARE_CONST_UNICODE_STRING(passName, L"PassThrough");

    RtlZeroMemory(&g, sizeof(g));
    KeInitializeEvent(&g.EngineLock, SynchronizationEvent, TRUE);

    WDF_DRIVER_CONFIG_INIT(&config, WDF_NO_EVENT_CALLBACK);
    config.DriverInitFlags |= WdfDriverInitNonPnpDriver;
    config.EvtDriverUnload = SeatWfpEvtUnload;
    config.DriverPoolTag = SEATWFP_POOL_TAG;
    status = WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES, &config, &g.Driver);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    g.PassThrough = (InitSafeBootMode != NULL && *InitSafeBootMode > 0) ? TRUE : FALSE;
    if (NT_SUCCESS(WdfDriverOpenParametersRegistryKey(g.Driver, KEY_READ, WDF_NO_OBJECT_ATTRIBUTES, &key))) {
        if (NT_SUCCESS(WdfRegistryQueryULong(key, &passName, &passThrough)) && passThrough != 0) {
            g.PassThrough = TRUE;
        }
        WdfRegistryClose(key);
    }

    status = SeatWfpControlCreate();
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (g.PassThrough) {
        /* Recovery: the control device answers the version query, nothing else runs. */
        return STATUS_SUCCESS;
    }
    status = SeatWfpCalloutsRegister();
    if (NT_SUCCESS(status)) {
        status = SeatWfpBfeStart();
    }
    if (!NT_SUCCESS(status)) {
        SeatWfpBfeStop();
        SeatWfpCalloutsUnregister();
        WdfObjectDelete(g.Control);
        g.Control = NULL;
        return status;
    }
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
SeatWfpEvtUnload(WDFDRIVER Driver)
{
    UNREFERENCED_PARAMETER(Driver);
    PAGED_CODE();

    /* Filters first (engine session closes), then the callouts, then the device they were
     * registered on. */
    SeatWfpBfeStop();
    SeatWfpCalloutsUnregister();
    SeatWfpMapReplace(NULL, 0);
    if (g.Control != NULL) {
        WdfObjectDelete(g.Control);
        g.Control = NULL;
    }
}

_Use_decl_annotations_
NTSTATUS
SeatWfpControlCreate(VOID)
{
    PWDFDEVICE_INIT init;
    WDFDEVICE device;
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_OBJECT_ATTRIBUTES queueAttributes;
    NTSTATUS status;
    DECLARE_CONST_UNICODE_STRING(name, SEATWFP_DEVICE_NAME);
    DECLARE_CONST_UNICODE_STRING(link, SEATWFP_SYMLINK_NAME);
    DECLARE_CONST_UNICODE_STRING(sddl, SEATWFP_DEVICE_SDDL);

    PAGED_CODE();

    init = WdfControlDeviceInitAllocate(g.Driver, &sddl);
    if (init == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    WdfDeviceInitSetExclusive(init, TRUE);
    WdfDeviceInitSetCharacteristics(init, FILE_DEVICE_SECURE_OPEN, TRUE);
    WdfDeviceInitSetIoType(init, WdfDeviceIoBuffered);
    status = WdfDeviceInitAssignName(init, &name);
    if (!NT_SUCCESS(status)) {
        WdfDeviceInitFree(init);
        return status;
    }
    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, SeatWfpEvtFileCreate, WDF_NO_EVENT_CALLBACK, SeatWfpEvtCleanup);
    WdfDeviceInitSetFileObjectConfig(init, &fileConfig, WDF_NO_OBJECT_ATTRIBUTES);

    status = WdfDeviceCreate(&init, WDF_NO_OBJECT_ATTRIBUTES, &device);
    if (!NT_SUCCESS(status)) {
        WdfDeviceInitFree(init);
        return status;
    }
    status = WdfDeviceCreateSymbolicLink(device, &link);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(device);
        return status;
    }
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchSequential);
    queueConfig.EvtIoDeviceControl = SeatWfpEvtIoctl;
    queueConfig.PowerManaged = WdfFalse;
    WDF_OBJECT_ATTRIBUTES_INIT(&queueAttributes);
    queueAttributes.ExecutionLevel = WdfExecutionLevelPassive;
    status = WdfIoQueueCreate(device, &queueConfig, &queueAttributes, WDF_NO_HANDLE);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(device);
        return status;
    }
    WdfControlFinishInitializing(device);
    g.Control = device;
    g.Wdm = WdfDeviceWdmGetDeviceObject(device);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
SeatWfpEvtFileCreate(WDFDEVICE Device, WDFREQUEST Request, WDFFILEOBJECT FileObject)
{
    UNREFERENCED_PARAMETER(Device);
    UNREFERENCED_PARAMETER(FileObject);
    PAGED_CODE();

    InterlockedIncrement(&g.ControlOpen);
    WdfRequestComplete(Request, STATUS_SUCCESS);
}

_Use_decl_annotations_
VOID
SeatWfpEvtCleanup(WDFFILEOBJECT FileObject)
{
    UNREFERENCED_PARAMETER(FileObject);
    PAGED_CODE();

    /* Service gone: fail open (see the header comment). */
    if (InterlockedDecrement(&g.ControlOpen) <= 0) {
        InterlockedExchange(&g.ControlOpen, 0);
        SeatWfpMapReplace(NULL, 0);
    }
}

static NTSTATUS
SeatWfpSetMap(_In_ WDFREQUEST Request, _In_ size_t InLength)
{
    PVOID in;
    PUCHAR copy;
    NTSTATUS status;
    ULONG bytes;

    if (InLength < sizeof(SEATWFP_MAP_HEADER) || InLength > SEATWFP_MAX_MAP_BYTES) {
        return STATUS_INVALID_PARAMETER;
    }
    status = WdfRequestRetrieveInputBuffer(Request, sizeof(SEATWFP_MAP_HEADER), &in, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    bytes = (ULONG)InLength;
    /* Validate a private copy: the buffered system buffer is ours already, but the copy
     * is what stays in force. */
    copy = (PUCHAR)ExAllocatePoolZero(NonPagedPoolNx, bytes, SEATWFP_POOL_TAG);
    if (copy == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlCopyMemory(copy, in, bytes);
    status = SeatWfpMapValidate(copy, bytes);
    if (!NT_SUCCESS(status)) {
        ExFreePoolWithTag(copy, SEATWFP_POOL_TAG);
        return status;
    }
    if (((const SEATWFP_MAP_HEADER*)copy)->SeatCount == 0) {
        ExFreePoolWithTag(copy, SEATWFP_POOL_TAG);
        copy = NULL;
    }
    SeatWfpMapReplace(copy, bytes);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
SeatWfpEvtIoctl(WDFQUEUE Queue, WDFREQUEST Request, size_t OutputBufferLength, size_t InputBufferLength, ULONG IoControlCode)
{
    NTSTATUS status = STATUS_INVALID_DEVICE_REQUEST;
    ULONG_PTR info = 0;
    PVOID out;

    UNREFERENCED_PARAMETER(Queue);
    UNREFERENCED_PARAMETER(OutputBufferLength);

    switch (IoControlCode) {
    case IOCTL_SEATWFP_GET_VERSION: {
        SEATWFP_VERSION* v;
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(SEATWFP_VERSION), &out, NULL);
        if (NT_SUCCESS(status)) {
            v = (SEATWFP_VERSION*)out;
            RtlZeroMemory(v, sizeof(*v));
            v->InterfaceMajor = SEATWFP_INTERFACE_MAJOR;
            v->InterfaceMinor = SEATWFP_INTERFACE_MINOR;
            v->Build = SEATWFP_BUILD;
            v->Flags = SeatWfpFlags();
            info = sizeof(*v);
        }
        break;
    }
    case IOCTL_SEATWFP_SET_MAP:
        status = g.PassThrough ? STATUS_DEVICE_NOT_READY : SeatWfpSetMap(Request, InputBufferLength);
        break;
    case IOCTL_SEATWFP_CLEAR:
        SeatWfpMapReplace(NULL, 0);
        status = STATUS_SUCCESS;
        break;
    case IOCTL_SEATWFP_GET_STATS: {
        SEATWFP_STATS* s;
        KIRQL irql;
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(SEATWFP_STATS), &out, NULL);
        if (NT_SUCCESS(status)) {
            s = (SEATWFP_STATS*)out;
            RtlZeroMemory(s, sizeof(*s));
            s->BindRedirects = (ULONGLONG)InterlockedCompareExchange64(&g.BindRedirects, 0, 0);
            s->ConnectRedirects = (ULONGLONG)InterlockedCompareExchange64(&g.ConnectRedirects, 0, 0);
            s->Skipped = (ULONGLONG)InterlockedCompareExchange64(&g.Skipped, 0, 0);
            irql = ExAcquireSpinLockShared(&g.MapLock);
            s->Generation = g.MapGeneration;
            s->SeatCount = g.MapSeats;
            ExReleaseSpinLockShared(&g.MapLock, irql);
            s->Flags = SeatWfpFlags();
            info = sizeof(*s);
        }
        break;
    }
    default:
        break;
    }
    WdfRequestCompleteWithInformation(Request, status, info);
}
