/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat source-IP callout - private declarations.
 *
 * Written from the public WDK documentation (Microsoft Learn: "Windows Filtering Platform
 * callout drivers", "Using bind or connect redirection", the fwpsk.h / fwpmk.h reference).
 * Built with WDK 10.0.28000 (unsigned); NEVER LOADED until Microsoft-signed
 * (docs/agents/plan.md). Untested on hardware.
 */
#pragma once

#include <ntifs.h>
#include <ntddk.h>
#pragma warning(push)
#pragma warning(disable : 4201) /* nameless struct / union in the NDIS / WFP headers */
#define NDIS630 1 /* NDIS 6.30 = Windows 8 headers, the WFP redirect-handle API level */
#include <ndis.h>
#include <fwpsk.h>
#pragma warning(pop)
#include <fwpmk.h>
#include <wdf.h>

#include "seatwfp_ioctl.h"

/* Our own keys (random, generated 2026-11-17). Defined in guids.c (initguid.h first);
 * every other file sees declarations. */
DEFINE_GUID(SEATWFP_SUBLAYER_KEY, 0x4f1c0ca3, 0x406f, 0x44d5, 0x9d, 0x66, 0x13, 0x7e, 0x0b, 0x89, 0xed, 0xa3);
DEFINE_GUID(SEATWFP_BIND_CALLOUT_KEY, 0x90751933, 0x2aa7, 0x4f81, 0xa2, 0xf2, 0xd0, 0xc7, 0xdb, 0xf7, 0x55, 0x6a);
DEFINE_GUID(SEATWFP_CONNECT_CALLOUT_KEY, 0x5333f74b, 0xae0d, 0x41d7, 0xaf, 0x52, 0xcd, 0x34, 0x00, 0x3b, 0x0f, 0x5c);
DEFINE_GUID(SEATWFP_PROVIDER_KEY, 0x16f8095d, 0x87b2, 0x4062, 0xaa, 0x46, 0x22, 0x32, 0x85, 0xfd, 0xb8, 0xec);

#define SEATWFP_POOL_TAG 'fWsP'
#define SEATWFP_BUILD    1u

#ifndef RPC_C_AUTHN_WINNT
#define RPC_C_AUTHN_WINNT 10
#endif

typedef struct _SEATWFP_GLOBALS {
    WDFDRIVER Driver;
    WDFDEVICE Control;
    PDEVICE_OBJECT Wdm;

    /* The map in force: a validated copy of the last SET_MAP buffer (non-paged), or NULL.
     * Readers (classify, <= DISPATCH_LEVEL) take MapLock shared; SET_MAP / CLEAR swap the
     * pointer under the exclusive lock and free the old copy after releasing it. */
    EX_SPIN_LOCK MapLock;
    PUCHAR Map;
    ULONG MapBytes;
    ULONG MapGeneration;
    ULONG MapSeats;

    volatile LONG ControlOpen;
    BOOLEAN PassThrough;

    UINT32 BindCalloutId;
    UINT32 ConnectCalloutId;
    BOOLEAN BindRegistered;
    BOOLEAN ConnectRegistered;
    HANDLE RedirectHandle;

    /* Filter engine session (dynamic: its sublayer, callouts and filters go away when the
     * handle closes, at unload or when BFE stops). Guarded by EngineLock, a
     * synchronization event used as a lock: unlike a fast mutex it keeps the holder at
     * PASSIVE_LEVEL with APCs enabled, which the FwpmXxx calls need. */
    KEVENT EngineLock;
    HANDLE Engine;
    HANDLE BfeSubscription;

    volatile LONG64 BindRedirects;
    volatile LONG64 ConnectRedirects;
    volatile LONG64 Skipped;
} SEATWFP_GLOBALS;

extern SEATWFP_GLOBALS g;

/* ---- map.c (pure: no WFP, no WDF) ---- */

/* Checks a SET_MAP buffer (see seatwfp_ioctl.h). */
_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS SeatWfpMapValidate(_In_reads_bytes_(Bytes) const UCHAR* Buffer, _In_ ULONG Bytes);

/* Looks up the address for a process of SessionId whose image is AppId (an NT path in
 * UTF-16, not necessarily terminated; NULL / 0 = unknown image). Returns TRUE and the
 * address octets (dotted order) when the seat's rule gives the process the address.
 * Caller holds MapLock shared and passes the map in force. */
_IRQL_requires_max_(DISPATCH_LEVEL)
BOOLEAN SeatWfpMapLookup(_In_reads_bytes_(Bytes) const UCHAR* Map, _In_ ULONG Bytes, _In_ ULONG SessionId,
                         _In_reads_opt_(AppChars) const WCHAR* AppId, _In_ ULONG AppChars, _In_ USHORT WantFlag,
                         _Out_writes_(4) UCHAR* Address);

/* ---- callout.c ---- */

_IRQL_requires_(PASSIVE_LEVEL)
NTSTATUS SeatWfpCalloutsRegister(VOID);
_IRQL_requires_(PASSIVE_LEVEL)
VOID SeatWfpCalloutsUnregister(VOID);
_IRQL_requires_(PASSIVE_LEVEL)
NTSTATUS SeatWfpBfeStart(VOID);
_IRQL_requires_(PASSIVE_LEVEL)
VOID SeatWfpBfeStop(VOID);
/* SEATWFP_VF_CALLOUTS / SEATWFP_VF_FILTERS. */
ULONG SeatWfpEngineFlags(VOID);

/* ---- driver.c ---- */

_IRQL_requires_(PASSIVE_LEVEL)
NTSTATUS SeatWfpControlCreate(VOID);
_IRQL_requires_(PASSIVE_LEVEL)
VOID SeatWfpMapReplace(_In_opt_ PUCHAR NewMap, _In_ ULONG Bytes);
