/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat input filter - internal declarations.
 *
 * One KMDF driver, three roles chosen per devnode from its setup class:
 *   - Keyboard {4D36E96B-E325-11CE-BFC1-08002BE10318}: device upper filter BELOW kbdclass
 *     (above kbdhid / i8042prt). Hooks the class service callback.
 *   - Mouse    {4D36E96F-E325-11CE-BFC1-08002BE10318}: same below mouclass.
 *   - HIDClass {745A17A0-74D3-11D0-B6FE-00A0C91EFB8B}: upper filter on HID collection
 *     devnodes; decides on IRP_MJ_CREATE by requestor session, never touches data.
 *
 * Written fresh from the public WDK documentation (Microsoft Learn). The MS-PL WDK samples
 * were not copied (spec 00 section 3.9 item 5).
 */
#pragma once

/* ntifs.h (includes ntddk.h): IoGetRequestorProcess, IoGetRequestorSessionId. */
#include <ntifs.h>
#include <wdf.h>
#include <ntddkbd.h>
#include <ntddmou.h>
#include <kbdmou.h>
#include <devpkey.h>

#include "seatinput_ioctl.h"

#define SEATINPUT_POOL_TAG 'iSsP'
#define SEATINPUT_BUILD    1

/* Ring of packets for the service (inverted call). */
#define SEATINPUT_RING_SIZE 4096
/* Gap flags are kept per unit id modulo this. */
#define SEATINPUT_GAP_BITS  1024

typedef enum _SEAT_ROLE {
    SeatRoleNone = 0,
    SeatRoleKeyboard = SEATINPUT_ROLE_KEYBOARD,
    SeatRoleMouse = SEATINPUT_ROLE_MOUSE,
    SeatRoleHid = SEATINPUT_ROLE_HID,
    SeatRoleExcluded = SEATINPUT_ROLE_EXCLUDED,
} SEAT_ROLE;

typedef VOID (*SEAT_KBD_CLASS_SERVICE)(PDEVICE_OBJECT, PKEYBOARD_INPUT_DATA, PKEYBOARD_INPUT_DATA, PULONG);
typedef VOID (*SEAT_MOU_CLASS_SERVICE)(PDEVICE_OBJECT, PMOUSE_INPUT_DATA, PMOUSE_INPUT_DATA, PULONG);

/* Per filter device object (one input unit). Non-paged (framework context). */
typedef struct _UNIT_CONTEXT {
    LIST_ENTRY    Link;               /* g.Units, under g.UnitLock */
    WDFDEVICE     Device;
    ULONG         UnitId;
    SEAT_ROLE     Role;
    volatile LONG Mask;               /* seat mask, read at DISPATCH_LEVEL */
    ULONGLONG     Revision;
    CONNECT_DATA  Upper;              /* kbdclass / mouclass connect data */
    BOOLEAN       Connected;
    BOOLEAN       CtrlDown;           /* seat-1 hotkey detection */
    BOOLEAN       HotkeySwallowed;
    UCHAR         ShiftDown;          /* bit 0 left, bit 1 right Shift (seat-1 hotkey) */
    USHORT        SwallowedScan;      /* make code whose make was swallowed (hotkey / emergency) */
    USHORT        UsagePage;          /* HID role: collection usage */
    USHORT        Usage;
    volatile LONG64 Packets;
    volatile LONG64 LastActivity;
    ULONGLONG     LastCopy;           /* last seat-1 activity copy (interrupt time) */
    WCHAR         InstanceId[SEATINPUT_MAX_INSTANCE_ID];
} UNIT_CONTEXT, *PUNIT_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(UNIT_CONTEXT, UnitGetContext)

/* Driver-wide state (non-paged .data / .bss). */
typedef struct _SEATINPUT_GLOBALS {
    WDFDRIVER     Driver;
    WDFWAITLOCK   UnitLock;           /* PASSIVE_LEVEL: unit list, control device life */
    LIST_ENTRY    Units;
    ULONG         UnitCount;
    volatile LONG NextUnitId;

    WDFDEVICE     Control;
    WDFQUEUE      PendingReads;       /* manual queue of IOCTL_SEATINPUT_READ_PACKETS */
    volatile LONG ControlOpen;        /* open service handles on Control (0 or 1) */
    BOOLEAN       ControlDeletePending; /* no unit left: delete once no handle is open */

    BOOLEAN       PassThrough;        /* Safe Mode or recovery value: never filter */
    volatile LONG AttachedOnce;       /* fail-open ends at the first attach since boot */
    volatile LONG Attached;           /* a service handle is open now */

    /* Config, each field updated atomically (read at DISPATCH_LEVEL). */
    volatile LONG CfgFlags;
    volatile LONG CfgHotkeyScan;
    volatile LONG CfgHotkeyFlags;
    volatile LONG CfgEmergencyScan;   /* 0 = none */
    volatile LONG CfgEmergencyFlags;

    volatile LONG CfgGapMs;
    volatile LONG CfgGapPackets;
    SEATINPUT_USAGE Unfiltered[SEATINPUT_MAX_UNFILTERED]; /* under UnitLock */
    ULONG         UnfilteredCount;
    volatile LONG SessionOfSeat[SEATINPUT_MAX_SEATS];

    KSPIN_LOCK    RingLock;
    SEATINPUT_PACKET Ring[SEATINPUT_RING_SIZE];
    ULONG         RingHead;           /* oldest */
    ULONG         RingCount;
    ULONG         GapBits[SEATINPUT_GAP_BITS / 32];
    USHORT        UnitQueued[SEATINPUT_GAP_BITS]; /* packets in Ring per unit id (mod), RingLock */
} SEATINPUT_GLOBALS;

extern SEATINPUT_GLOBALS g;

/* driver.c */
DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD SeatEvtDeviceAdd;
EVT_WDF_OBJECT_CONTEXT_CLEANUP SeatEvtDeviceCleanup;
VOID SeatForwardRequest(_In_ WDFREQUEST Request, _In_ WDFIOTARGET Target);
ULONG SeatOwnerSeat(_In_ LONG Mask);

/* control.c */
NTSTATUS SeatControlCreate(VOID);
VOID SeatControlDelete(VOID);
VOID SeatRingPush(_In_ const SEATINPUT_PACKET* Packet);
VOID SeatDrainPending(VOID);

/* kbdmou.c */
EVT_WDF_IO_QUEUE_IO_INTERNAL_DEVICE_CONTROL SeatEvtInternalIoctl;
VOID SeatKbdServiceCallback(PDEVICE_OBJECT DeviceObject, PKEYBOARD_INPUT_DATA Start, PKEYBOARD_INPUT_DATA End, PULONG Consumed);
VOID SeatMouServiceCallback(PDEVICE_OBJECT DeviceObject, PMOUSE_INPUT_DATA Start, PMOUSE_INPUT_DATA End, PULONG Consumed);

/* hid.c */
EVT_WDF_DEVICE_FILE_CREATE SeatHidEvtFileCreate;
VOID SeatHidParseUsage(_Inout_ PUSHORT UsagePage, _Inout_ PUSHORT Usage, _In_reads_bytes_(Bytes) PCWSTR MultiSz, _In_ ULONG Bytes);
