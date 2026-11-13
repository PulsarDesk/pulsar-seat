/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat storage gate: Filter Manager minifilter that refuses file opens on a volume
 * owned by other seats (spec 70 §5.2(4)).
 *
 * EXCEPTION: a minifilter is outside the rules.md driver list. This source exists so the
 * maintainer can decide; it is never built, installed or loaded without the maintainer's
 * explicit OK, an altitude allocated by Microsoft and an attestation-signed build
 * (spec 70 §5.3, drivers/README.md). Built unsigned (WDK 10.0.28000). UNTESTED ON HARDWARE.
 *
 * Written fresh from the public Filter Manager documentation (Microsoft Learn). The WDK
 * samples are MS-PL and were not copied.
 *
 * Behaviour:
 *  - Fails open: passes everything until the service committed a plan since boot, after
 *    PASS_ALL, in Windows Safe Mode, or when Parameters\FailOpen = 1.
 *  - Once armed, a create on a volume with an owner mask (not shared) is refused with
 *    STATUS_ACCESS_DENIED unless: the requestor is kernel mode without SL_FORCE_ACCESS_CHECK
 *    (a kernel create that asks for an access check, e.g. the SMB server serving a share
 *    for a client, is judged like a user request on the impersonated client's token and
 *    the requesting session), its token user is a service
 *    account (LocalSystem, LocalService, NetworkService, NT SERVICE\*), its session is
 *    mapped to an owning seat, or (with ADMINS_BYPASS) it holds an administrator token.
 *    The exemption is decided on the token, never on "session 0": a user's own code can
 *    run in session 0 (S4U / stored-password scheduled tasks, BITS jobs, impersonating RPC
 *    services) and is then judged like any other unmapped session (denied).
 *  - The volume GUID is looked up at instance setup and, when that fails (e.g. the mount
 *    manager has not published it yet for a new stick), again at PASSIVE_LEVEL from a
 *    generic work item queued by the first create. While unresolved, a volume on a
 *    removable bus (USB, SD, MMC, removable media, optical) is denied once armed.
 *  - Paging-file opens are never touched. Network file systems are not attached.
 *  - The communication port is admin + SYSTEM only (default filter port descriptor) and
 *    accepts one client (the service).
 *  - Unloadable (fltmc unload / service stop) for uninstall and update.
 *
 * HVCI: no executable pool, NX pool only, no RWX sections; link with /INTEGRITYCHECK.
 */

#include <fltKernel.h>
#include <ntddstor.h>
#include <ntstrsafe.h>

#include "../inc/seat_storage_gate.h"

#define PSSG_TAG 'gSsP'
#define PSSG_BUILD 1

typedef struct _PSSG_VOLUME_ENTRY {
    ULONG SeatMask;
    WCHAR Guid[PSSG_VOLUME_GUID_CHARS];
} PSSG_VOLUME_ENTRY;

typedef struct _PSSG_SESSION_ENTRY {
    ULONG SessionId;
    ULONG SeatBit;
} PSSG_SESSION_ENTRY;

typedef struct _PSSG_PLAN {
    ULONG Flags;
    ULONG SessionCount;
    ULONG VolumeCount;
    PSSG_SESSION_ENTRY Sessions[PSSG_MAX_SESSIONS];
    PSSG_VOLUME_ENTRY Volumes[PSSG_MAX_VOLUMES];
} PSSG_PLAN;

typedef struct _PSSG_INSTANCE_CONTEXT {
    volatile LONG HasGuid;       /* 1 once Guid is valid; Guid is written before the flag */
    volatile LONG ResolveQueued; /* a lazy lookup work item is queued or running */
    BOOLEAN Removable;           /* removable bus / media: denied while unresolved */
    WCHAR Guid[PSSG_VOLUME_GUID_CHARS];
} PSSG_INSTANCE_CONTEXT, *PPSSG_INSTANCE_CONTEXT;

/* Globals (nonpaged .data / .bss). */
static PFLT_FILTER g_Filter;
static PFLT_PORT g_ServerPort;
static PFLT_PORT g_ClientPort;
static EX_PUSH_LOCK g_PlanLock;
static EX_PUSH_LOCK g_StageLock; /* held exclusive for every staging command */
static PSSG_PLAN g_Active;  /* guarded by g_PlanLock */
static PSSG_PLAN g_Staged;  /* guarded by g_StageLock: one client may still send from
                             * several threads at once (MaxConnections limits connections,
                             * not concurrency); lock order g_StageLock -> g_PlanLock */
