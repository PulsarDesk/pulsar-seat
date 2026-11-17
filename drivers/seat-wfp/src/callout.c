/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat source-IP callout - WFP registration and classify (spec 80 section 4.2,
 * section 11.3).
 *
 * Two callouts, IPv4 only (the seat address is IPv4; IPv6 traffic is never touched):
 *
 * - FWPS_LAYER_ALE_BIND_REDIRECT_V4: a TCP / UDP socket of a seat process bound to the
 *   wildcard address (0.0.0.0, explicit or implicit at the first connect / send) is bound
 *   to the seat address instead, port unchanged. Explicit binds to a given address are left
 *   alone (the program chose it).
 * - FWPS_LAYER_ALE_CONNECT_REDIRECT_V4: an outbound TCP connect / UDP flow of a seat process
 *   to a non-loopback destination gets the seat address as its local address (backstop for
 *   sockets bound before the map arrived). Connections redirected by another callout are
 *   left alone (FwpsQueryConnectionRedirectState0), and so are sockets the program bound
 *   explicitly to a given address: the bind classify records their transport endpoint
 *   handle and address in a small table (SeatWfpBound*), the connect classify skips an
 *   endpoint found there with the same local address (parity review 2026-11-17).
 *
 * The seat of a process is its session (one seat per session; the service leaves shared
 * sessions out of the map). The session is read from the process's primary token
 * (PsLookupProcessByProcessId, PsReferencePrimaryToken, SeQuerySessionIdToken), so only at
 * PASSIVE_LEVEL; at a higher IRQL the socket is counted as skipped and left alone.
 * HYPOTHESIS: both ALE redirect layers classify at PASSIVE_LEVEL in the caller's context
 * for socket calls of user-mode processes (GET_STATS Skipped shows it on hardware).
 *
 * Objects live in a dynamic filter-engine session (FWPM_SESSION_FLAG_DYNAMIC): sublayer,
 * callouts and filters disappear when the engine handle closes (unload, BFE stop). Nothing
 * is persistent, nothing survives the driver.
 *
 * Untested on hardware.
 */
#include "seatwfp.h"

#define SEATWFP_IPPROTO_TCP 6
#define SEATWFP_IPPROTO_UDP 17


#ifdef ALLOC_PRAGMA
#pragma alloc_text(PAGE, SeatWfpCalloutsRegister)
#pragma alloc_text(PAGE, SeatWfpCalloutsUnregister)
#pragma alloc_text(PAGE, SeatWfpBfeStart)
#pragma alloc_text(PAGE, SeatWfpBfeStop)
#endif

static volatile LONG SeatWfpFiltersAdded;

/*
 * Explicitly bound endpoints (bind to a non-wildcard address) of the time the map is set:
 * direct-mapped by the transport endpoint handle. A colliding newer bind evicts the older
 * one (that socket then falls back to the connect backstop: rewritten). Handles are reused
 * after a socket closes, so an entry matches only with the same local address too.
 * HYPOTHESIS: FWPS_METADATA_FIELD_TRANSPORT_ENDPOINT_HANDLE is present at both ALE redirect
 * layers for the same socket (Windows 8+ documentation).
 */
#define SEATWFP_BOUND_SLOTS 1024
typedef struct _SEATWFP_BOUND {
    UINT64 Endpoint;
    UINT32 Local; /* host order */
} SEATWFP_BOUND;
static SEATWFP_BOUND SeatWfpBound[SEATWFP_BOUND_SLOTS];
static EX_SPIN_LOCK SeatWfpBoundLock;

static ULONG
SeatWfpBoundSlot(_In_ UINT64 Endpoint)
{
    UINT64 h = Endpoint ^ (Endpoint >> 17) ^ (Endpoint >> 31);
    return (ULONG)(h % SEATWFP_BOUND_SLOTS);
}

static VOID
SeatWfpBoundRecord(_In_ UINT64 Endpoint, _In_ UINT32 Local)
{
    KIRQL irql;
    ULONG s = SeatWfpBoundSlot(Endpoint);

    irql = ExAcquireSpinLockExclusive(&SeatWfpBoundLock);
    SeatWfpBound[s].Endpoint = Endpoint;
    SeatWfpBound[s].Local = Local;
    ExReleaseSpinLockExclusive(&SeatWfpBoundLock, irql);
}

