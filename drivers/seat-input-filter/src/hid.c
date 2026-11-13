/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat input filter - HID collection visibility (spec 50 section 4.1.3).
 *
 * Decides on IRP_MJ_CREATE only, by the requestor's session; never reads or changes
 * reports and never holds reads. Rules, in order:
 *   0. before the first service attach since boot, Safe Mode, recovery value: allow;
 *   1. unit on seat 1, shared, or its usage is in the unfiltered list (usage 0 in the list
 *      is a whole-page wildcard), or it is a system-control collection (page 0x01, usage
 *      0x80: power / sleep keys) and the service set SEATINPUT_CF_ALLOW_SYSTEM_BUTTONS:
 *      allow;
 *   2. threads of the System process (no session): allow. Requestor session 0: allow only
 *      when the create's token user is a service account (LocalSystem, LocalService,
 *      NetworkService, NT SERVICE\* = S-1-5-80-*), the same rule as the storage gate
 *      (seat-storage-filter gate.c). "Session 0" alone is not enough: a user's own code can
 *      run there (S4U / stored-password scheduled tasks, BITS, impersonating services) and
 *      would read another seat's gamepad or touch screen. Other session-0 requestors are
 *      judged like an unmapped session (rule 4). The decision is always by
 *      IoGetRequestorSessionId, also for kernel-mode requestors: win32k's raw input thread
 *      opens touch, pen, consumer and system-control collections from kernel mode in each
 *      session's own context, so a "kernel mode = allow" shortcut would let the console
 *      consume an extra seat's touch screen and media / power keys (spec 50 section 4.1.3,
 *      4.1.10). Only when the session lookup itself fails do we fail open (a requestor we
 *      cannot classify is a system component; denying it could break the PC, while letting
 *      it through only weakens isolation for that one open);
 *   3. requestor session is the session of a seat in the unit's mask: allow;
 *   4. otherwise STATUS_ACCESS_DENIED.
 * An allowed open stays valid after a reassignment (the service reports needs_replug).
 *
 * Untested on hardware.
 */
#include "seatinput.h"

static BOOLEAN SeatIsServiceSid(_In_opt_ PSID Sid);
static BOOLEAN SeatCreateByService(_In_ PIRP Irp);
static BOOLEAN SeatHidAllow(_In_ PUNIT_CONTEXT Unit, _In_ WDFREQUEST Request);

#ifdef ALLOC_PRAGMA
#pragma alloc_text(PAGE, SeatHidEvtFileCreate)
#pragma alloc_text(PAGE, SeatHidParseUsage)
#pragma alloc_text(PAGE, SeatIsServiceSid)
#pragma alloc_text(PAGE, SeatCreateByService)
#pragma alloc_text(PAGE, SeatHidAllow)
#endif

static BOOLEAN
SeatHex4(_In_reads_(4) PCWSTR s, _Out_ PUSHORT Value)
{
    ULONG v = 0;
    ULONG i;
    WCHAR c;

    for (i = 0; i < 4; i++) {
        c = s[i];
        v <<= 4;
        if (c >= L'0' && c <= L'9') {
            v |= (ULONG)(c - L'0');
        } else if (c >= L'a' && c <= L'f') {
            v |= (ULONG)(c - L'a' + 10);
        } else if (c >= L'A' && c <= L'F') {
            v |= (ULONG)(c - L'A' + 10);
        } else {
            *Value = 0;
            return FALSE;
        }
    }
    *Value = (USHORT)v;
    return TRUE;
}

/* Usage page / usage from `HID_DEVICE_UP:pppp_U:uuuu` (or the HID_DEVICE_SYSTEM_* ids)
 * in a MULTI_SZ of hardware / compatible ids. */
