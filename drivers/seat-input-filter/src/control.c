/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat input filter - control device and packet ring (spec 50 section 4.1.2,
 * 4.1.5; spec 00 section 5.5.2). Contract: include/seatinput_ioctl.h.
 *
 * - Non-PnP control device \Device\PulsarSeatInput, SDDL_DEVOBJ_SYS_ALL (SYSTEM only),
 *   exclusive: one service handle at a time.
 * - The first successful IOCTL_SEATINPUT_ATTACH since boot ends fail-open mode. When the
 *   handle closes (service stop / crash) the filter stays in force: extra seats' packets are
 *   kept in a bounded buffer PER UNIT (CfgGapPackets each, spec 50 section 4.1.5 / 4.3;
 *   further packets of that unit are dropped, so one busy mouse cannot push out another
 *   seat's keys) and aged out after CfgGapMs, never passed to the console. The next
 *   delivered packet of a unit with dropped packets carries SEATINPUT_PF_GAP so the service
 *   synthesizes key-ups. Per-unit counts are kept modulo SEATINPUT_GAP_BITS, like the gap
 *   flags: two units that collide share one budget (harmless, only stricter).
 * - The control device lives while a unit or an open service handle exists: when the last
 *   unit goes away while the service still holds its handle, deletion is deferred (the name
 *   would otherwise stay taken until that handle closes and a new unit could not recreate
 *   it). A later unit then reuses the same control device.
 * - IOCTL_SEATINPUT_READ_PACKETS is an inverted call: pended in a manual queue and
 *   completed from the service callbacks (DISPATCH_LEVEL).
 *
 * Untested on hardware.
 */
#include "seatinput.h"
#include <wdmsec.h>

EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL SeatEvtControlIoctl;
EVT_WDF_DEVICE_FILE_CREATE SeatEvtControlFileCreate;
EVT_WDF_FILE_CLEANUP SeatEvtControlCleanup;

#ifdef ALLOC_PRAGMA
#pragma alloc_text(PAGE, SeatControlCreate)
#pragma alloc_text(PAGE, SeatControlDelete)
#pragma alloc_text(PAGE, SeatEvtControlCleanup)
#pragma alloc_text(PAGE, SeatEvtControlFileCreate)
#endif

/* ---- ring (any IRQL <= DISPATCH_LEVEL) ---- */

static VOID
SeatGapSet(_In_ ULONG UnitId)
{
    ULONG bit = UnitId % SEATINPUT_GAP_BITS;
    InterlockedOr((volatile LONG*)&g.GapBits[bit / 32], (LONG)(1u << (bit % 32)));
}

/* Caller holds RingLock. */
static VOID
SeatQueuedAdd(_In_ ULONG UnitId, _In_ LONG Delta)
{
    USHORT* c = &g.UnitQueued[UnitId % SEATINPUT_GAP_BITS];
    if (Delta > 0) {
        if (*c != 0xFFFF) {
            (*c)++;
        }
    } else if (*c != 0) {
        (*c)--;
    }
}

static BOOLEAN
SeatGapTake(_In_ ULONG UnitId)
{
    ULONG bit = UnitId % SEATINPUT_GAP_BITS;
    LONG mask = (LONG)(1u << (bit % 32));
    LONG old = InterlockedAnd((volatile LONG*)&g.GapBits[bit / 32], ~mask);
    return (old & mask) != 0 ? TRUE : FALSE;
}

VOID
SeatRingPush(_In_ const SEATINPUT_PACKET* Packet)
{
    KIRQL irql;
    ULONG unitCap = SEATINPUT_RING_SIZE;
    ULONG gapPackets;
    SEATINPUT_PACKET* slot;
    SEATINPUT_PACKET* old;

    if (InterlockedCompareExchange(&g.Attached, 0, 0) == 0) {
        gapPackets = (ULONG)g.CfgGapPackets;
        unitCap = gapPackets == 0 ? 1 : (gapPackets < SEATINPUT_RING_SIZE ? gapPackets : SEATINPUT_RING_SIZE);
    }
    KeAcquireSpinLock(&g.RingLock, &irql);
    if (g.UnitQueued[Packet->UnitId % SEATINPUT_GAP_BITS] >= unitCap) {
        /* Service gap and this unit's budget is used up: drop the new packet. */
        if (Packet->Kind != SEATINPUT_PK_HOTKEY) {
            SeatGapSet(Packet->UnitId);
        }
        KeReleaseSpinLock(&g.RingLock, irql);
        return;
    }
    while (g.RingCount >= SEATINPUT_RING_SIZE) {
        old = &g.Ring[g.RingHead];
        if (old->Kind != SEATINPUT_PK_HOTKEY) {
            SeatGapSet(old->UnitId);
        }
        SeatQueuedAdd(old->UnitId, -1);
        g.RingHead = (g.RingHead + 1) % SEATINPUT_RING_SIZE;
        g.RingCount--;
    }
    slot = &g.Ring[(g.RingHead + g.RingCount) % SEATINPUT_RING_SIZE];
    *slot = *Packet;
    slot->Time = KeQueryInterruptTime();
    g.RingCount++;
    SeatQueuedAdd(Packet->UnitId, 1);
    KeReleaseSpinLock(&g.RingLock, irql);
}

/* Caller holds RingLock. */
static ULONG
SeatRingPop(_Out_writes_to_(Max, return) SEATINPUT_PACKET* Out, _In_ ULONG Max)
{
    ULONGLONG now = KeQueryInterruptTime();
    ULONGLONG maxAge = (ULONGLONG)(ULONG)g.CfgGapMs * 10000ull;
    ULONG n = 0;
    SEATINPUT_PACKET* p;

    while (g.RingCount != 0 && n < Max) {
        p = &g.Ring[g.RingHead];
        g.RingHead = (g.RingHead + 1) % SEATINPUT_RING_SIZE;
        g.RingCount--;
        SeatQueuedAdd(p->UnitId, -1);
        if (maxAge != 0 && now - p->Time > maxAge) {
            /* Hotkey reports age out too (a stale one must not move a unit later); they
             * carry no key state, so they need no GAP. */
            if (p->Kind != SEATINPUT_PK_HOTKEY) {
                SeatGapSet(p->UnitId);
            }
            continue;
        }
        Out[n] = *p;
        if (SeatGapTake(p->UnitId)) {
            Out[n].Flags |= SEATINPUT_PF_GAP;
        }
        n++;
    }
    return n;
}

/* Completes Request with packets; FALSE (request untouched) when the ring is empty. */
static BOOLEAN
SeatCompleteRead(_In_ WDFREQUEST Request)
{
    SEATINPUT_PACKET* buffer;
    size_t length = 0;
    NTSTATUS status;
    KIRQL irql;
    ULONG n;

    status = WdfRequestRetrieveOutputBuffer(Request, sizeof(SEATINPUT_PACKET), (PVOID*)&buffer, &length);
    if (!NT_SUCCESS(status)) {
        WdfRequestComplete(Request, status);
        return TRUE;
    }
    KeAcquireSpinLock(&g.RingLock, &irql);
    n = SeatRingPop(buffer, (ULONG)(length / sizeof(SEATINPUT_PACKET)));
    KeReleaseSpinLock(&g.RingLock, irql);
    if (n == 0) {
        return FALSE;
    }
    WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, (ULONG_PTR)n * sizeof(SEATINPUT_PACKET));
    return TRUE;
}