static volatile LONG g_Armed;
static BOOLEAN g_ForceFailOpen;

DRIVER_INITIALIZE DriverEntry;
static NTSTATUS FLTAPI PssgUnload(FLT_FILTER_UNLOAD_FLAGS Flags);
static NTSTATUS FLTAPI PssgInstanceSetup(PCFLT_RELATED_OBJECTS FltObjects, FLT_INSTANCE_SETUP_FLAGS Flags,
                                         DEVICE_TYPE VolumeDeviceType, FLT_FILESYSTEM_TYPE VolumeFilesystemType);
static FLT_PREOP_CALLBACK_STATUS FLTAPI PssgPreCreate(PFLT_CALLBACK_DATA Data, PCFLT_RELATED_OBJECTS FltObjects,
                                                       PVOID *CompletionContext);
static NTSTATUS FLTAPI PssgConnect(PFLT_PORT ClientPort, PVOID ServerPortCookie, PVOID ConnectionContext,
                                   ULONG SizeOfContext, PVOID *ConnectionPortCookie);
static VOID FLTAPI PssgDisconnect(PVOID ConnectionCookie);
static NTSTATUS FLTAPI PssgMessage(PVOID PortCookie, PVOID InputBuffer, ULONG InputBufferLength, PVOID OutputBuffer,
                                   ULONG OutputBufferLength, PULONG ReturnOutputBufferLength);

static VOID PssgResolveGuid(PFLT_VOLUME Volume, PPSSG_INSTANCE_CONTEXT Ctx);
static BOOLEAN PssgOnRemovableBus(PCFLT_RELATED_OBJECTS FltObjects, DEVICE_TYPE VolumeDeviceType);

#ifdef ALLOC_PRAGMA
#pragma alloc_text(INIT, DriverEntry)
#pragma alloc_text(PAGE, PssgUnload)
#pragma alloc_text(PAGE, PssgInstanceSetup)
#pragma alloc_text(PAGE, PssgResolveGuid)
#pragma alloc_text(PAGE, PssgOnRemovableBus)
#pragma alloc_text(PAGE, PssgConnect)
#pragma alloc_text(PAGE, PssgDisconnect)
#pragma alloc_text(PAGE, PssgMessage)
#endif

static const FLT_CONTEXT_REGISTRATION g_Contexts[] = {
    { FLT_INSTANCE_CONTEXT, 0, NULL, sizeof(PSSG_INSTANCE_CONTEXT), PSSG_TAG },
    { FLT_CONTEXT_END }
};

static const FLT_OPERATION_REGISTRATION g_Callbacks[] = {
    { IRP_MJ_CREATE, 0, PssgPreCreate, NULL },
    { IRP_MJ_OPERATION_END }
};

static const FLT_REGISTRATION g_Registration = {
    sizeof(FLT_REGISTRATION),
    FLT_REGISTRATION_VERSION,
    0,                 /* Flags: unload allowed */
    g_Contexts,
    g_Callbacks,
    PssgUnload,
    PssgInstanceSetup,
    NULL,              /* InstanceQueryTeardown: allow */
    NULL, NULL,        /* teardown start / complete */
    NULL, NULL, NULL   /* name provider callbacks */
};

/* Exported by the kernel; non-zero in Safe Mode. */
extern PULONG InitSafeBootMode;

/* ---------------------------------------------------------------------------------------- */

