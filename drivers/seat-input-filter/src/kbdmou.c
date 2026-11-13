/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat input filter - keyboard / mouse role (spec 50 section 4.1.2, 4.1.5, 4.1.9).
 *
 * Position: device upper filter below kbdclass / mouclass. On the internal connect request
 * we keep the class driver's CONNECT_DATA and put our own service callback in its place, so
 * every input packet of the unit passes through SeatKbdServiceCallback /
 * SeatMouServiceCallback (DISPATCH_LEVEL) before it reaches the class driver:
 *
 *   before the first service attach since boot, Safe Mode, recovery value: pass everything;
 *   unit on seat 1 or shared: pass (and copy for the wizard / activity);
 *   unit on exactly one other seat: never to the console; queued for the service (inverted
 *     call), also while the service is gone (bounded, aged: dropped rather than leaked);
 *   spare unit: dropped, or captured for the service while the wizard runs;
 *   a mask of several seats without seat 1 (not valid for keyboards and mice, which are
 *     exclusive): treated as spare, the same rule as the service's route model
 *     (crates/seat-input/src/routes.rs UnitRoute::Shared: delivered to nobody). A mask of
 *     several seats that includes seat 1 passes to the console like a seat-1 mask.
 *
 * Untested on hardware.
 */
#include "seatinput.h"

#define SEAT_SC_CTRL   0x1D
#define SEAT_SC_LSHIFT 0x2A
#define SEAT_SC_RSHIFT 0x36
/* Copies of seat-1 packets for activity outside the wizard: at most one per this many 100 ns. */
#define SEAT_ACTIVITY_COPY_100NS (250ull * 10000ull)

typedef enum _SEAT_ROUTE {
    SeatRoutePass,
    SeatRouteSeat,
    SeatRouteDrop,
    SeatRouteWizard,
} SEAT_ROUTE;

static BOOLEAN
SeatAttachedOnce(VOID)
{
    return InterlockedCompareExchange(&g.AttachedOnce, 0, 0) != 0 ? TRUE : FALSE;
}

static SEAT_ROUTE
SeatRoute(_In_ PUNIT_CONTEXT Unit, _Out_ PULONG Seat)
{
    LONG mask;

    *Seat = 0;
    if (g.PassThrough || Unit->Role == SeatRoleExcluded || !SeatAttachedOnce()) {
        return SeatRoutePass;
    }
    mask = InterlockedCompareExchange(&Unit->Mask, 0, 0);
    if (mask == (LONG)0xFFFFFFFF || (mask & 1) != 0) {
        return SeatRoutePass;
    }
    /* Spare, or several seats without seat 1 (see the header comment). */
    if (mask == 0 || (((ULONG)mask & ((ULONG)mask - 1u)) != 0)) {
        return (g.CfgFlags & SEATINPUT_CF_WIZARD_CAPTURE) ? SeatRouteWizard : SeatRouteDrop;
    }
    *Seat = SeatOwnerSeat(mask);
    return SeatRouteSeat;
}

static VOID
SeatTouch(_Inout_ PUNIT_CONTEXT Unit, _In_ ULONG Count)
{
    InterlockedAdd64(&Unit->Packets, (LONG64)Count);
    InterlockedExchange64(&Unit->LastActivity, (LONG64)KeQueryInterruptTime());
}

static VOID
SeatPushKey(_In_ PUNIT_CONTEXT Unit, _In_ UCHAR Seat, _In_ USHORT Flags, _In_ const KEYBOARD_INPUT_DATA* Data)
{
    SEATINPUT_PACKET p;

    RtlZeroMemory(&p, sizeof(p));
    p.UnitId = Unit->UnitId;
    p.Kind = SEATINPUT_PK_KEYBOARD;
    p.Seat = Seat;
    p.Flags = Flags;
    /* A console copy outside the wizard only reports that (and when) the unit was used:
     * no make code or flags, so no sample of console keystrokes (passwords included) ever
     * reaches the ring, the service's IPC or its logs. The service ignores A / B of such
     * copies (crates/seat-input/src/kmdf_router.rs). */
    if ((Flags & SEATINPUT_PF_CONSOLE_COPY) == 0 || (g.CfgFlags & SEATINPUT_CF_WIZARD_CAPTURE) != 0) {
        p.A = Data->MakeCode;
        p.B = Data->Flags;
    }
    SeatRingPush(&p);
}