VOID
SeatDrainPending(VOID)
{
    WDFQUEUE queue = g.PendingReads;
    WDFREQUEST request;

    if (queue == NULL) {
        return;
    }
    for (;;) {
        if (!NT_SUCCESS(WdfIoQueueRetrieveNextRequest(queue, &request))) {
            return;
        }
        if (!SeatCompleteRead(request)) {
            /* Another drainer emptied the ring first: back to the head of the queue. */
            if (!NT_SUCCESS(WdfRequestRequeue(request))) {
                WdfRequestComplete(request, STATUS_CANCELLED);
            }
            return;
        }
    }
}

/* ---- control device (PASSIVE_LEVEL) ---- */

NTSTATUS
SeatControlCreate(VOID)
{
    PWDFDEVICE_INIT init;
    WDFDEVICE device;
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_OBJECT_ATTRIBUTES queueAttributes;
    NTSTATUS status;
    DECLARE_CONST_UNICODE_STRING(name, SEATINPUT_DEVICE_NAME);
    DECLARE_CONST_UNICODE_STRING(link, SEATINPUT_SYMLINK_NAME);

    PAGED_CODE();

    init = WdfControlDeviceInitAllocate(g.Driver, &SDDL_DEVOBJ_SYS_ALL);
    if (init == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    WdfDeviceInitSetExclusive(init, TRUE);
    /* The DACL also guards opens of names below the device (\\.\PulsarSeatInput\x). */
    WdfDeviceInitSetCharacteristics(init, FILE_DEVICE_SECURE_OPEN, TRUE);
    WdfDeviceInitSetIoType(init, WdfDeviceIoBuffered);
    status = WdfDeviceInitAssignName(init, &name);
    if (!NT_SUCCESS(status)) {
        WdfDeviceInitFree(init);
        return status;
    }
    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, SeatEvtControlFileCreate, WDF_NO_EVENT_CALLBACK, SeatEvtControlCleanup);
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

    /* Control devices have no power management: queues must not be power managed. */
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoDeviceControl = SeatEvtControlIoctl;
    queueConfig.PowerManaged = WdfFalse;
    WDF_OBJECT_ATTRIBUTES_INIT(&queueAttributes);
    queueAttributes.ExecutionLevel = WdfExecutionLevelPassive;
    status = WdfIoQueueCreate(device, &queueConfig, &queueAttributes, WDF_NO_HANDLE);
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(device);
        return status;
    }
    WDF_IO_QUEUE_CONFIG_INIT(&queueConfig, WdfIoQueueDispatchManual);
    queueConfig.PowerManaged = WdfFalse;
    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &g.PendingReads);
    if (!NT_SUCCESS(status)) {
        g.PendingReads = NULL;
        WdfObjectDelete(device);
        return status;
    }
    WdfControlFinishInitializing(device);
    g.Control = device;
    return STATUS_SUCCESS;
}