static BOOLEAN
SeatWfpBoundExplicit(_In_ UINT64 Endpoint, _In_ UINT32 Local)
{
    KIRQL irql;
    BOOLEAN hit;
    ULONG s = SeatWfpBoundSlot(Endpoint);

    irql = ExAcquireSpinLockShared(&SeatWfpBoundLock);
    hit = (SeatWfpBound[s].Endpoint == Endpoint && SeatWfpBound[s].Local == Local) ? TRUE : FALSE;
    ExReleaseSpinLockShared(&SeatWfpBoundLock, irql);
    return hit;
}

_IRQL_requires_(PASSIVE_LEVEL)
static VOID
SeatWfpEngineLock(VOID)
{
    (VOID)KeWaitForSingleObject(&g.EngineLock, Executive, KernelMode, FALSE, NULL);
}

_IRQL_requires_(PASSIVE_LEVEL)
static VOID
SeatWfpEngineUnlock(VOID)
{
    (VOID)KeSetEvent(&g.EngineLock, IO_NO_INCREMENT, FALSE);
}

ULONG
SeatWfpEngineFlags(VOID)
{
    ULONG f = 0;
    if (g.BindRegistered && g.ConnectRegistered) {
        f |= SEATWFP_VF_CALLOUTS;
    }
    if (InterlockedCompareExchange(&SeatWfpFiltersAdded, 0, 0) != 0) {
        f |= SEATWFP_VF_FILTERS;
    }
    return f;
}

/* ---- classify ---- */

static BOOLEAN
SeatWfpProcessSession(_In_ UINT64 ProcessId, _Out_ ULONG* SessionId)
{
    PEPROCESS process;
    PACCESS_TOKEN token;
    NTSTATUS status;

    *SessionId = 0;
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return FALSE;
    }
    status = PsLookupProcessByProcessId((HANDLE)(ULONG_PTR)ProcessId, &process);
    if (!NT_SUCCESS(status)) {
        return FALSE;
    }
    token = PsReferencePrimaryToken(process);
    status = token != NULL ? SeQuerySessionIdToken(token, SessionId) : STATUS_NO_TOKEN;
    if (token != NULL) {
        PsDereferencePrimaryToken(token);
    }
    ObDereferenceObject(process);
    return NT_SUCCESS(status) ? TRUE : FALSE;
}

/* Seat address for the socket's process, or FALSE. */
static BOOLEAN
SeatWfpAddressFor(_In_ const FWPS_INCOMING_VALUES0* In, _In_ UINT32 AppField,
                  _In_ const FWPS_INCOMING_METADATA_VALUES0* Meta, _In_ USHORT WantFlag, _Out_writes_(4) UCHAR* Address)
{
    ULONG session;
    const FWP_BYTE_BLOB* app;
    const WCHAR* appChars = NULL;
    ULONG appCount = 0;
    BOOLEAN found = FALSE;
    KIRQL irql;

    RtlZeroMemory(Address, 4);
    if (ReadULongAcquire(&g.MapSeats) == 0) {
        return FALSE;
    }
    if (!FWPS_IS_METADATA_FIELD_PRESENT(Meta, FWPS_METADATA_FIELD_PROCESS_ID)) {
        return FALSE;
    }
    if (!SeatWfpProcessSession(Meta->processId, &session)) {
        InterlockedIncrement64(&g.Skipped);
        return FALSE;
    }
    if (In->incomingValue[AppField].value.type == FWP_BYTE_BLOB_TYPE) {
        app = In->incomingValue[AppField].value.byteBlob;
        if (app != NULL && app->data != NULL && app->size >= sizeof(WCHAR)) {
            appChars = (const WCHAR*)app->data;
            appCount = app->size / sizeof(WCHAR);
        }
    }
    irql = ExAcquireSpinLockShared(&g.MapLock);
    if (g.Map != NULL) {
        found = SeatWfpMapLookup(g.Map, g.MapBytes, session, appChars, appCount, WantFlag, Address);
    }
    ExReleaseSpinLockShared(&g.MapLock, irql);
    return found;
}