/* Hand [From, To) to kbdclass; returns how many it took (never more than it was given).
 * A full class queue takes fewer: the port driver keeps the rest for its next callback. */
static ULONG
SeatKbdPassRun(_In_ PUNIT_CONTEXT Unit, _In_ SEAT_KBD_CLASS_SERVICE Upper, _In_ PKEYBOARD_INPUT_DATA From,
               _In_ PKEYBOARD_INPUT_DATA To)
{
    ULONG given = (ULONG)(To - From);
    ULONG taken = 0;

    Upper(Unit->Upper.ClassDeviceObject, From, To, &taken);
    return taken < given ? taken : given;
}

static VOID
SeatPushMouse(_In_ PUNIT_CONTEXT Unit, _In_ UCHAR Seat, _In_ USHORT Flags, _In_ const MOUSE_INPUT_DATA* Data)
{
    SEATINPUT_PACKET p;

    RtlZeroMemory(&p, sizeof(p));
    p.UnitId = Unit->UnitId;
    p.Kind = SEATINPUT_PK_MOUSE;
    p.Seat = Seat;
    p.Flags = Flags;
    p.A = Data->Flags;
    p.B = Data->ButtonFlags;
    p.C = Data->ButtonData;
    p.LastX = Data->LastX;
    p.LastY = Data->LastY;
    SeatRingPush(&p);
}

/* Copy of a seat-1 packet: every important one while the wizard runs, otherwise rate-limited
 * activity. Returns TRUE when something was queued. */
static BOOLEAN
SeatShouldCopy(_Inout_ PUNIT_CONTEXT Unit, _In_ BOOLEAN Important)
{
    ULONGLONG now;

    if (!SeatAttachedOnce() || g.PassThrough || InterlockedCompareExchange(&g.Attached, 0, 0) == 0) {
        return FALSE;
    }
    if ((g.CfgFlags & SEATINPUT_CF_WIZARD_CAPTURE) && Important) {
        return TRUE;
    }
    now = KeQueryInterruptTime();
    /* The port driver serializes one unit's callbacks; a lost race only costs one copy. */
    if (now - Unit->LastCopy >= SEAT_ACTIVITY_COPY_100NS) {
        Unit->LastCopy = now;
        return TRUE;
    }
    return FALSE;
}

/* On a seat-1 unit: Ctrl + move hotkey (only while SEATINPUT_CF_HOTKEY_ENABLED), or
 * Ctrl+Shift + emergency key (EmergencyScan, checked on its own, so the way back to seat 1
 * stays with the move hotkey off). Swallow make, repeats and break; report once, with the
 * Shift state in packet field A (SEATINPUT_HK_SHIFT) so the service can tell the move
 * hotkey from an emergency press (Ctrl+Shift+key, three times).
 * Only while a service handle is attached: with the service gone nothing would act on the
 * report, so the combination passes to the console unchanged (and no stale hotkey is
 * queued for a service that comes back minutes later). */
static BOOLEAN
SeatHotkey(_Inout_ PUNIT_CONTEXT Unit, _In_ const KEYBOARD_INPUT_DATA* Data)
{
    BOOLEAN up = (Data->Flags & KEY_BREAK) != 0;
    BOOLEAN e0 = (Data->Flags & KEY_E0) != 0;
    LONG scan = (g.CfgFlags & SEATINPUT_CF_HOTKEY_ENABLED) ? g.CfgHotkeyScan : 0;
    LONG hflags = g.CfgHotkeyFlags;
    LONG escan = g.CfgEmergencyScan;
    LONG eflags = g.CfgEmergencyFlags;
    BOOLEAN isMove;
    BOOLEAN isEmergency;
    BOOLEAN shift;
    SEATINPUT_PACKET p;

    if (Data->MakeCode == SEAT_SC_CTRL) {
        Unit->CtrlDown = up ? FALSE : TRUE;
        return FALSE;
    }
    if ((Data->MakeCode == SEAT_SC_LSHIFT || Data->MakeCode == SEAT_SC_RSHIFT) && !e0) {
        UCHAR bit = Data->MakeCode == SEAT_SC_LSHIFT ? 1 : 2;
        Unit->ShiftDown = up ? (UCHAR)(Unit->ShiftDown & ~bit) : (UCHAR)(Unit->ShiftDown | bit);
        return FALSE;
    }
    if (!SeatAttachedOnce() || g.PassThrough || (Data->Flags & KEY_E1)) {
        return FALSE;
    }
    isMove = scan != 0 && Data->MakeCode == (USHORT)scan && e0 == ((hflags & KEY_E0) != 0);
    isEmergency = escan != 0 && Data->MakeCode == (USHORT)escan && e0 == ((eflags & KEY_E0) != 0);
    if (!isMove && !isEmergency) {
        return FALSE;
    }
    if (Unit->HotkeySwallowed && Unit->SwallowedScan == Data->MakeCode) {
        if (up) {
            Unit->HotkeySwallowed = FALSE;
        }
        return TRUE; /* break, or auto-repeat */
    }
    if (up) {
        return FALSE;
    }
    if (!Unit->CtrlDown || InterlockedCompareExchange(&g.Attached, 0, 0) == 0) {
        return FALSE;
    }
    shift = Unit->ShiftDown != 0;
    /* Ctrl+Shift needs the emergency key, Ctrl alone the move hotkey (with both off for
     * this key, it passes to the console). */
    if (shift ? !isEmergency : !isMove) {
        return FALSE;
    }
    Unit->HotkeySwallowed = TRUE;
    Unit->SwallowedScan = Data->MakeCode;
    RtlZeroMemory(&p, sizeof(p));
    p.UnitId = Unit->UnitId;
    p.Kind = SEATINPUT_PK_HOTKEY;
    p.Seat = 1;
    p.A = shift ? SEATINPUT_HK_SHIFT : 0;
    SeatRingPush(&p);
    return TRUE;
}