/* Caller holds UnitLock and no filter device remains. With a service handle still open the
 * deletion is deferred (ControlDeletePending): the next unit reuses the device; see the
 * header comment. */
VOID
SeatControlDelete(VOID)
{
    WDFDEVICE device = g.Control;

    PAGED_CODE();

    if (device == NULL) {
        return;
    }
    if (InterlockedCompareExchange(&g.ControlOpen, 0, 0) != 0) {
        g.ControlDeletePending = TRUE;
        return;
    }
    g.ControlDeletePending = FALSE;
    g.PendingReads = NULL;
    g.Control = NULL;
    InterlockedExchange(&g.Attached, 0);
    WdfObjectDelete(device);
}

VOID
SeatEvtControlFileCreate(_In_ WDFDEVICE Device, _In_ WDFREQUEST Request, _In_ WDFFILEOBJECT FileObject)
{
    UNREFERENCED_PARAMETER(Device);
    UNREFERENCED_PARAMETER(FileObject);
    PAGED_CODE();

    /* Exclusive device: at most one open; counted so deletion waits for its cleanup. */
    InterlockedIncrement(&g.ControlOpen);
    WdfRequestComplete(Request, STATUS_SUCCESS);
}

VOID
SeatEvtControlCleanup(_In_ WDFFILEOBJECT FileObject)
{
    WDFREQUEST request;
    WDFQUEUE queue;

    PAGED_CODE();

    /* Service gone: keep filtering (gap mode), finish its pended reads. */
    InterlockedExchange(&g.Attached, 0);
    /* Under UnitLock, and the pended reads are drained BEFORE ControlOpen drops: while
     * ControlOpen is still non-zero, SeatControlDelete (last unit's cleanup, also under
     * UnitLock) only defers, so the queue cannot be deleted under this loop. */
    WdfWaitLockAcquire(g.UnitLock, NULL);
    queue = g.PendingReads;
    if (queue != NULL) {
        while (NT_SUCCESS(WdfIoQueueRetrieveRequestByFileObject(queue, FileObject, &request))) {
            WdfRequestComplete(request, STATUS_CANCELLED);
        }
    }
    if (InterlockedDecrement(&g.ControlOpen) < 0) {
        InterlockedExchange(&g.ControlOpen, 0);
    }
    WdfWaitLockRelease(g.UnitLock);
    /* A deferred deletion (no unit left) is not done here: deleting the control device from
     * its own file callback is not safe. The device stays until the next unit arrives
     * (reused) or leaves (deleted then); it only keeps the driver image loaded. */
}