static VOID PssgReadParameters(PUNICODE_STRING RegistryPath)
{
    OBJECT_ATTRIBUTES oa;
    UNICODE_STRING sub, name;
    HANDLE key = NULL;
    WCHAR path[256];
    UCHAR buf[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + sizeof(ULONG)];
    ULONG got = 0;
    NTSTATUS st;

    g_ForceFailOpen = FALSE;
    st = RtlStringCbPrintfW(path, sizeof(path), L"%wZ\\Parameters", RegistryPath);
    if (!NT_SUCCESS(st)) {
        return;
    }
    RtlInitUnicodeString(&sub, path);
    InitializeObjectAttributes(&oa, &sub, OBJ_KERNEL_HANDLE | OBJ_CASE_INSENSITIVE, NULL, NULL);
    if (!NT_SUCCESS(ZwOpenKey(&key, KEY_READ, &oa))) {
        return;
    }
    RtlInitUnicodeString(&name, L"FailOpen");
    st = ZwQueryValueKey(key, &name, KeyValuePartialInformation, buf, sizeof(buf), &got);
    if (NT_SUCCESS(st)) {
        PKEY_VALUE_PARTIAL_INFORMATION v = (PKEY_VALUE_PARTIAL_INFORMATION)buf;
        if (v->Type == REG_DWORD && v->DataLength == sizeof(ULONG) && *(ULONG UNALIGNED *)v->Data != 0) {
            g_ForceFailOpen = TRUE;
        }
    }
    ZwClose(key);
}

NTSTATUS DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    NTSTATUS st;
    PSECURITY_DESCRIPTOR sd = NULL;
    OBJECT_ATTRIBUTES oa;
    UNICODE_STRING portName;

    RtlZeroMemory(&g_Active, sizeof(g_Active));
    RtlZeroMemory(&g_Staged, sizeof(g_Staged));
    g_Armed = 0;
    FltInitializePushLock(&g_PlanLock);
    FltInitializePushLock(&g_StageLock);
    PssgReadParameters(RegistryPath);
    if (InitSafeBootMode != NULL && *InitSafeBootMode != 0) {
        g_ForceFailOpen = TRUE;
    }

    st = FltRegisterFilter(DriverObject, &g_Registration, &g_Filter);
    if (!NT_SUCCESS(st)) {
        return st;
    }

    /* Port: SYSTEM + Administrators only. */
    st = FltBuildDefaultSecurityDescriptor(&sd, FLT_PORT_ALL_ACCESS);
    if (NT_SUCCESS(st)) {
        RtlInitUnicodeString(&portName, PSSG_PORT_NAME);
        InitializeObjectAttributes(&oa, &portName, OBJ_KERNEL_HANDLE | OBJ_CASE_INSENSITIVE, NULL, sd);
        st = FltCreateCommunicationPort(g_Filter, &g_ServerPort, &oa, NULL, PssgConnect, PssgDisconnect, PssgMessage, 1);
        FltFreeSecurityDescriptor(sd);
    }
    if (NT_SUCCESS(st)) {
        st = FltStartFiltering(g_Filter);
    }
    if (!NT_SUCCESS(st)) {
        if (g_ServerPort != NULL) {
            FltCloseCommunicationPort(g_ServerPort);
            g_ServerPort = NULL;
        }
        FltUnregisterFilter(g_Filter);
        g_Filter = NULL;
    }
    return st;
}

static NTSTATUS FLTAPI PssgUnload(FLT_FILTER_UNLOAD_FLAGS Flags)
{
    UNREFERENCED_PARAMETER(Flags);
    PAGED_CODE();
    InterlockedExchange(&g_Armed, 0);
    if (g_ServerPort != NULL) {
        FltCloseCommunicationPort(g_ServerPort);
        g_ServerPort = NULL;
    }
    FltUnregisterFilter(g_Filter);
    FltDeletePushLock(&g_PlanLock);
    FltDeletePushLock(&g_StageLock);
    return STATUS_SUCCESS;
}

/* "\??\Volume{GUID}" (or "\\?\Volume{GUID}\") -> "{guid}" lower case. */
static BOOLEAN PssgExtractGuid(PCUNICODE_STRING Name, WCHAR Out[PSSG_VOLUME_GUID_CHARS])
{
    USHORT n = Name->Length / sizeof(WCHAR);
    USHORT i, start = n, len;

    for (i = 0; i < n; i++) {
        if (Name->Buffer[i] == L'{') {
            start = i;
            break;
        }
    }
    if (start == n) {
        return FALSE;
    }
    for (i = start; i < n && Name->Buffer[i] != L'}'; i++) {
    }
    if (i == n) {
        return FALSE;
    }
    len = (USHORT)(i - start + 1);
    if (len >= PSSG_VOLUME_GUID_CHARS) {
        return FALSE;
    }
    RtlZeroMemory(Out, PSSG_VOLUME_GUID_CHARS * sizeof(WCHAR));
    for (i = 0; i < len; i++) {
        WCHAR c = Name->Buffer[start + i];
        Out[i] = (c >= L'A' && c <= L'F') ? (WCHAR)(c - L'A' + L'a') : c;
    }
    return TRUE;
}