VOID
SeatKbdServiceCallback(PDEVICE_OBJECT DeviceObject, PKEYBOARD_INPUT_DATA Start, PKEYBOARD_INPUT_DATA End, PULONG Consumed)
{
    PUNIT_CONTEXT unit = UnitGetContext(WdfWdmDeviceGetWdfDeviceHandle(DeviceObject));
    SEAT_KBD_CLASS_SERVICE upper = (SEAT_KBD_CLASS_SERVICE)(ULONG_PTR)unit->Upper.ClassService;
    ULONG total = (ULONG)(End - Start);
    ULONG seat;
    ULONG taken;
    BOOLEAN queued = FALSE;
    PKEYBOARD_INPUT_DATA p;
    PKEYBOARD_INPUT_DATA run;
    PKEYBOARD_INPUT_DATA stop = End; /* first packet not accounted for (pass route) */
    SEAT_ROUTE route = SeatRoute(unit, &seat);
    LONG hkScan;
    LONG emScan;

    if (total != 0) {
        SeatTouch(unit, total);
    }
    if (route == SeatRoutePass) {
        /* *Consumed = packets swallowed (hotkey) + packets kbdclass took, up to the first
         * run it took only partly; the port driver re-delivers everything after that, so
         * a full class queue never loses console keys (or key-ups: no stuck keys).
         * A possible hotkey packet first flushes the run before it, so the hotkey is only
         * acted on once every earlier packet was taken. Re-delivered packets repeat only
         * idempotent work (modifier state) and, at worst, one more activity copy. */
        hkScan = g.CfgHotkeyScan;
        emScan = g.CfgEmergencyScan;
        run = Start;
        for (p = Start; p < End; p++) {
            if (((hkScan != 0 && p->MakeCode == (USHORT)hkScan) || (emScan != 0 && p->MakeCode == (USHORT)emScan)) && p > run) {

                taken = SeatKbdPassRun(unit, upper, run, p);
                if (taken < (ULONG)(p - run)) {
                    stop = run + taken;
                    run = End; /* nothing left to pass */
                    break;
                }
                run = p;
            }
            if (SeatShouldCopy(unit, (p->Flags & KEY_BREAK) == 0)) {
                SeatPushKey(unit, 1, SEATINPUT_PF_CONSOLE_COPY, p);
                queued = TRUE;
            }
            if (SeatHotkey(unit, p)) {
                queued = TRUE;
                if (p > run) {
                    /* Only after a hotkey change between the snapshot and SeatHotkey. */
                    taken = SeatKbdPassRun(unit, upper, run, p);
                    if (taken < (ULONG)(p - run)) {
                        /* p was swallowed already; on re-delivery SeatHotkey swallows it
                         * again (HotkeySwallowed) without a second report. */
                        stop = run + taken;
                        run = End;
                        break;
                    }
                }
                run = p + 1;
            }
        }
        if (End > run) {
            taken = SeatKbdPassRun(unit, upper, run, End);
            if (taken < (ULONG)(End - run)) {
                stop = run + taken;
            }
        }
        *Consumed = (ULONG)(stop - Start);
    } else if (route != SeatRouteDrop) {
        for (p = Start; p < End; p++) {
            SeatPushKey(unit,
                        route == SeatRouteSeat ? (UCHAR)seat : 0,
                        route == SeatRouteWizard ? SEATINPUT_PF_WIZARD : 0,
                        p);
        }
        queued = total != 0;
    }
    if (route != SeatRoutePass) {
        *Consumed = total; /* seat, drop and wizard routes take everything themselves */
    }
    if (queued) {
        SeatDrainPending();
    }
}