static NTSTATUS
SeatSetUnit(_In_ const SEATINPUT_UNIT_MASK* In)
{
    WCHAR id[SEATINPUT_MAX_INSTANCE_ID];
    UNICODE_STRING want;
    UNICODE_STRING have;
    PLIST_ENTRY e;
    PUNIT_CONTEXT unit;
    NTSTATUS status = STATUS_NOT_FOUND;

    RtlCopyMemory(id, In->InstanceId, sizeof(id));
    id[SEATINPUT_MAX_INSTANCE_ID - 1] = L'\0';
    RtlInitUnicodeString(&want, id);

    WdfWaitLockAcquire(g.UnitLock, NULL);
    for (e = g.Units.Flink; e != &g.Units; e = e->Flink) {
        unit = CONTAINING_RECORD(e, UNIT_CONTEXT, Link);
        RtlInitUnicodeString(&have, unit->InstanceId);
        if (RtlEqualUnicodeString(&want, &have, TRUE)) {
            InterlockedExchange(&unit->Mask, (LONG)In->Mask);
            unit->Revision = In->Revision;
            status = STATUS_SUCCESS;
            /* Several devnodes never share an instance id; stop at the first. */
            break;
        }
    }
    WdfWaitLockRelease(g.UnitLock);
    return status;
}

/* Full = the caller sent a minor-1 config (EmergencyScan present). */
static NTSTATUS
SeatSetConfig(_In_ const SEATINPUT_CONFIG* In, _In_ BOOLEAN Full)
{
    ULONG i;
    ULONG count = In->UnfilteredCount;
    LONG emScan;
    LONG emFlags;

    if (count > SEATINPUT_MAX_UNFILTERED) {
        return STATUS_INVALID_PARAMETER;
    }
    if (Full) {
        emScan = (LONG)In->EmergencyScan;
        emFlags = (LONG)In->EmergencyFlags;
    } else {
        /* Minor 0: the emergency combination was Ctrl+Shift + the move hotkey. */
        emScan = (In->Flags & SEATINPUT_CF_HOTKEY_ENABLED) ? (LONG)In->HotkeyScan : 0;
        emFlags = (LONG)In->HotkeyFlags;
    }
    InterlockedExchange(&g.CfgFlags, (LONG)In->Flags);
    InterlockedExchange(&g.CfgHotkeyScan, (LONG)In->HotkeyScan);
    InterlockedExchange(&g.CfgHotkeyFlags, (LONG)In->HotkeyFlags);
    InterlockedExchange(&g.CfgEmergencyScan, emScan);
    InterlockedExchange(&g.CfgEmergencyFlags, emFlags);
    InterlockedExchange(&g.CfgGapMs, (LONG)In->GapMs);
    InterlockedExchange(&g.CfgGapPackets, (LONG)In->GapPackets);
    WdfWaitLockAcquire(g.UnitLock, NULL);
    for (i = 0; i < count; i++) {
        g.Unfiltered[i] = In->Unfiltered[i];
    }
    g.UnfilteredCount = count;
    WdfWaitLockRelease(g.UnitLock);
    return STATUS_SUCCESS;
}