/* PASSIVE_LEVEL. Volume GUID name -> ctx->Guid, then HasGuid = 1. */
static VOID PssgResolveGuid(PFLT_VOLUME Volume, PPSSG_INSTANCE_CONTEXT Ctx)
{
    NTSTATUS st;
    /* "\??\Volume{GUID}" is 49 characters; 128 leaves room for any trailing form. A
     * longer name (STATUS_BUFFER_TOO_SMALL) is not a volume GUID name we could match. */
    WCHAR buf[128];
    UNICODE_STRING name;
    ULONG needed = 0;
    WCHAR guid[PSSG_VOLUME_GUID_CHARS];

    PAGED_CODE();
    if (ReadAcquire(&Ctx->HasGuid) != 0) {
        return;
    }
    RtlInitEmptyUnicodeString(&name, buf, sizeof(buf));
    st = FltGetVolumeGuidName(Volume, &name, &needed);
    if (NT_SUCCESS(st) && PssgExtractGuid(&name, guid)) {
        RtlCopyMemory(Ctx->Guid, guid, sizeof(guid));
        InterlockedExchange(&Ctx->HasGuid, 1); /* full barrier: Guid visible first */
    }
}

/* Generic work item (PASSIVE_LEVEL). Filter Manager keeps a rundown reference on the
 * instance while the item is queued; the context reference was taken by the queuer. */
static VOID FLTAPI PssgResolveWork(PFLT_GENERIC_WORKITEM WorkItem, PVOID FltObject, PVOID Context)
{
    PPSSG_INSTANCE_CONTEXT ctx = (PPSSG_INSTANCE_CONTEXT)Context;
    PFLT_VOLUME volume = NULL;

    if (NT_SUCCESS(FltGetVolumeFromInstance((PFLT_INSTANCE)FltObject, &volume))) {
        PssgResolveGuid(volume, ctx);
        FltObjectDereference(volume);
    }
    InterlockedExchange(&ctx->ResolveQueued, 0); /* a later create may retry */
    FltReleaseContext(ctx);
    FltFreeGenericWorkItem(WorkItem);
}

/* From pre-create (FltGetVolumeGuidName is not safe there): queue one lookup at a time. */
static VOID PssgQueueResolve(PCFLT_RELATED_OBJECTS FltObjects, PPSSG_INSTANCE_CONTEXT Ctx)
{
    PFLT_GENERIC_WORKITEM wi;

    if (InterlockedCompareExchange(&Ctx->ResolveQueued, 1, 0) != 0) {
        return;
    }
    wi = FltAllocateGenericWorkItem();
    if (wi == NULL) {
        InterlockedExchange(&Ctx->ResolveQueued, 0);
        return;
    }
    FltReferenceContext(Ctx);
    if (!NT_SUCCESS(FltQueueGenericWorkItem(wi, FltObjects->Instance, PssgResolveWork, DelayedWorkQueue, Ctx))) {
        FltReleaseContext(Ctx);
        FltFreeGenericWorkItem(wi);
        InterlockedExchange(&Ctx->ResolveQueued, 0);
    }
}

/* PASSIVE_LEVEL. Optical, removable media, or a disk on a USB / SD / MMC bus
 * (IOCTL_STORAGE_QUERY_PROPERTY, StorageDeviceProperty). */