VOID
SeatMouServiceCallback(PDEVICE_OBJECT DeviceObject, PMOUSE_INPUT_DATA Start, PMOUSE_INPUT_DATA End, PULONG Consumed)
{
    PUNIT_CONTEXT unit = UnitGetContext(WdfWdmDeviceGetWdfDeviceHandle(DeviceObject));
    SEAT_MOU_CLASS_SERVICE upper = (SEAT_MOU_CLASS_SERVICE)(ULONG_PTR)unit->Upper.ClassService;
    ULONG total = (ULONG)(End - Start);
    ULONG seat;
    ULONG consumed = 0;
    BOOLEAN queued = FALSE;
    PMOUSE_INPUT_DATA p;
    SEAT_ROUTE route = SeatRoute(unit, &seat);

    if (total != 0) {
        SeatTouch(unit, total);
    }
    if (route == SeatRoutePass) {
        for (p = Start; p < End; p++) {
            /* Button downs matter to the wizard; motion only as rate-limited activity. */
            if (SeatShouldCopy(unit, (p->ButtonFlags & (MOUSE_LEFT_BUTTON_DOWN | MOUSE_RIGHT_BUTTON_DOWN | MOUSE_MIDDLE_BUTTON_DOWN)) != 0)) {
                SeatPushMouse(unit, 1, SEATINPUT_PF_CONSOLE_COPY, p);
                queued = TRUE;
            }
        }
        if (total != 0) {
            upper(unit->Upper.ClassDeviceObject, Start, End, &consumed);
        }
        /* What mouclass took; a full class queue takes fewer and the port driver keeps
         * the rest (clicks are never lost). Re-delivered packets may give one more copy. */
        *Consumed = consumed < total ? consumed : total;
    } else if (route != SeatRouteDrop) {
        for (p = Start; p < End; p++) {
            SeatPushMouse(unit,
                          route == SeatRouteSeat ? (UCHAR)seat : 0,
                          route == SeatRouteWizard ? SEATINPUT_PF_WIZARD : 0,
                          p);
        }
        queued = total != 0;
    }
    if (route != SeatRoutePass) {
        *Consumed = total;
    }
    if (queued) {
        SeatDrainPending();
    }
}

VOID
SeatEvtInternalIoctl(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t OutputBufferLength,
                     _In_ size_t InputBufferLength, _In_ ULONG IoControlCode)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PUNIT_CONTEXT unit = UnitGetContext(device);
    PCONNECT_DATA connect = NULL;
    size_t length = 0;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case IOCTL_INTERNAL_KEYBOARD_CONNECT:
    case IOCTL_INTERNAL_MOUSE_CONNECT:
        if (unit->Connected) {
            WdfRequestComplete(Request, STATUS_SHARING_VIOLATION);
            return;
        }
        status = WdfRequestRetrieveInputBuffer(Request, sizeof(CONNECT_DATA), (PVOID*)&connect, &length);
        if (!NT_SUCCESS(status)) {
            WdfRequestComplete(Request, status);
            return;
        }
        unit->Upper = *connect;
        connect->ClassDeviceObject = WdfDeviceWdmGetDeviceObject(device);
        connect->ClassService = (IoControlCode == IOCTL_INTERNAL_KEYBOARD_CONNECT)
            ? (PVOID)(ULONG_PTR)SeatKbdServiceCallback
            : (PVOID)(ULONG_PTR)SeatMouServiceCallback;
        unit->Connected = TRUE;
        break;

    case IOCTL_INTERNAL_KEYBOARD_DISCONNECT:
    case IOCTL_INTERNAL_MOUSE_DISCONNECT:
        /* The class drivers never disconnect; unhooking safely is not possible. */
        WdfRequestComplete(Request, STATUS_NOT_IMPLEMENTED);
        return;

    default:
        break;
    }
    SeatForwardRequest(Request, WdfDeviceGetIoTarget(device));
}