static UINT32
SeatWfpHostOrder(_In_reads_(4) const UCHAR* A)
{
    return ((UINT32)A[0] << 24) | ((UINT32)A[1] << 16) | ((UINT32)A[2] << 8) | (UINT32)A[3];
}

static BOOLEAN
SeatWfpTcpOrUdp(_In_ const FWPS_INCOMING_VALUES0* In, _In_ UINT32 Field)
{
    UINT8 p = In->incomingValue[Field].value.uint8;
    return (p == SEATWFP_IPPROTO_TCP || p == SEATWFP_IPPROTO_UDP) ? TRUE : FALSE;
}

/* Writes the address into the writable copy of the layer data and applies it. */
static BOOLEAN
SeatWfpRewrite(_In_opt_ const void* ClassifyContext, _In_ const FWPS_FILTER2* Filter, _Inout_ FWPS_CLASSIFY_OUT0* Out,
               _In_ BOOLEAN Connect, _In_reads_(4) const UCHAR* Address)
{
    UINT64 handle = 0;
    PVOID data = NULL;
    SOCKADDR_STORAGE* local;
    SOCKADDR_IN* sin;
    NTSTATUS status;

    if (ClassifyContext == NULL) {
        return FALSE;
    }
    status = FwpsAcquireClassifyHandle0((void*)ClassifyContext, 0, &handle);
    if (!NT_SUCCESS(status)) {
        return FALSE;
    }
    status = FwpsAcquireWritableLayerDataPointer0(handle, Filter->filterId, 0, &data, Out);
    if (!NT_SUCCESS(status) || data == NULL) {
        FwpsReleaseClassifyHandle0(handle);
        return FALSE;
    }
    local = Connect ? &((FWPS_CONNECT_REQUEST0*)data)->localAddressAndPort : &((FWPS_BIND_REQUEST0*)data)->localAddressAndPort;
    if (local->ss_family != AF_INET) {
        /* Hand the unmodified copy back. */
        FwpsApplyModifiedLayerData0(handle, data, 0);
        FwpsReleaseClassifyHandle0(handle);
        return FALSE;
    }
    sin = (SOCKADDR_IN*)local;
    RtlCopyMemory(&sin->sin_addr, Address, 4); /* network byte order = dotted order; port kept */
    FwpsApplyModifiedLayerData0(handle, data, 0);
    FwpsReleaseClassifyHandle0(handle);
    return TRUE;
}

static VOID NTAPI
SeatWfpClassifyBind(_In_ const FWPS_INCOMING_VALUES0* inFixedValues, _In_ const FWPS_INCOMING_METADATA_VALUES0* inMetaValues,
                    _Inout_opt_ void* layerData, _In_opt_ const void* classifyContext, _In_ const FWPS_FILTER2* filter, _In_ UINT64 flowContext,
                    _Inout_ FWPS_CLASSIFY_OUT0* classifyOut)
{
    UCHAR address[4];
    UINT32 local;

    UNREFERENCED_PARAMETER(layerData);
    UNREFERENCED_PARAMETER(flowContext);

    if ((classifyOut->rights & FWPS_RIGHT_ACTION_WRITE) == 0) {
        return; /* a stronger filter decided already */
    }
    classifyOut->actionType = FWP_ACTION_CONTINUE;
    if (!SeatWfpTcpOrUdp(inFixedValues, FWPS_FIELD_ALE_BIND_REDIRECT_V4_IP_PROTOCOL)) {
        return;
    }
    local = inFixedValues->incomingValue[FWPS_FIELD_ALE_BIND_REDIRECT_V4_IP_LOCAL_ADDRESS].value.uint32;
    if (local != 0) {
        /* Explicit bind to an address: left alone, and remembered so the connect backstop
         * leaves it alone too. Only while a map is set (nothing to protect otherwise). */
        if (ReadULongAcquire(&g.MapSeats) != 0 && (local >> 24) != 127 &&
            FWPS_IS_METADATA_FIELD_PRESENT(inMetaValues, FWPS_METADATA_FIELD_TRANSPORT_ENDPOINT_HANDLE)) {
            SeatWfpBoundRecord(inMetaValues->transportEndpointHandle, local);
        }
        return;
    }
    if (!SeatWfpAddressFor(inFixedValues, FWPS_FIELD_ALE_BIND_REDIRECT_V4_ALE_APP_ID, inMetaValues, SEATWFP_SF_BIND, address)) {
        return;
    }
    if (SeatWfpRewrite(classifyContext, filter, classifyOut, FALSE, address)) {
        InterlockedIncrement64(&g.BindRedirects);
        classifyOut->actionType = FWP_ACTION_PERMIT;
    } else {
        InterlockedIncrement64(&g.Skipped);
        classifyOut->actionType = FWP_ACTION_CONTINUE;
    }
}