static BOOLEAN PssgOnRemovableBus(PCFLT_RELATED_OBJECTS FltObjects, DEVICE_TYPE VolumeDeviceType)
{
    PDEVICE_OBJECT disk = NULL;
    STORAGE_PROPERTY_QUERY q;
    union {
        STORAGE_DEVICE_DESCRIPTOR d;
        UCHAR raw[sizeof(STORAGE_DEVICE_DESCRIPTOR) + 16];
    } out;
    KEVENT ev;
    IO_STATUS_BLOCK iosb;
    PIRP irp;
    NTSTATUS st;
    BOOLEAN removable = FALSE;

    PAGED_CODE();
    if (VolumeDeviceType == FILE_DEVICE_CD_ROM_FILE_SYSTEM) {
        return TRUE;
    }
    if (!NT_SUCCESS(FltGetDiskDeviceObject(FltObjects->Volume, &disk))) {
        return FALSE;
    }
    if (FlagOn(disk->Characteristics, FILE_REMOVABLE_MEDIA)) {
        removable = TRUE;
    } else {
        RtlZeroMemory(&q, sizeof(q));
        RtlZeroMemory(&out, sizeof(out));
        q.PropertyId = StorageDeviceProperty;
        q.QueryType = PropertyStandardQuery;
        KeInitializeEvent(&ev, NotificationEvent, FALSE);
        irp = IoBuildDeviceIoControlRequest(IOCTL_STORAGE_QUERY_PROPERTY, disk, &q, sizeof(q), &out, sizeof(out), FALSE, &ev,
                                            &iosb);
        if (irp != NULL) {
            st = IoCallDriver(disk, irp);
            if (st == STATUS_PENDING) {
                KeWaitForSingleObject(&ev, Executive, KernelMode, FALSE, NULL);
                st = iosb.Status;
            }
            if (NT_SUCCESS(st) &&
                iosb.Information >= FIELD_OFFSET(STORAGE_DEVICE_DESCRIPTOR, BusType) + sizeof(STORAGE_BUS_TYPE)) {
                STORAGE_BUS_TYPE bt = out.d.BusType;
                removable = (bt == BusTypeUsb || bt == BusTypeSd || bt == BusTypeMmc);
            }
        }
    }
    ObDereferenceObject(disk);
    return removable;
}

static NTSTATUS FLTAPI PssgInstanceSetup(PCFLT_RELATED_OBJECTS FltObjects, FLT_INSTANCE_SETUP_FLAGS Flags,
                                         DEVICE_TYPE VolumeDeviceType, FLT_FILESYSTEM_TYPE VolumeFilesystemType)
{
    NTSTATUS st;
    PPSSG_INSTANCE_CONTEXT ctx = NULL;

    UNREFERENCED_PARAMETER(Flags);
    PAGED_CODE();

    /* Local disk and optical file systems only. */
    if (VolumeDeviceType == FILE_DEVICE_NETWORK_FILE_SYSTEM || VolumeFilesystemType == FLT_FSTYPE_RAW) {
        return STATUS_FLT_DO_NOT_ATTACH;
    }

    st = FltAllocateContext(FltObjects->Filter, FLT_INSTANCE_CONTEXT, sizeof(PSSG_INSTANCE_CONTEXT), NonPagedPoolNx,
                            (PFLT_CONTEXT *)&ctx);
    if (!NT_SUCCESS(st)) {
        return STATUS_FLT_DO_NOT_ATTACH; /* fail open: no context, no gating on this volume */
    }
    RtlZeroMemory(ctx, sizeof(*ctx));
    ctx->Removable = PssgOnRemovableBus(FltObjects, VolumeDeviceType);
    /* May fail for a volume mounted before the mount manager published its GUID link;
     * pre-create then retries from a work item. */
    PssgResolveGuid(FltObjects->Volume, ctx);
    st = FltSetInstanceContext(FltObjects->Instance, FLT_SET_CONTEXT_KEEP_IF_EXISTS, ctx, NULL);
    FltReleaseContext(ctx);
    if (!NT_SUCCESS(st) && st != STATUS_FLT_CONTEXT_ALREADY_DEFINED) {
        return STATUS_FLT_DO_NOT_ATTACH;
    }
    return STATUS_SUCCESS;
}

/* A volume GUID as PssgExtractGuid stores it: exactly "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}"
 * (38 characters), hex lower-cased in place, every character after it NUL. FALSE (and
 * nothing to rely on in Guid) for any other content. */
static BOOLEAN PssgNormalizeGuid(WCHAR Guid[PSSG_VOLUME_GUID_CHARS])
{
    ULONG i;

    for (i = 0; i < 38; i++) {
        WCHAR c = Guid[i];
        if (i == 0) {
            if (c != L'{') {
                return FALSE;
            }
        } else if (i == 37) {
            if (c != L'}') {
                return FALSE;
            }
        } else if (i == 9 || i == 14 || i == 19 || i == 24) {
            if (c != L'-') {
                return FALSE;
            }
        } else if (c >= L'A' && c <= L'F') {
            Guid[i] = (WCHAR)(c - L'A' + L'a');
        } else if (!((c >= L'0' && c <= L'9') || (c >= L'a' && c <= L'f'))) {
            return FALSE;
        }
    }
    for (i = 38; i < PSSG_VOLUME_GUID_CHARS; i++) {
        if (Guid[i] != L'\0') {
            return FALSE;
        }
    }
    return TRUE;
}