static NTSTATUS
SeatListUnits(_Out_writes_bytes_(Length) PUCHAR Out, _In_ size_t Length, _Out_ size_t* Written)
{
    SEATINPUT_UNIT_LIST* header = (SEATINPUT_UNIT_LIST*)Out;
    SEATINPUT_UNIT_INFO* info = (SEATINPUT_UNIT_INFO*)(Out + sizeof(SEATINPUT_UNIT_LIST));
    size_t room = (Length - sizeof(SEATINPUT_UNIT_LIST)) / sizeof(SEATINPUT_UNIT_INFO);
    ULONG count = 0;
    ULONG total = 0;
    PLIST_ENTRY e;
    PUNIT_CONTEXT unit;

    WdfWaitLockAcquire(g.UnitLock, NULL);
    for (e = g.Units.Flink; e != &g.Units; e = e->Flink) {
        unit = CONTAINING_RECORD(e, UNIT_CONTEXT, Link);
        total++;
        if (count < room) {
            RtlZeroMemory(&info[count], sizeof(SEATINPUT_UNIT_INFO));
            info[count].UnitId = unit->UnitId;
            info[count].Role = (ULONG)unit->Role;
            info[count].Mask = (ULONG)InterlockedCompareExchange(&unit->Mask, 0, 0);
            info[count].Packets = (ULONGLONG)InterlockedCompareExchange64(&unit->Packets, 0, 0);
            info[count].LastActivity = (ULONGLONG)InterlockedCompareExchange64(&unit->LastActivity, 0, 0);
            RtlCopyMemory(info[count].InstanceId, unit->InstanceId, sizeof(info[count].InstanceId));
            count++;
        }
    }
    WdfWaitLockRelease(g.UnitLock);
    *Written = sizeof(SEATINPUT_UNIT_LIST) + (size_t)count * sizeof(SEATINPUT_UNIT_INFO);
    header->Size = (ULONG)*Written;
    header->Count = count;
    header->Total = total;
    header->Reserved = 0;
    return STATUS_SUCCESS;
}