VOID
SeatHidParseUsage(_Inout_ PUSHORT UsagePage, _Inout_ PUSHORT Usage, _In_reads_bytes_(Bytes) PCWSTR MultiSz, _In_ ULONG Bytes)
{
    static const WCHAR up[] = L"HID_DEVICE_UP:";
    const SIZE_T upLen = (sizeof(up) / sizeof(WCHAR)) - 1;
    ULONG chars = Bytes / sizeof(WCHAR);
    ULONG i = 0;
    SIZE_T len;
    PCWSTR s;
    USHORT page;
    USHORT usage;

    PAGED_CODE();

    while (i < chars && MultiSz[i] != L'\0') {
        s = &MultiSz[i];
        len = 0;
        while (i + len < chars && s[len] != L'\0') {
            len++;
        }
        if (len >= upLen + 4 + 3 + 4 && _wcsnicmp(s, up, upLen) == 0
            && SeatHex4(s + upLen, &page)
            && (s[upLen + 4] == L'_') && (s[upLen + 5] == L'U' || s[upLen + 5] == L'u') && s[upLen + 6] == L':'
            && SeatHex4(s + upLen + 7, &usage)) {
            *UsagePage = page;
            *Usage = usage;
            return;
        }
        if (len == 22 && _wcsnicmp(s, L"HID_DEVICE_SYSTEM_GAME", 22) == 0) {
            *UsagePage = 0x01;
            *Usage = 0x05;
            return;
        }
        if (len == 26 && _wcsnicmp(s, L"HID_DEVICE_SYSTEM_CONSUMER", 26) == 0) {
            *UsagePage = 0x0C;
            *Usage = 0x01;
            return;
        }
        if (len == 25 && _wcsnicmp(s, L"HID_DEVICE_SYSTEM_CONTROL", 25) == 0) {
            *UsagePage = 0x01;
            *Usage = 0x80;
            return;
        }
        i += (ULONG)len + 1;
    }
}

static BOOLEAN
SeatHidUnfiltered(_In_ PUNIT_CONTEXT Unit)
{
    BOOLEAN hit = FALSE;
    ULONG i;

    WdfWaitLockAcquire(g.UnitLock, NULL);
    for (i = 0; i < g.UnfilteredCount && i < SEATINPUT_MAX_UNFILTERED; i++) {
        /* Usage 0 is reserved in every HID usage page, so it serves as "whole page". */
        if (g.Unfiltered[i].Page == Unit->UsagePage
            && (g.Unfiltered[i].Usage == 0 || g.Unfiltered[i].Usage == Unit->Usage)) {
            hit = TRUE;
            break;
        }
    }
    WdfWaitLockRelease(g.UnitLock);
    return hit;
}

/* LocalSystem, LocalService, NetworkService, or a per-service virtual account
 * (NT SERVICE\<name> = S-1-5-80-...). Adapted from pulsar-seat's storage gate
 * (drivers/seat-storage-filter/src/gate.c PssgIsServiceSid). */
static BOOLEAN
SeatIsServiceSid(_In_opt_ PSID Sid)
{
    SID_IDENTIFIER_AUTHORITY nt = SECURITY_NT_AUTHORITY;

    PAGED_CODE();
    if (Sid == NULL || !RtlValidSid(Sid)) {
        return FALSE;
    }
    if (RtlEqualSid(Sid, SeExports->SeLocalSystemSid) || RtlEqualSid(Sid, SeExports->SeLocalServiceSid) ||
        RtlEqualSid(Sid, SeExports->SeNetworkServiceSid)) {
        return TRUE;
    }
    /* RtlIdentifierAuthoritySid is not exported to kernel mode: read the documented SID
     * structure (ntifs.h) directly; RtlValidSid above checked it. */
    return *RtlSubAuthorityCountSid(Sid) >= 1 &&
           RtlEqualMemory(&((PISID)Sid)->IdentifierAuthority, &nt, sizeof(nt)) &&
           *RtlSubAuthoritySid(Sid, 0) == SECURITY_SERVICE_ID_BASE_RID;
}

/* PASSIVE_LEVEL (create path). The create's effective token (the client token when the
 * requestor impersonates) belongs to a service account. No token / no access state = not
 * a service (deny path). */