/* Under g_PlanLock (shared). */
static BOOLEAN PssgVolumeMask(const WCHAR Guid[PSSG_VOLUME_GUID_CHARS], ULONG *Mask)
{
    ULONG i;
    for (i = 0; i < g_Active.VolumeCount; i++) {
        if (RtlEqualMemory(g_Active.Volumes[i].Guid, Guid, PSSG_VOLUME_GUID_CHARS * sizeof(WCHAR))) {
            *Mask = g_Active.Volumes[i].SeatMask;
            return TRUE;
        }
    }
    return FALSE;
}

static ULONG PssgSeatBit(ULONG SessionId)
{
    ULONG i;
    for (i = 0; i < g_Active.SessionCount; i++) {
        if (g_Active.Sessions[i].SessionId == SessionId) {
            return g_Active.Sessions[i].SeatBit;
        }
    }
    return 0;
}

/* LocalSystem, LocalService, NetworkService, or a per-service virtual account
 * (NT SERVICE\<name> = S-1-5-80-...). No user's own code runs under these. */
static BOOLEAN PssgIsServiceSid(PSID Sid)
{
    SID_IDENTIFIER_AUTHORITY nt = SECURITY_NT_AUTHORITY;

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

/* The effective token of the create (the client token when impersonating) belongs to a
 * service account, or, with AdminsBypass, is an administrator token. Needs PASSIVE_LEVEL
 * (SeQueryInformationToken); pre-create can arrive at APC_LEVEL (a create issued under a
 * fast mutex), and then the requestor is not exempt: only gated volumes get here, so this
 * denies, never opens. The subject context is locked while its token is used
 * (SeQuerySubjectContextToken, Microsoft Learn). */
static BOOLEAN PssgRequestorIsExempt(PFLT_CALLBACK_DATA Data, BOOLEAN AdminsBypass)
{
    PIO_SECURITY_CONTEXT sc = Data->Iopb->Parameters.Create.SecurityContext;
    PSECURITY_SUBJECT_CONTEXT subject;
    PACCESS_TOKEN token;
    PTOKEN_USER user = NULL;
    BOOLEAN exempt = FALSE;

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return FALSE;
    }
    if (sc == NULL || sc->AccessState == NULL) {
        return FALSE;
    }
    subject = &sc->AccessState->SubjectSecurityContext;
    SeLockSubjectContext(subject);
    token = SeQuerySubjectContextToken(subject);
    if (token != NULL) {
        if (NT_SUCCESS(SeQueryInformationToken(token, TokenUser, (PVOID *)&user)) && user != NULL) {
            exempt = PssgIsServiceSid(user->User.Sid);
            ExFreePool(user);
        }
        if (!exempt && AdminsBypass) {
            exempt = SeTokenIsAdmin(token);
        }
    }
    SeUnlockSubjectContext(subject);
    return exempt;
}