VOID
SeatEvtControlIoctl(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t OutputBufferLength,
                    _In_ size_t InputBufferLength, _In_ ULONG IoControlCode)
{
    NTSTATUS status = STATUS_SUCCESS;
    size_t info = 0;
    size_t length = 0;
    PVOID in = NULL;
    PVOID out = NULL;
    ULONG i;

    UNREFERENCED_PARAMETER(Queue);
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case IOCTL_SEATINPUT_GET_VERSION: {
        SEATINPUT_VERSION* v;
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(SEATINPUT_VERSION), &out, &length);
        if (NT_SUCCESS(status) && length < sizeof(SEATINPUT_VERSION)) {
            status = STATUS_BUFFER_TOO_SMALL; /* not reached: the framework checked the minimum */
        }
        if (NT_SUCCESS(status)) {
            v = (SEATINPUT_VERSION*)out;
            RtlZeroMemory(v, sizeof(*v));
            v->InterfaceMajor = SEATINPUT_INTERFACE_MAJOR;
            v->InterfaceMinor = SEATINPUT_INTERFACE_MINOR;
            v->Build = SEATINPUT_BUILD;
            v->Flags = (g.PassThrough ? SEATINPUT_VF_PASS_THROUGH : 0)
                | (InterlockedCompareExchange(&g.AttachedOnce, 0, 0) ? SEATINPUT_VF_ATTACHED_ONCE : 0)
                | (InterlockedCompareExchange(&g.Attached, 0, 0) ? SEATINPUT_VF_ATTACHED : 0);
            info = sizeof(*v);
        }
        break;
    }
    case IOCTL_SEATINPUT_ATTACH: {
        const SEATINPUT_ATTACH* a;
        status = WdfRequestRetrieveInputBuffer(Request, sizeof(SEATINPUT_ATTACH), &in, &length);
        if (NT_SUCCESS(status)) {
            a = (const SEATINPUT_ATTACH*)in;
            if (a->Size < sizeof(SEATINPUT_ATTACH) || a->Major != SEATINPUT_INTERFACE_MAJOR) {
                /* Wrong major: stay fail-open (spec 00 section 5.5.2). */
                status = STATUS_REVISION_MISMATCH;
            } else {
                InterlockedExchange(&g.Attached, 1);
                InterlockedExchange(&g.AttachedOnce, 1);
            }
        }
        break;
    }
    case IOCTL_SEATINPUT_SET_CONFIG:
        /* Minor-0 callers send SEATINPUT_CONFIG_V0_SIZE bytes; the trailing emergency
         * fields are read only when both the buffer and Size cover them. */
        status = WdfRequestRetrieveInputBuffer(Request, SEATINPUT_CONFIG_V0_SIZE, &in, &length);
        if (NT_SUCCESS(status)) {
            ULONG size = ((const SEATINPUT_CONFIG*)in)->Size;
            status = (size < SEATINPUT_CONFIG_V0_SIZE || size > length)
                ? STATUS_INVALID_PARAMETER
                : SeatSetConfig((const SEATINPUT_CONFIG*)in,
                                (BOOLEAN)(size >= sizeof(SEATINPUT_CONFIG) && length >= sizeof(SEATINPUT_CONFIG)));
        }
        break;

    case IOCTL_SEATINPUT_SET_UNIT:
        status = WdfRequestRetrieveInputBuffer(Request, sizeof(SEATINPUT_UNIT_MASK), &in, &length);
        if (NT_SUCCESS(status)) {
            status = ((const SEATINPUT_UNIT_MASK*)in)->Size < sizeof(SEATINPUT_UNIT_MASK)
                ? STATUS_INVALID_PARAMETER
                : SeatSetUnit((const SEATINPUT_UNIT_MASK*)in);
        }
        break;

    case IOCTL_SEATINPUT_SET_SESSIONS: {
        const SEATINPUT_SESSIONS* s;
        status = WdfRequestRetrieveInputBuffer(Request, sizeof(SEATINPUT_SESSIONS), &in, &length);
        if (NT_SUCCESS(status)) {
            s = (const SEATINPUT_SESSIONS*)in;
            if (s->Size < sizeof(SEATINPUT_SESSIONS)) {
                status = STATUS_INVALID_PARAMETER;
            } else {
                for (i = 0; i < SEATINPUT_MAX_SEATS; i++) {
                    InterlockedExchange(&g.SessionOfSeat[i], (LONG)(i < s->Count ? s->SessionOfSeat[i] : SEATINPUT_NO_SESSION));
                }
            }
        }
        break;
    }
    case IOCTL_SEATINPUT_READ_PACKETS:
        if (SeatCompleteRead(Request)) {
            return;
        }
        status = WdfRequestForwardToIoQueue(Request, g.PendingReads);
        if (!NT_SUCCESS(status)) {
            break;
        }
        /* A packet may have arrived between the empty check and the forward. */
        SeatDrainPending();
        return;

    case IOCTL_SEATINPUT_LIST_UNITS:
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(SEATINPUT_UNIT_LIST), &out, &length);
        if (NT_SUCCESS(status)) {
            status = SeatListUnits((PUCHAR)out, length, &info);
        }
        break;

    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    }
    WdfRequestCompleteWithInformation(Request, status, info);
}
