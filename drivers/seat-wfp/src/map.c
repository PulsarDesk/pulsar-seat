/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat source-IP callout - session -> address map (contract:
 * include/seatwfp_ioctl.h). Pure code: no WFP or WDF calls, callable at any IRQL up to
 * DISPATCH_LEVEL on non-paged memory. The Rust encoder (crates/seat-compat/src/wfp.rs)
 * mirrors every rule checked here; its tests run the same cases.
 *
 * Untested on hardware.
 */
#include "seatwfp.h"

static const SEATWFP_SEAT*
SeatWfpSeats(_In_ const UCHAR* Map)
{
    return (const SEATWFP_SEAT*)(Map + sizeof(SEATWFP_MAP_HEADER));
}

static const SEATWFP_PATH*
SeatWfpPaths(_In_ const UCHAR* Map)
{
    const SEATWFP_MAP_HEADER* h = (const SEATWFP_MAP_HEADER*)Map;
    return (const SEATWFP_PATH*)(Map + sizeof(SEATWFP_MAP_HEADER) + (SIZE_T)h->SeatCount * sizeof(SEATWFP_SEAT));
}

_Use_decl_annotations_
NTSTATUS
SeatWfpMapValidate(const UCHAR* Buffer, ULONG Bytes)
{
    const SEATWFP_MAP_HEADER* h;
    const SEATWFP_SEAT* seats;
    const SEATWFP_PATH* paths;
    ULONGLONG tables;
    ULONG i;
    ULONG j;

    if (Buffer == NULL || Bytes < sizeof(SEATWFP_MAP_HEADER) || Bytes > SEATWFP_MAX_MAP_BYTES) {
        return STATUS_INVALID_PARAMETER;
    }
    h = (const SEATWFP_MAP_HEADER*)Buffer;
    if (h->Size != Bytes || h->Major != SEATWFP_INTERFACE_MAJOR) {
        return STATUS_REVISION_MISMATCH;
    }
    if (h->SeatCount > SEATWFP_MAX_SEATS || h->PathCount > SEATWFP_MAX_PATHS) {
        return STATUS_INVALID_PARAMETER;
    }
    tables = (ULONGLONG)sizeof(SEATWFP_MAP_HEADER) + (ULONGLONG)h->SeatCount * sizeof(SEATWFP_SEAT) +
             (ULONGLONG)h->PathCount * sizeof(SEATWFP_PATH);
    if (tables > Bytes) {
        return STATUS_INVALID_PARAMETER;
    }
    seats = SeatWfpSeats(Buffer);
    paths = SeatWfpPaths(Buffer);
    for (i = 0; i < h->SeatCount; i++) {
        const SEATWFP_SEAT* s = &seats[i];
        if (s->Rule > SEATWFP_RULE_ALL_EXCEPT_LISTED) {
            return STATUS_INVALID_PARAMETER;
        }
        if ((s->Flags & ~(USHORT)(SEATWFP_SF_BIND | SEATWFP_SF_CONNECT)) != 0) {
            return STATUS_INVALID_PARAMETER;
        }
        /* 0.0.0.0, 127.x.x.x, multicast and broadcast are never a source address. */
        if ((s->Address[0] == 0 && s->Address[1] == 0 && s->Address[2] == 0 && s->Address[3] == 0) ||
            s->Address[0] == 127 || s->Address[0] >= 224) {
            return STATUS_INVALID_PARAMETER;
        }
        if ((ULONGLONG)s->FirstPath + s->PathCount > h->PathCount) {
            return STATUS_INVALID_PARAMETER;
        }
        for (j = 0; j < i; j++) {
            if (seats[j].SessionId == s->SessionId) {
                return STATUS_DUPLICATE_OBJECTID;
            }
        }
    }
    for (i = 0; i < h->PathCount; i++) {
        const SEATWFP_PATH* p = &paths[i];
        if (p->Chars == 0 || p->Chars > SEATWFP_MAX_PATH_CHARS || (p->Offset & 1) != 0 || p->Offset < tables ||
            (ULONGLONG)p->Offset + (ULONGLONG)p->Chars * sizeof(WCHAR) > Bytes) {
            return STATUS_INVALID_PARAMETER;
        }
    }
    return STATUS_SUCCESS;
}

static WCHAR
SeatWfpLower(_In_ WCHAR c)
{
    return (c >= L'A' && c <= L'Z') ? (WCHAR)(c + (L'a' - L'A')) : c;
}

/* App equals the list path, or lies below it (next character a backslash). */
static BOOLEAN
SeatWfpPathMatches(_In_reads_(PathChars) const WCHAR* Path, _In_ ULONG PathChars, _In_reads_(AppChars) const WCHAR* App,
                   _In_ ULONG AppChars)
{
    ULONG i;

    if (AppChars < PathChars) {
        return FALSE;
    }
    for (i = 0; i < PathChars && i < AppChars; i++) {
        if (SeatWfpLower(App[i]) != SeatWfpLower(Path[i])) {
            return FALSE;
        }
    }
    if (AppChars == PathChars) {
        return TRUE;
    }
    /* i == PathChars < AppChars here. */
    return (i < AppChars && App[i] == L'\\') ? TRUE : FALSE;
}

_Use_decl_annotations_
BOOLEAN
SeatWfpMapLookup(const UCHAR* Map, ULONG Bytes, ULONG SessionId, const WCHAR* AppId, ULONG AppChars, USHORT WantFlag,
                 UCHAR* Address)
{
    const SEATWFP_MAP_HEADER* h = (const SEATWFP_MAP_HEADER*)Map;
    const SEATWFP_SEAT* seats;
    const SEATWFP_PATH* paths;
    const SEATWFP_SEAT* s = NULL;
    BOOLEAN listed = FALSE;
    ULONG i;

    RtlZeroMemory(Address, 4);
    if (Map == NULL || Bytes < sizeof(SEATWFP_MAP_HEADER)) {
        return FALSE;
    }
    seats = SeatWfpSeats(Map);
    for (i = 0; i < h->SeatCount; i++) {
        if (seats[i].SessionId == SessionId) {
            s = &seats[i];
            break;
        }
    }
    if (s == NULL || (s->Flags & WantFlag) == 0) {
        return FALSE;
    }
    /* Trailing terminators of the WFP app id are not part of the path. */
    while (AppId != NULL && AppChars > 0 && AppId[AppChars - 1] == L'\0') {
        AppChars--;
    }
    if (s->Rule != SEATWFP_RULE_ALL && AppId != NULL && AppChars > 0) {
        paths = SeatWfpPaths(Map);
        for (i = 0; i < s->PathCount; i++) {
            const SEATWFP_PATH* p = &paths[s->FirstPath + i];
            if (SeatWfpPathMatches((const WCHAR*)(Map + p->Offset), p->Chars, AppId, AppChars)) {
                listed = TRUE;
                break;
            }
        }
    }
    switch (s->Rule) {
    case SEATWFP_RULE_ALL:
        break;
    case SEATWFP_RULE_ONLY_LISTED:
        if (!listed) {
            return FALSE;
        }
        break;
    default:
        /* All except listed. An unknown image is not on the list: it gets the address. */
        if (listed) {
            return FALSE;
        }
        break;
    }
    RtlCopyMemory(Address, s->Address, 4);
    return TRUE;
}