static FLT_PREOP_CALLBACK_STATUS FLTAPI PssgPreCreate(PFLT_CALLBACK_DATA Data, PCFLT_RELATED_OBJECTS FltObjects,
                                                       PVOID *CompletionContext)
{
    PPSSG_INSTANCE_CONTEXT ctx = NULL;
    ULONG session = MAXULONG, mask = 0, bit = 0, flags;
    BOOLEAN hasGuid, known = FALSE, gated, unresolvedRemovable, anyVolumes;

    *CompletionContext = NULL;

    if (g_ForceFailOpen || ReadAcquire(&g_Armed) == 0) {
        return FLT_PREOP_SUCCESS_NO_CALLBACK;
    }
    if (FlagOn(Data->Iopb->OperationFlags, SL_OPEN_PAGING_FILE)) {
        return FLT_PREOP_SUCCESS_NO_CALLBACK;
    }
    /* Kernel components opening for themselves pass. A kernel create with
     * SL_FORCE_ACCESS_CHECK (IoCreateFileEx + IO_FORCE_ACCESS_CHECK: the SMB server, or
     * any driver acting for a user) is judged like a user create: the subject context then
     * carries the impersonated client's token. */
    if (Data->RequestorMode == KernelMode && !FlagOn(Data->Iopb->OperationFlags, SL_FORCE_ACCESS_CHECK)) {
        return FLT_PREOP_SUCCESS_NO_CALLBACK;
    }
    if (!NT_SUCCESS(FltGetInstanceContext(FltObjects->Instance, (PFLT_CONTEXT *)&ctx))) {
        return FLT_PREOP_SUCCESS_NO_CALLBACK; /* not attached with a context: fail open */
    }
    hasGuid = ReadAcquire(&ctx->HasGuid) != 0;
    if (!hasGuid) {
        PssgQueueResolve(FltObjects, ctx);
    }
    if (!NT_SUCCESS(FltGetRequestorSessionId(Data, &session))) {
        session = MAXULONG; /* maps to no seat */
    }

    FltAcquirePushLockShared(&g_PlanLock);
    if (hasGuid) {
        known = PssgVolumeMask(ctx->Guid, &mask);
    }
    bit = PssgSeatBit(session);
    flags = g_Active.Flags;
    anyVolumes = g_Active.VolumeCount > 0;
    FltReleasePushLock(&g_PlanLock);
    unresolvedRemovable = !hasGuid && ctx->Removable && anyVolumes;
    FltReleaseContext(ctx);

    gated = (known && mask != PSSG_SHARED_MASK && (mask & bit) == 0) || unresolvedRemovable;
    if (!gated || PssgRequestorIsExempt(Data, FlagOn(flags, PSSG_FLAG_ADMINS_BYPASS) != 0)) {
        return FLT_PREOP_SUCCESS_NO_CALLBACK;
    }
    Data->IoStatus.Status = STATUS_ACCESS_DENIED;
    Data->IoStatus.Information = 0;
    return FLT_PREOP_COMPLETE;
}

/* ---------------------------------------------------------------------------------------- */

static NTSTATUS FLTAPI PssgConnect(PFLT_PORT ClientPort, PVOID ServerPortCookie, PVOID ConnectionContext,
                                   ULONG SizeOfContext, PVOID *ConnectionPortCookie)
{
    UNREFERENCED_PARAMETER(ServerPortCookie);
    UNREFERENCED_PARAMETER(ConnectionContext);
    UNREFERENCED_PARAMETER(SizeOfContext);
    PAGED_CODE();
    g_ClientPort = ClientPort; /* max connections = 1 */
    *ConnectionPortCookie = NULL;
    return STATUS_SUCCESS;
}

static VOID FLTAPI PssgDisconnect(PVOID ConnectionCookie)
{
    UNREFERENCED_PARAMETER(ConnectionCookie);
    PAGED_CODE();
    /* Keep the active plan: a service restart must not open other seats' volumes. */
    FltCloseClientPort(g_Filter, &g_ClientPort);
}

static NTSTATUS PssgReply(PVOID Out, ULONG OutLen, PULONG Ret, NTSTATUS Status)
{
    if (Out == NULL || OutLen < sizeof(INT32)) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    __try {
        *(INT32 UNALIGNED *)Out = (INT32)Status;
        *Ret = sizeof(INT32);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return GetExceptionCode();
    }
    return STATUS_SUCCESS;
}