static BOOLEAN
SeatCreateByService(_In_ PIRP Irp)
{
    PIO_STACK_LOCATION sl = IoGetCurrentIrpStackLocation(Irp);
    PIO_SECURITY_CONTEXT sc = sl->Parameters.Create.SecurityContext;
    PSECURITY_SUBJECT_CONTEXT subject;
    PACCESS_TOKEN token;
    PTOKEN_USER user = NULL;
    BOOLEAN service = FALSE;

    PAGED_CODE();
    if (sl->MajorFunction != IRP_MJ_CREATE || sc == NULL || sc->AccessState == NULL) {
        return FALSE;
    }
    subject = &sc->AccessState->SubjectSecurityContext;
    SeLockSubjectContext(subject);
    token = SeQuerySubjectContextToken(subject);
    if (token != NULL && NT_SUCCESS(SeQueryInformationToken(token, TokenUser, (PVOID*)&user)) && user != NULL) {
        service = SeatIsServiceSid(user->User.Sid);
        ExFreePool(user);
    }
    SeUnlockSubjectContext(subject);
    return service;
}

static BOOLEAN
SeatHidAllow(_In_ PUNIT_CONTEXT Unit, _In_ WDFREQUEST Request)
{
    PIRP irp;
    ULONG session = 0;
    ULONG mask;
    ULONG i;

    PAGED_CODE();
    if (g.PassThrough || Unit->Role != SeatRoleHid || InterlockedCompareExchange(&g.AttachedOnce, 0, 0) == 0) {
        return TRUE;
    }
    mask = (ULONG)InterlockedCompareExchange(&Unit->Mask, 0, 0);
    if (mask == 0xFFFFFFFFu || (mask & 1u) != 0 || SeatHidUnfiltered(Unit)) {
        return TRUE;
    }
    /* Power / sleep keys of any keyboard reach every session when the service allows it
     * (the service also lists 0x01/0x80 as unfiltered; this keeps the flag effective even
     * when that list is full). */
    if ((InterlockedCompareExchange(&g.CfgFlags, 0, 0) & SEATINPUT_CF_ALLOW_SYSTEM_BUTTONS) != 0 &&
        Unit->UsagePage == 0x01 && Unit->Usage == 0x80) {
        return TRUE;
    }
    irp = WdfRequestWdmGetIrp(Request);
    /* System-process threads (PsInitialSystemProcess) have no session: session 0. */
    if (IoGetRequestorProcess(irp) == PsInitialSystemProcess) {
        return TRUE;
    }
    if (!NT_SUCCESS(IoGetRequestorSessionId(irp, &session))) {
        /* Session lookup failed: fail open for this one open (see rule 2 above). */
        return TRUE;
    }
    if (session == 0 && SeatCreateByService(irp)) {
        return TRUE;
    }
    /* Session 0 but a user's token: no seat maps session 0, so the loop below denies. */
    for (i = 0; i < SEATINPUT_MAX_SEATS; i++) {
        if ((mask & (1u << i)) != 0 && (ULONG)g.SessionOfSeat[i] == session) {
            return TRUE;
        }
    }
    return FALSE;
}

static EVT_WDF_REQUEST_COMPLETION_ROUTINE SeatCreateCompletion;

static VOID
SeatCreateCompletion(_In_ WDFREQUEST Request, _In_ WDFIOTARGET Target, _In_ PWDF_REQUEST_COMPLETION_PARAMS Params,
                     _In_ WDFCONTEXT Context)
{
    UNREFERENCED_PARAMETER(Target);
    UNREFERENCED_PARAMETER(Context);
    WdfRequestComplete(Request, Params->IoStatus.Status);
}

VOID
SeatHidEvtFileCreate(_In_ WDFDEVICE Device, _In_ WDFREQUEST Request, _In_ WDFFILEOBJECT FileObject)
{
    PUNIT_CONTEXT unit = UnitGetContext(Device);

    UNREFERENCED_PARAMETER(FileObject);
    PAGED_CODE();

    if (!SeatHidAllow(unit, Request)) {
        WdfRequestComplete(Request, STATUS_ACCESS_DENIED);
        return;
    }
    WdfRequestFormatRequestUsingCurrentType(Request);
    WdfRequestSetCompletionRoutine(Request, SeatCreateCompletion, WDF_NO_CONTEXT);
    if (!WdfRequestSend(Request, WdfDeviceGetIoTarget(Device), WDF_NO_SEND_OPTIONS)) {
        WdfRequestComplete(Request, WdfRequestGetStatus(Request));
    }
}