static VOID NTAPI
SeatWfpClassifyConnect(_In_ const FWPS_INCOMING_VALUES0* inFixedValues, _In_ const FWPS_INCOMING_METADATA_VALUES0* inMetaValues,
                       _Inout_opt_ void* layerData, _In_opt_ const void* classifyContext, _In_ const FWPS_FILTER2* filter, _In_ UINT64 flowContext,
                       _Inout_ FWPS_CLASSIFY_OUT0* classifyOut)
{
    UCHAR address[4];
    UINT32 local;
    UINT32 remote;
    FWPS_CONNECTION_REDIRECT_STATE state;

    UNREFERENCED_PARAMETER(layerData);
    UNREFERENCED_PARAMETER(flowContext);

    if ((classifyOut->rights & FWPS_RIGHT_ACTION_WRITE) == 0) {
        return;
    }
    classifyOut->actionType = FWP_ACTION_CONTINUE;
    if (!SeatWfpTcpOrUdp(inFixedValues, FWPS_FIELD_ALE_CONNECT_REDIRECT_V4_IP_PROTOCOL)) {
        return;
    }
    remote = inFixedValues->incomingValue[FWPS_FIELD_ALE_CONNECT_REDIRECT_V4_IP_REMOTE_ADDRESS].value.uint32;
    local = inFixedValues->incomingValue[FWPS_FIELD_ALE_CONNECT_REDIRECT_V4_IP_LOCAL_ADDRESS].value.uint32;
    if ((remote >> 24) == 127 || (local >> 24) == 127) {
        return; /* loopback traffic keeps working */
    }
    if (g.RedirectHandle != NULL && FWPS_IS_METADATA_FIELD_PRESENT(inMetaValues, FWPS_METADATA_FIELD_REDIRECT_RECORD_HANDLE) &&
        inMetaValues->redirectRecords != NULL) {
        state = FwpsQueryConnectionRedirectState0(inMetaValues->redirectRecords, g.RedirectHandle, NULL);
        if (state != FWPS_CONNECTION_NOT_REDIRECTED) {
            return; /* ours already, or another proxy's: never fight over it */
        }
    }
    if (!SeatWfpAddressFor(inFixedValues, FWPS_FIELD_ALE_CONNECT_REDIRECT_V4_ALE_APP_ID, inMetaValues, SEATWFP_SF_CONNECT, address)) {
        return;
    }
    if (local == SeatWfpHostOrder(address)) {
        return; /* already the seat address (bind redirect did it) */
    }
    if (FWPS_IS_METADATA_FIELD_PRESENT(inMetaValues, FWPS_METADATA_FIELD_TRANSPORT_ENDPOINT_HANDLE) &&
        SeatWfpBoundExplicit(inMetaValues->transportEndpointHandle, local)) {
        return; /* the program bound this socket to this address itself */
    }
    if (SeatWfpRewrite(classifyContext, filter, classifyOut, TRUE, address)) {
        InterlockedIncrement64(&g.ConnectRedirects);
        classifyOut->actionType = FWP_ACTION_PERMIT;
    } else {
        InterlockedIncrement64(&g.Skipped);
        classifyOut->actionType = FWP_ACTION_CONTINUE;
    }
}