static NTSTATUS FLTAPI PssgMessage(PVOID PortCookie, PVOID InputBuffer, ULONG InputBufferLength, PVOID OutputBuffer,
                                   ULONG OutputBufferLength, PULONG ReturnOutputBufferLength)
{
    UCHAR msg[sizeof(PSSG_SET_VOLUME)];
    PSSG_HEADER *h = (PSSG_HEADER *)msg;
    ULONG len;
    NTSTATUS result;

    UNREFERENCED_PARAMETER(PortCookie);
    PAGED_CODE();
    *ReturnOutputBufferLength = 0;

    if (InputBuffer == NULL || InputBufferLength < sizeof(PSSG_HEADER)) {
        return STATUS_INVALID_PARAMETER;
    }
    len = min(InputBufferLength, (ULONG)sizeof(msg));
    RtlZeroMemory(msg, sizeof(msg));
    /* The buffers are user-mode memory of the caller: copy under SEH. */
    __try {
        RtlCopyMemory(msg, InputBuffer, len);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return GetExceptionCode();
    }
    if (h->Magic != PSSG_MAGIC || h->Size != InputBufferLength) {
        return PssgReply(OutputBuffer, OutputBufferLength, ReturnOutputBufferLength, STATUS_INVALID_PARAMETER);
    }

    if (h->Command == PSSG_CMD_QUERY_VERSION) {
        PSSG_VERSION_REPLY v = { PSSG_INTERFACE_MAJOR, PSSG_INTERFACE_MINOR, PSSG_BUILD };
        if (OutputBuffer == NULL || OutputBufferLength < sizeof(v)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        __try {
            RtlCopyMemory(OutputBuffer, &v, sizeof(v));
            *ReturnOutputBufferLength = sizeof(v);
        } __except (EXCEPTION_EXECUTE_HANDLER) {
            return GetExceptionCode();
        }
        return STATUS_SUCCESS;
    }
    if (h->Major != PSSG_INTERFACE_MAJOR) {
        return PssgReply(OutputBuffer, OutputBufferLength, ReturnOutputBufferLength, STATUS_REVISION_MISMATCH);
    }

    result = STATUS_INVALID_PARAMETER;
    FltAcquirePushLockExclusive(&g_StageLock);
    switch (h->Command) {
    case PSSG_CMD_BEGIN_PLAN:
        if (InputBufferLength != sizeof(PSSG_BEGIN_PLAN)) {
            break;
        }
        RtlZeroMemory(&g_Staged, sizeof(g_Staged));
        g_Staged.Flags = ((PSSG_BEGIN_PLAN *)msg)->Flags;
        result = STATUS_SUCCESS;
        break;
    case PSSG_CMD_SET_SESSION: {
        PSSG_SET_SESSION *m = (PSSG_SET_SESSION *)msg;
        if (InputBufferLength != sizeof(PSSG_SET_SESSION)) {
            break;
        }
        if (g_Staged.SessionCount >= PSSG_MAX_SESSIONS) {
            result = STATUS_INSUFFICIENT_RESOURCES;
            break;
        }
        g_Staged.Sessions[g_Staged.SessionCount].SessionId = m->SessionId;
        g_Staged.Sessions[g_Staged.SessionCount].SeatBit = m->SeatBit;
        g_Staged.SessionCount++;
        result = STATUS_SUCCESS;
        break;
    }
    case PSSG_CMD_SET_VOLUME: {
        PSSG_SET_VOLUME *m = (PSSG_SET_VOLUME *)msg;
        if (InputBufferLength != sizeof(PSSG_SET_VOLUME)) {
            break;
        }
        if (g_Staged.VolumeCount >= PSSG_MAX_VOLUMES) {
            result = STATUS_INSUFFICIENT_RESOURCES;
            break;
        }
        /* Enforce the documented form, so the RtlEqualMemory match in PssgVolumeMask can
         * never silently miss: "{8-4-4-4-12}" hex, lower-cased here, then NUL padding. */
        if (!PssgNormalizeGuid(m->VolumeGuid)) {
            result = STATUS_INVALID_PARAMETER;
            break;
        }
        g_Staged.Volumes[g_Staged.VolumeCount].SeatMask = m->SeatMask;
        RtlCopyMemory(g_Staged.Volumes[g_Staged.VolumeCount].Guid, m->VolumeGuid, sizeof(m->VolumeGuid));
        g_Staged.VolumeCount++;
        result = STATUS_SUCCESS;
        break;
    }
    case PSSG_CMD_COMMIT_PLAN:
        if (InputBufferLength != sizeof(PSSG_HEADER)) {
            break;
        }
        FltAcquirePushLockExclusive(&g_PlanLock);
        RtlCopyMemory(&g_Active, &g_Staged, sizeof(g_Active));
        FltReleasePushLock(&g_PlanLock);
        InterlockedExchange(&g_Armed, 1);
        result = STATUS_SUCCESS;
        break;
    case PSSG_CMD_PASS_ALL:
        if (InputBufferLength != sizeof(PSSG_HEADER)) {
            break;
        }
        InterlockedExchange(&g_Armed, 0);
        result = STATUS_SUCCESS;
        break;
    default:
        break;
    }
    FltReleasePushLock(&g_StageLock);
    return PssgReply(OutputBuffer, OutputBufferLength, ReturnOutputBufferLength, result);
}