static NTSTATUS NTAPI
SeatWfpNotify(_In_ FWPS_CALLOUT_NOTIFY_TYPE notifyType, _In_ const GUID* filterKey, _Inout_ FWPS_FILTER2* filter)
{
    UNREFERENCED_PARAMETER(notifyType);
    UNREFERENCED_PARAMETER(filterKey);
    UNREFERENCED_PARAMETER(filter);
    return STATUS_SUCCESS;
}

/* ---- registration ---- */

static NTSTATUS
SeatWfpRegisterOne(_In_ const GUID* Key, _In_ FWPS_CALLOUT_CLASSIFY_FN2 Classify, _Out_ UINT32* Id)
{
    FWPS_CALLOUT2 c;

    RtlZeroMemory(&c, sizeof(c));
    c.calloutKey = *Key;
    c.classifyFn = Classify;
    c.notifyFn = SeatWfpNotify;
    return FwpsCalloutRegister2(g.Wdm, &c, Id);
}

_Use_decl_annotations_
NTSTATUS
SeatWfpCalloutsRegister(VOID)
{
    NTSTATUS status;

    PAGED_CODE();

    status = FwpsRedirectHandleCreate0(&SEATWFP_PROVIDER_KEY, 0, &g.RedirectHandle);
    if (!NT_SUCCESS(status)) {
        g.RedirectHandle = NULL;
        return status;
    }
    status = SeatWfpRegisterOne(&SEATWFP_BIND_CALLOUT_KEY, SeatWfpClassifyBind, &g.BindCalloutId);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    g.BindRegistered = TRUE;
    status = SeatWfpRegisterOne(&SEATWFP_CONNECT_CALLOUT_KEY, SeatWfpClassifyConnect, &g.ConnectCalloutId);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    g.ConnectRegistered = TRUE;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
SeatWfpCalloutsUnregister(VOID)
{
    PAGED_CODE();

    if (g.ConnectRegistered) {
        (VOID)FwpsCalloutUnregisterById0(g.ConnectCalloutId);
        g.ConnectRegistered = FALSE;
    }
    if (g.BindRegistered) {
        (VOID)FwpsCalloutUnregisterById0(g.BindCalloutId);
        g.BindRegistered = FALSE;
    }
    if (g.RedirectHandle != NULL) {
        FwpsRedirectHandleDestroy0(g.RedirectHandle);
        g.RedirectHandle = NULL;
    }
}

static NTSTATUS
SeatWfpAddCallout(_In_ HANDLE Engine, _In_ const GUID* Key, _In_ const GUID* Layer, _In_ PWSTR Name)
{
    FWPM_CALLOUT0 c;

    RtlZeroMemory(&c, sizeof(c));
    c.calloutKey = *Key;
    c.displayData.name = Name;
    c.applicableLayer = *Layer;
    return FwpmCalloutAdd0(Engine, &c, NULL, NULL);
}

static NTSTATUS
SeatWfpAddFilter(_In_ HANDLE Engine, _In_ const GUID* Callout, _In_ const GUID* Layer, _In_ PWSTR Name)
{
    FWPM_FILTER0 f;

    RtlZeroMemory(&f, sizeof(f));
    f.displayData.name = Name;
    f.layerKey = *Layer;
    f.subLayerKey = SEATWFP_SUBLAYER_KEY;
    f.weight.type = FWP_EMPTY; /* auto weight inside our sublayer */
    f.numFilterConditions = 0; /* every socket; the classify function picks the seat ones */
    f.action.type = FWP_ACTION_CALLOUT_UNKNOWN;
    f.action.calloutKey = *Callout;
    return FwpmFilterAdd0(Engine, &f, NULL, NULL);
}

/* Caller holds EngineLock. */
static VOID
SeatWfpAddFiltersLocked(VOID)
{
    FWPM_SESSION0 session;
    FWPM_SUBLAYER0 sub;
    HANDLE engine = NULL;
    NTSTATUS status;
    static WCHAR subName[] = L"Pulsar Seat source address";
    static WCHAR bindName[] = L"Pulsar Seat bind redirect";
    static WCHAR connectName[] = L"Pulsar Seat connect redirect";

    if (g.Engine != NULL) {
        return;
    }
    RtlZeroMemory(&session, sizeof(session));
    session.flags = FWPM_SESSION_FLAG_DYNAMIC;
    status = FwpmEngineOpen0(NULL, RPC_C_AUTHN_WINNT, NULL, &session, &engine);
    if (!NT_SUCCESS(status)) {
        return;
    }
    status = FwpmTransactionBegin0(engine, 0);
    if (NT_SUCCESS(status)) {
        RtlZeroMemory(&sub, sizeof(sub));
        sub.subLayerKey = SEATWFP_SUBLAYER_KEY;
        sub.displayData.name = subName;
        sub.weight = 0x100; /* low: firewall and VPN sublayers keep their say */
        status = FwpmSubLayerAdd0(engine, &sub, NULL);
        if (NT_SUCCESS(status)) {
            status = SeatWfpAddCallout(engine, &SEATWFP_BIND_CALLOUT_KEY, &FWPM_LAYER_ALE_BIND_REDIRECT_V4, bindName);
        }
        if (NT_SUCCESS(status)) {
            status = SeatWfpAddCallout(engine, &SEATWFP_CONNECT_CALLOUT_KEY, &FWPM_LAYER_ALE_CONNECT_REDIRECT_V4, connectName);
        }
        if (NT_SUCCESS(status)) {
            status = SeatWfpAddFilter(engine, &SEATWFP_BIND_CALLOUT_KEY, &FWPM_LAYER_ALE_BIND_REDIRECT_V4, bindName);
        }
        if (NT_SUCCESS(status)) {
            status = SeatWfpAddFilter(engine, &SEATWFP_CONNECT_CALLOUT_KEY, &FWPM_LAYER_ALE_CONNECT_REDIRECT_V4, connectName);
        }
        if (NT_SUCCESS(status)) {
            status = FwpmTransactionCommit0(engine);
        } else {
            (VOID)FwpmTransactionAbort0(engine);
        }
    }
    if (!NT_SUCCESS(status)) {
        (VOID)FwpmEngineClose0(engine);
        return;
    }
    g.Engine = engine;
    InterlockedExchange(&SeatWfpFiltersAdded, 1);
}

/* Caller holds EngineLock. */
static VOID
SeatWfpRemoveFiltersLocked(VOID)
{
    if (g.Engine != NULL) {
        /* Dynamic session: closing it deletes the sublayer, callouts and filters. */
        (VOID)FwpmEngineClose0(g.Engine);
        g.Engine = NULL;
    }
    InterlockedExchange(&SeatWfpFiltersAdded, 0);
}

static VOID NTAPI
SeatWfpBfeChanged(_Inout_ void* context, _In_ FWPM_SERVICE_STATE newState)
{
    UNREFERENCED_PARAMETER(context);

    SeatWfpEngineLock();
    if (newState == FWPM_SERVICE_RUNNING) {
        SeatWfpAddFiltersLocked();
    } else if (newState == FWPM_SERVICE_STOP_PENDING) {
        SeatWfpRemoveFiltersLocked();
    }
    SeatWfpEngineUnlock();
}

_Use_decl_annotations_
NTSTATUS
SeatWfpBfeStart(VOID)
{
    NTSTATUS status;

    PAGED_CODE();

    /* BFE may start after us at boot: subscribe first, then add now if it already runs. */
    status = FwpmBfeStateSubscribeChanges0(g.Wdm, SeatWfpBfeChanged, NULL, &g.BfeSubscription);
    if (!NT_SUCCESS(status)) {
        g.BfeSubscription = NULL;
        return status;
    }
    if (FwpmBfeStateGet0() == FWPM_SERVICE_RUNNING) {
        SeatWfpEngineLock();
        SeatWfpAddFiltersLocked();
        SeatWfpEngineUnlock();
    }
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
SeatWfpBfeStop(VOID)
{
    PAGED_CODE();

    if (g.BfeSubscription != NULL) {
        (VOID)FwpmBfeStateUnsubscribeChanges0(g.BfeSubscription);
        g.BfeSubscription = NULL;
    }
    SeatWfpEngineLock();
    SeatWfpRemoveFiltersLocked();
    SeatWfpEngineUnlock();
}
