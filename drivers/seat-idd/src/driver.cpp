/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat IddCx driver, local role: device / adapter / monitor callbacks and the control
 * IOCTLs. See driver.h. Built unsigned (WDK 10.0.28000), never loaded, untested on
 * hardware.
 */
#include "driver.h"

static const GUID PsiddInterfaceGuid = PSIDD_INTERFACE_GUID_INIT;

/* Device contexts of this host process, for ParseMonitorDescription(2), which gets no
 * object handle. The INF asks for one WUDFHost per devnode (ProcessSharingDisabled), but
 * with one adapter per render GPU (minor 3) several local devnodes exist, and nothing here
 * relies on them living in different processes: every lookup scans the registered
 * devices. Slot numbers (seat, connector) are unique across adapters (the service plugs a
 * seat output on one adapter only), and so are the patched EDIDs. */
static SRWLOCK g_DevicesLock = SRWLOCK_INIT;
static PSIDD_DEVICE_CONTEXT *g_Devices[PSIDD_MAX_DEVICES];

static BOOL RegisterDevice(PSIDD_DEVICE_CONTEXT *ctx)
{
    BOOL ok = FALSE;
    AcquireSRWLockExclusive(&g_DevicesLock);
    for (UINT i = 0; i < PSIDD_MAX_DEVICES; i++) {
        if (g_Devices[i] == NULL) {
            g_Devices[i] = ctx;
            ok = TRUE;
            break;
        }
    }
    ReleaseSRWLockExclusive(&g_DevicesLock);
    return ok;
}

static void UnregisterDevice(WDFDEVICE device)
{
    AcquireSRWLockExclusive(&g_DevicesLock);
    for (UINT i = 0; i < PSIDD_MAX_DEVICES; i++) {
        if (g_Devices[i] != NULL && g_Devices[i]->Device == device) {
            g_Devices[i] = NULL;
        }
    }
    ReleaseSRWLockExclusive(&g_DevicesLock);
}

static BOOL LuidEq(LUID a, LUID b)
{
    return a.LowPart == b.LowPart && a.HighPart == b.HighPart;
}

static LUID LuidFrom(uint64_t v)
{
    LUID l;
    l.LowPart = (DWORD)(v & 0xffffffffu);
    l.HighPart = (LONG)(v >> 32);
    return l;
}

static uint64_t LuidTo(LUID l)
{
    return ((uint64_t)(uint32_t)l.HighPart << 32) | l.LowPart;
}

/* DriverEntry and the role choice (local / remote) are in roles.cpp. */

NTSTATUS PsiddDeviceAdd(WDFDRIVER Driver, PWDFDEVICE_INIT DeviceInit)
{
    NTSTATUS status;
    WDF_PNPPOWER_EVENT_CALLBACKS pnp;
    IDD_CX_CLIENT_CONFIG cfg;
    WDF_OBJECT_ATTRIBUTES attr;
    WDFDEVICE device = NULL;
    PSIDD_DEVICE_CONTEXT *ctx;

    UNREFERENCED_PARAMETER(Driver);

    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp);
    pnp.EvtDeviceD0Entry = PsiddD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnp);

    IDD_CX_CLIENT_CONFIG_INIT(&cfg);
    cfg.EvtIddCxDeviceIoControl = PsiddIoControl;
    cfg.EvtIddCxAdapterInitFinished = PsiddAdapterInitFinished;
    cfg.EvtIddCxAdapterCommitModes = PsiddCommitModes;
    cfg.EvtIddCxParseMonitorDescription = PsiddParseMonitorDescription;
    cfg.EvtIddCxMonitorGetDefaultDescriptionModes = PsiddGetDefaultModes;
    cfg.EvtIddCxMonitorQueryTargetModes = PsiddQueryTargetModes;
    cfg.EvtIddCxMonitorAssignSwapChain = PsiddAssignSwapChain;
    cfg.EvtIddCxMonitorUnassignSwapChain = PsiddUnassignSwapChain;
    /* IddCx 1.10 callbacks (HDR / wide colour, minor 3): only the fields the running IddCx
     * knows ("Supporting a 1.10 driver running down level"). */
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterQueryTargetInfo)) {
        cfg.EvtIddCxAdapterQueryTargetInfo = PsiddQueryTargetInfo;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterCommitModes2)) {
        cfg.EvtIddCxAdapterCommitModes2 = PsiddCommitModes2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxParseMonitorDescription2)) {
        cfg.EvtIddCxParseMonitorDescription2 = PsiddParseMonitorDescription2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorQueryTargetModes2)) {
        cfg.EvtIddCxMonitorQueryTargetModes2 = PsiddQueryTargetModes2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorSetDefaultHdrMetaData)) {
        cfg.EvtIddCxMonitorSetDefaultHdrMetaData = PsiddSetDefaultHdrMetaData;
    }

    status = IddCxDeviceInitConfig(DeviceInit, &cfg);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, PSIDD_DEVICE_CONTEXT);
    attr.EvtCleanupCallback = PsiddDeviceCleanup;
    status = WdfDeviceCreate(&DeviceInit, &attr, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    ctx = PsiddGetDeviceContext(device);
    RtlZeroMemory(ctx, sizeof(*ctx));
    ctx->Device = device;
    InitializeSRWLock(&ctx->Lock);
    for (UINT i = 0; i < PSIDD_MAX_MONITORS; i++) {
        InitializeSRWLock(&ctx->SwapLocks[i]);
    }
    if (!RegisterDevice(ctx)) {
        return STATUS_INSUFFICIENT_RESOURCES; /* more local adapters than PSIDD_MAX_DEVICES in one host */
    }

    status = IddCxDeviceInitialize(device);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    /* Control interface. Its ACL is the devnode's Security value from the INF (SYSTEM). */
    return WdfDeviceCreateDeviceInterface(device, &PsiddInterfaceGuid, NULL);
}

VOID PsiddDeviceCleanup(WDFOBJECT Object)
{
    /* Device removal: ParseMonitorDescription must not see a freed context. */
    UnregisterDevice((WDFDEVICE)Object);
}

NTSTATUS PsiddD0Entry(WDFDEVICE Device, WDF_POWER_DEVICE_STATE PreviousState)
{
    PSIDD_DEVICE_CONTEXT *ctx = PsiddGetDeviceContext(Device);
    IDDCX_ADAPTER_CAPS caps;
    IDARG_IN_ADAPTER_INIT in;
    IDARG_OUT_ADAPTER_INIT out;
    WDF_OBJECT_ATTRIBUTES attr;
    IDDCX_ENDPOINT_VERSION version;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(PreviousState);
    if (ctx->Adapter != NULL) {
        return STATUS_SUCCESS; /* adapter survives D0 transitions */
    }
    {
        IDARG_OUT_GETVERSION ver;
        RtlZeroMemory(&ver, sizeof(ver));
        if (NT_SUCCESS(IddCxGetVersion(&ver))) {
            ctx->IddCxVersion = ver.IddCxVersion;
        }
    }
    /* IddCx 1.10 at run time: FP16 swap chains + the HDR callbacks (set in DeviceAdd). */
    ctx->Has110 = IDD_IS_FUNCTION_AVAILABLE(IddCxSwapChainReleaseAndAcquireBuffer2) &&
                  IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterQueryTargetInfo) &&
                  IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterCommitModes2);

    RtlZeroMemory(&version, sizeof(version));
    version.Size = sizeof(version);
    version.MajorVer = PSIDD_INTERFACE_MAJOR;
    version.MinorVer = PSIDD_INTERFACE_MINOR;
    version.Build = PSIDD_BUILD;

    RtlZeroMemory(&caps, sizeof(caps));
    caps.Size = sizeof(caps);
    caps.MaxMonitorsSupported = PSIDD_ADAPTER_MAX_MONITORS; /* IddCx limit, see the header */
    /* With 1.10 the adapter takes FP16 surfaces (needed for HDR / SDR wide colour). The
     * header asks for this flag even when no monitor is HDR, so every swap chain may hand
     * FP16: HDR monitors get the FP16 pixels, the others a BGRA8 conversion
     * (swapchain.cpp). */
    if (ctx->Has110) {
        caps.Flags = IDDCX_ADAPTER_FLAGS_CAN_PROCESS_FP16;
    }
    caps.EndPointDiagnostics.Size = sizeof(caps.EndPointDiagnostics);
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName = L"Pulsar Seat";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"Pulsar";
    caps.EndPointDiagnostics.pEndPointModelName = L"Seat virtual display";
    caps.EndPointDiagnostics.pFirmwareVersion = &version;
    caps.EndPointDiagnostics.pHardwareVersion = &version;

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, PSIDD_ADAPTER_CONTEXT);

    RtlZeroMemory(&in, sizeof(in));
    in.WdfDevice = Device;
    in.pCaps = &caps;
    in.ObjectAttributes = &attr;

    /* EvtIddCxAdapterInitFinished may run on another thread before IddCxAdapterInitAsync
     * returns, so it never relies on the adapter context being set (see below). */
    status = IddCxAdapterInitAsync(&in, &out);
    if (NT_SUCCESS(status)) {
        AcquireSRWLockExclusive(&ctx->Lock);
        ctx->Adapter = out.AdapterObject;
        ReleaseSRWLockExclusive(&ctx->Lock);
        PsiddGetAdapterContext(out.AdapterObject)->Device = ctx;
    }
    return status;
}

/* Device context of an adapter. D0Entry stores it right after IddCxAdapterInitAsync
 * returns; a callback that runs earlier waits for it (at most 2 s), then also accepts the
 * one registered device whose adapter is still unknown. */
static PSIDD_DEVICE_CONTEXT *AdapterDevice(IDDCX_ADAPTER Adapter)
{
    ULONGLONG start = GetTickCount64();
    for (;;) {
        PSIDD_DEVICE_CONTEXT *ctx = PsiddGetAdapterContext(Adapter)->Device;
        if (ctx != NULL) {
            return ctx;
        }
        if (GetTickCount64() - start >= 2000) {
            break;
        }
        Sleep(1);
    }
    {
        PSIDD_DEVICE_CONTEXT *only = NULL;
        UINT n = 0;
        AcquireSRWLockShared(&g_DevicesLock);
        for (UINT i = 0; i < PSIDD_MAX_DEVICES; i++) {
            if (g_Devices[i] != NULL && (g_Devices[i]->Adapter == NULL || g_Devices[i]->Adapter == Adapter)) {
                only = g_Devices[i];
                n++;
            }
        }
        ReleaseSRWLockShared(&g_DevicesLock);
        return n == 1 ? only : NULL;
    }
}

NTSTATUS PsiddAdapterInitFinished(IDDCX_ADAPTER Adapter, const IDARG_IN_ADAPTER_INIT_FINISHED *pInArgs)
{
    /* Can run before D0Entry stored the adapter context (the context memory is zeroed by
     * WDF). Several local adapters may initialise at once (one per render GPU), so there is
     * no single device to fall back to: wait (bounded) for D0Entry to store it. */
    PSIDD_DEVICE_CONTEXT *ctx = AdapterDevice(Adapter);
    if (ctx == NULL) {
        return STATUS_SUCCESS; /* device already gone, or IddCxAdapterInitAsync never returned */
    }
    AcquireSRWLockExclusive(&ctx->Lock);
    ctx->Adapter = Adapter; /* also covers the race with D0Entry */
    ctx->AdapterReady = NT_SUCCESS(pInArgs->AdapterInitStatus);
    ReleaseSRWLockExclusive(&ctx->Lock);
    /* Monitors are plugged only on request of the seat service. */
    return STATUS_SUCCESS;
}

NTSTATUS PsiddCommitModes(IDDCX_ADAPTER Adapter, const IDARG_IN_COMMITMODES *pInArgs)
{
    UNREFERENCED_PARAMETER(Adapter);
    UNREFERENCED_PARAMETER(pInArgs);
    /* No hardware to program: the mode is whatever the swap chain delivers. */
    return STATUS_SUCCESS;
}

/* ---- modes ------------------------------------------------------------------------- */

/* monitorMode: a monitor mode (ParseMonitorDescription / default modes) has
 * vSyncFreqDivider 0, a target mode 1. With 1 in the monitor modes IddCxMonitorArrival
 * failed with STATUS_INVALID_PARAMETER (test VM 2026-11-16); the WDK IndirectDisplay
 * sample, read as documentation, sets the same values. */
DISPLAYCONFIG_VIDEO_SIGNAL_INFO PsiddSignalInfo(const PSIDD_MODE *mode, BOOL monitorMode)
{
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO s;
    uint32_t mhz = mode->RefreshMilliHz ? mode->RefreshMilliHz : 60000;
    /* hSync = vSync * lines. Modes are validated (<= PSIDD_MAX_MODE_DIM lines,
     * <= PSIDD_MAX_REFRESH_MHZ), so the product fits 64 bits; scale it into 32 bits. */
    uint64_t hNum = (uint64_t)mhz * mode->Height;
    uint32_t hDen = 1000;

    while (hNum > 0xffffffffull && hDen > 1) {
        hNum /= 10;
        hDen /= 10;
    }
    RtlZeroMemory(&s, sizeof(s));
    s.totalSize.cx = s.activeSize.cx = mode->Width;
    s.totalSize.cy = s.activeSize.cy = mode->Height;
    s.AdditionalSignalInfo.vSyncFreqDivider = monitorMode ? 0 : 1;
    s.AdditionalSignalInfo.videoStandard = 255;
    s.vSyncFreq.Numerator = mhz;
    s.vSyncFreq.Denominator = 1000;
    s.hSyncFreq.Numerator = (UINT32)hNum;
    s.hSyncFreq.Denominator = hDen;
    s.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    s.pixelRate = ((uint64_t)mhz * mode->Width * mode->Height) / 1000;
    return s;
}

/* Mode list copied out of a slot, so ParseMonitorDescription never reads a slot after it
 * dropped the lock. Count 0 = the default mode list. */
typedef struct _PSIDD_MODE_SNAPSHOT {
    PSIDD_MODE Modes[PSIDD_MAX_MODES];
    uint32_t Count;
    BOOL Hdr; /* the monitor offers HDR / wide colour (PSIDD_ADD_FLAG_HDR on a 1.10 adapter) */
} PSIDD_MODE_SNAPSHOT;

/* HDR offered for a monitor: asked for at add time and possible on this adapter. */
static BOOL MonitorHdr(const PSIDD_DEVICE_CONTEXT *ctx, const PSIDD_MONITOR *m)
{
    return ctx->Has110 && (m->Flags & PSIDD_ADD_FLAG_HDR) != 0;
}

static void Snapshot(const PSIDD_DEVICE_CONTEXT *ctx, const PSIDD_MONITOR *m, PSIDD_MODE_SNAPSHOT *snap)
{
    uint32_t n = m->ModeCount <= PSIDD_MAX_MODES ? m->ModeCount : PSIDD_MAX_MODES;
    memcpy(snap->Modes, m->Modes, sizeof(PSIDD_MODE) * n);
    snap->Count = n;
    snap->Hdr = MonitorHdr(ctx, m);
}

static BOOL EdidMatches(const PSIDD_MONITOR *m, const IDDCX_MONITOR_DESCRIPTION *d)
{
    return d->DataSize != 0 && d->pData != NULL && d->DataSize == m->EdidLength &&
           memcmp(d->pData, m->Edid, d->DataSize) == 0;
}

/* Reverse lookup for ParseMonitorDescription (no monitor handle). Caller holds ctx->Lock
 * (shared). EDIDs are unique per slot (PatchEdidSerial); an empty or still ambiguous
 * description gets no monitor, i.e. the default mode list, never another seat's modes. */
static PSIDD_MONITOR *FindByEdid(PSIDD_DEVICE_CONTEXT *ctx, const IDDCX_MONITOR_DESCRIPTION *d)
{
    PSIDD_MONITOR *found = NULL;
    uint32_t i;
    for (i = 0; i < PSIDD_MAX_MONITORS; i++) {
        PSIDD_MONITOR *m = &ctx->Monitors[i];
        if (m->InUse && !m->Removing && EdidMatches(m, d)) {
            if (found != NULL) {
                return NULL;
            }
            found = m;
        }
    }
    return found;
}

/* How long Parse waits for an AddMonitor / RemoveMonitor that holds the lock exclusively
 * before it answers with the default modes (it never blocks forever). */
#define PSIDD_PARSE_WAIT_MS 2000u

/* The modes for a description, without racing AddMonitor:
 *  - on the adding thread (IddCxMonitorArrival calls back into Parse there) the pending
 *    slot is used directly: that thread owns the exclusive lock;
 *  - on any other thread the slots are scanned under the shared lock. The lock is only
 *    tried, never waited on blindly: while an add holds it, a description equal to the
 *    pending slot's EDID is answered from that slot (complete and unchanged until the add
 *    returns), so even an arrival that waited for a parse on another thread (not what the
 *    IddCx docs describe, but not excluded by them) cannot deadlock. */
static void ModesForDescription(PSIDD_DEVICE_CONTEXT *ctx, const IDDCX_MONITOR_DESCRIPTION *d, PSIDD_MODE_SNAPSHOT *snap)
{
    ULONGLONG start = GetTickCount64();
    PSIDD_MONITOR *pending;

    snap->Count = 0;
    snap->Hdr = FALSE;
    pending = ctx->Pending;
    if (pending != NULL && ctx->PendingThread == GetCurrentThreadId()) {
        Snapshot(ctx, pending, snap);
        return;
    }
    for (;;) {
        if (TryAcquireSRWLockShared(&ctx->Lock)) {
            PSIDD_MONITOR *m = FindByEdid(ctx, d);
            if (m != NULL) {
                Snapshot(ctx, m, snap);
            }
            ReleaseSRWLockShared(&ctx->Lock);
            return;
        }
        pending = ctx->Pending;
        if (pending != NULL && EdidMatches(pending, d)) {
            Snapshot(ctx, pending, snap);
            return;
        }
        if (GetTickCount64() - start >= PSIDD_PARSE_WAIT_MS) {
            return; /* default modes; the OS asks the monitor again once it exists */
        }
        Sleep(1);
    }
}

/* Make the EDID unique per slot: serial number field (base block bytes 12..15) =
 * slot (little endian, 2 bytes) + "SP", then recompute the base-block checksum (byte 127).
 * Only for a well-formed base block (header 00 FF FF FF FF FF FF 00). */
static void PatchEdidSerial(PSIDD_MONITOR *m, uint32_t slot)
{
    static const uint8_t header[8] = { 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00 };
    uint8_t sum = 0;
    uint32_t i;
    if (m->EdidLength < 128 || memcmp(m->Edid, header, 8) != 0) {
        return;
    }
    m->Edid[12] = (uint8_t)(slot & 0xff);
    m->Edid[13] = (uint8_t)((slot >> 8) & 0xff);
    m->Edid[14] = 'S';
    m->Edid[15] = 'P';
    for (i = 0; i < 127; i++) {
        sum = (uint8_t)(sum + m->Edid[i]);
    }
    m->Edid[127] = (uint8_t)(0x100 - sum);
}

static BOOL ModesValid(const PSIDD_MODE *modes, uint32_t count)
{
    uint32_t i;
    for (i = 0; i < count; i++) {
        if (modes[i].Width == 0 || modes[i].Height == 0 || modes[i].Width > PSIDD_MAX_MODE_DIM ||
            modes[i].Height > PSIDD_MAX_MODE_DIM || modes[i].RefreshMilliHz > PSIDD_MAX_REFRESH_MHZ) {
            return FALSE;
        }
    }
    return TRUE;
}

static const PSIDD_MODE DefaultMode = { 1920, 1080, 60000 };

static NTSTATUS FillMonitorModes(const PSIDD_MODE *list, uint32_t listCount, UINT inCount, IDDCX_MONITOR_MODE *out,
                                 UINT *outCount)
{
    const PSIDD_MODE *modes = listCount ? list : &DefaultMode;
    UINT n = listCount ? listCount : 1;
    UINT i;
    *outCount = n;
    if (inCount == 0) {
        return STATUS_SUCCESS; /* size query */
    }
    if (inCount < n) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    for (i = 0; i < n; i++) {
        out[i].Size = sizeof(IDDCX_MONITOR_MODE);
        out[i].Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
        out[i].MonitorVideoSignalInfo = PsiddSignalInfo(&modes[i], TRUE);
    }
    return STATUS_SUCCESS;
}

/* The modes of a description on whichever registered adapter owns it. The adding thread's
 * own adapter is asked first (its arrival calls back into Parse); then the others. A
 * description no adapter knows gets the default mode. */
static void ModesForAnyDevice(const IDDCX_MONITOR_DESCRIPTION *d, PSIDD_MODE_SNAPSHOT *snap)
{
    PSIDD_DEVICE_CONTEXT *devs[PSIDD_MAX_DEVICES];
    UINT n = 0;
    DWORD me = GetCurrentThreadId();

    snap->Count = 0;
    snap->Hdr = FALSE;
    AcquireSRWLockShared(&g_DevicesLock);
    for (UINT i = 0; i < PSIDD_MAX_DEVICES; i++) {
        if (g_Devices[i] != NULL) {
            devs[n++] = g_Devices[i];
        }
    }
    ReleaseSRWLockShared(&g_DevicesLock);
    for (UINT i = 0; i < n; i++) {
        if (devs[i]->Pending != NULL && devs[i]->PendingThread == me) {
            ModesForDescription(devs[i], d, snap);
            return;
        }
    }
    for (UINT i = 0; i < n; i++) {
        ModesForDescription(devs[i], d, snap);
        if (snap->Count != 0) {
            return;
        }
    }
}

NTSTATUS PsiddParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION *pInArgs, IDARG_OUT_PARSEMONITORDESCRIPTION *pOutArgs)
{
    PSIDD_MODE_SNAPSHOT snap;
    UINT count = 0;
    NTSTATUS status;

    ModesForAnyDevice(&pInArgs->MonitorDescription, &snap);
    status = FillMonitorModes(snap.Modes, snap.Count, pInArgs->MonitorModeBufferInputCount, pInArgs->pMonitorModes, &count);
    pOutArgs->MonitorModeBufferOutputCount = count;
    pOutArgs->PreferredMonitorModeIdx = 0;
    return status;
}

/* ---- IddCx 1.10: HDR / wide colour (minor 3) ---------------------------------------- */

/* 8 bits per component for SDR; 8 + 10 when the monitor offers HDR (the OS dithers 10-bit
 * HDR down where needed; Windows requires the 10-bit capability for HDR10). */
static IDDCX_WIRE_BITS_PER_COMPONENT WireBits(BOOL hdr)
{
    IDDCX_WIRE_BITS_PER_COMPONENT b;
    RtlZeroMemory(&b, sizeof(b));
    b.Rgb = hdr ? (IDDCX_BITS_PER_COMPONENT)(IDDCX_BITS_PER_COMPONENT_8 | IDDCX_BITS_PER_COMPONENT_10) : IDDCX_BITS_PER_COMPONENT_8;
    return b;
}

NTSTATUS PsiddParseMonitorDescription2(const IDARG_IN_PARSEMONITORDESCRIPTION2 *pInArgs, IDARG_OUT_PARSEMONITORDESCRIPTION *pOutArgs)
{
    PSIDD_MODE_SNAPSHOT snap;
    const PSIDD_MODE *modes;
    UINT n;

    ModesForAnyDevice(&pInArgs->MonitorDescription, &snap);
    modes = snap.Count ? snap.Modes : &DefaultMode;
    n = snap.Count ? snap.Count : 1;
    pOutArgs->MonitorModeBufferOutputCount = n;
    pOutArgs->PreferredMonitorModeIdx = 0;
    if (pInArgs->MonitorModeBufferInputCount == 0) {
        return STATUS_SUCCESS; /* size query */
    }
    if (pInArgs->MonitorModeBufferInputCount < n) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    for (UINT i = 0; i < n; i++) {
        IDDCX_MONITOR_MODE2 *o = &pInArgs->pMonitorModes[i];
        RtlZeroMemory(o, sizeof(*o));
        o->Size = sizeof(*o);
        o->Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
        o->MonitorVideoSignalInfo = PsiddSignalInfo(&modes[i], TRUE);
        o->BitsPerComponent = WireBits(snap.Hdr);
    }
    return STATUS_SUCCESS;
}

NTSTATUS PsiddQueryTargetModes2(IDDCX_MONITOR Monitor, const IDARG_IN_QUERYTARGETMODES2 *pInArgs, IDARG_OUT_QUERYTARGETMODES *pOutArgs)
{
    PSIDD_MONITOR_CONTEXT *mc = PsiddGetMonitorContext(Monitor);
    PSIDD_MONITOR *m = &mc->Device->Monitors[mc->Slot];
    const PSIDD_MODE *modes = m->ModeCount ? m->Modes : &DefaultMode;
    UINT n = m->ModeCount ? m->ModeCount : 1;
    BOOL hdr = MonitorHdr(mc->Device, m);

    pOutArgs->TargetModeBufferOutputCount = n;
    if (pInArgs->TargetModeBufferInputCount == 0) {
        return STATUS_SUCCESS;
    }
    if (pInArgs->TargetModeBufferInputCount < n) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    for (UINT i = 0; i < n; i++) {
        IDDCX_TARGET_MODE2 *t = &pInArgs->pTargetModes[i];
        RtlZeroMemory(t, sizeof(*t));
        t->Size = sizeof(*t);
        t->TargetVideoSignalInfo.targetVideoSignalInfo = PsiddSignalInfo(&modes[i], FALSE);
        t->RequiredBandwidth = t->TargetVideoSignalInfo.targetVideoSignalInfo.pixelRate;
        t->BitsPerComponent = WireBits(hdr);
    }
    return STATUS_SUCCESS;
}

/* Target caps per connector: HDR10 + SDR wide colour for monitors added with the HDR flag
 * (the OS still derives the actual HDR modes from the EDID's HDR blocks), nothing for the
 * others. Runs during IddCxMonitorArrival on the adding thread (which holds Lock), so the
 * pending slot is checked first, then the slots under a tried shared lock. */
NTSTATUS PsiddQueryTargetInfo(IDDCX_ADAPTER Adapter, IDARG_IN_QUERYTARGET_INFO *pInArgs, IDARG_OUT_QUERYTARGET_INFO *pOutArgs)
{
    PSIDD_DEVICE_CONTEXT *ctx = AdapterDevice(Adapter);
    BOOL hdr = FALSE;
    PSIDD_MONITOR *pending;

    if (ctx != NULL) {
        pending = ctx->Pending;
        if (pending != NULL && ctx->PendingThread == GetCurrentThreadId() && pending->ConnectorIndex == pInArgs->ConnectorIndex) {
            hdr = MonitorHdr(ctx, pending);
        } else {
            ULONGLONG start = GetTickCount64();
            for (;;) {
                if (TryAcquireSRWLockShared(&ctx->Lock)) {
                    for (UINT i = 0; i < PSIDD_MAX_MONITORS; i++) {
                        PSIDD_MONITOR *m = &ctx->Monitors[i];
                        if (m->InUse && m->ConnectorIndex == pInArgs->ConnectorIndex) {
                            hdr = MonitorHdr(ctx, m);
                            break;
                        }
                    }
                    ReleaseSRWLockShared(&ctx->Lock);
                    break;
                }
                pending = ctx->Pending;
                if (pending != NULL && pending->ConnectorIndex == pInArgs->ConnectorIndex) {
                    hdr = MonitorHdr(ctx, pending);
                    break;
                }
                if (GetTickCount64() - start >= PSIDD_PARSE_WAIT_MS) {
                    break; /* SDR caps; the OS asks again on the next arrival */
                }
                Sleep(1);
            }
        }
    }
    pOutArgs->TargetCaps = hdr ? (IDDCX_TARGET_CAPS)(IDDCX_TARGET_CAPS_HIGH_COLOR_SPACE | IDDCX_TARGET_CAPS_WIDE_COLOR_SPACE) : IDDCX_TARGET_CAPS_NONE;
    pOutArgs->DitheringSupport = WireBits(hdr);
    return STATUS_SUCCESS;
}

NTSTATUS PsiddCommitModes2(IDDCX_ADAPTER Adapter, const IDARG_IN_COMMITMODES2 *pInArgs)
{
    UNREFERENCED_PARAMETER(Adapter);
    /* No hardware to program. Remember per monitor whether the path runs in a wide / high
     * colour mode (diagnostics; the frame header carries each frame's own colour space). */
    for (UINT i = 0; i < pInArgs->PathCount; i++) {
        const IDDCX_PATH2 *p = &pInArgs->pPaths[i];
        PSIDD_MONITOR_CONTEXT *mc;
        if (p->MonitorObject == NULL) {
            continue;
        }
        mc = PsiddGetMonitorContext(p->MonitorObject);
        if (mc->Device == NULL || mc->Slot >= PSIDD_MAX_MONITORS) {
            continue;
        }
        AcquireSRWLockExclusive(&mc->Device->SwapLocks[mc->Slot]);
        mc->Device->Monitors[mc->Slot].AdvancedColor =
            (p->Flags & IDDCX_PATH_FLAGS_ACTIVE) != 0 &&
            (p->WireFormatInfo.ColorSpace == IDDCX_COLOR_SPACE_G2084_P2020 || p->WireFormatInfo.ColorSpace == IDDCX_COLOR_SPACE_G22_P709_WCG);
        ReleaseSRWLockExclusive(&mc->Device->SwapLocks[mc->Slot]);
    }
    return STATUS_SUCCESS;
}

/* Default HDR10 metadata of a monitor (sent by the OS after IddCxMonitorCreate and whenever
 * it changes): used for frames whose metadata type is "default", copied to the header. */
NTSTATUS PsiddSetDefaultHdrMetaData(IDDCX_MONITOR Monitor, const IDARG_IN_MONITOR_SET_DEFAULT_HDR_METADATA *pInArgs)
{
    PSIDD_MONITOR_CONTEXT *mc = PsiddGetMonitorContext(Monitor);
    PSIDD_MONITOR *m;
    if (mc->Device == NULL || mc->Slot >= PSIDD_MAX_MONITORS) {
        return STATUS_SUCCESS;
    }
    m = &mc->Device->Monitors[mc->Slot];
    AcquireSRWLockExclusive(&mc->Device->SwapLocks[mc->Slot]);
    if (pInArgs->Type == IDDCX_HDRMETADATA_TYPE_HDR10 && pInArgs->Data.pHdr10 != NULL && pInArgs->Size >= sizeof(IDDCX_HDR10_METADATA)) {
        const IDDCX_HDR10_METADATA *h = pInArgs->Data.pHdr10;
        m->DefaultHdr[0] = h->RedPrimary[0];
        m->DefaultHdr[1] = h->RedPrimary[1];
        m->DefaultHdr[2] = h->GreenPrimary[0];
        m->DefaultHdr[3] = h->GreenPrimary[1];
        m->DefaultHdr[4] = h->BluePrimary[0];
        m->DefaultHdr[5] = h->BluePrimary[1];
        m->DefaultHdr[6] = h->WhitePoint[0];
        m->DefaultHdr[7] = h->WhitePoint[1];
        m->DefaultHdr[8] = h->MaxMasteringLuminance;
        m->DefaultHdr[9] = h->MinMasteringLuminance;
        m->DefaultHdr[10] = h->MaxContentLightLevel;
        m->DefaultHdr[11] = h->MaxFrameAverageLightLevel;
        m->DefaultHdrValid = TRUE;
    } else {
        m->DefaultHdrValid = FALSE;
    }
    ReleaseSRWLockExclusive(&mc->Device->SwapLocks[mc->Slot]);
    return STATUS_SUCCESS;
}

NTSTATUS PsiddGetDefaultModes(IDDCX_MONITOR Monitor, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES *pInArgs, IDARG_OUT_GETDEFAULTDESCRIPTIONMODES *pOutArgs)
{
    PSIDD_MONITOR_CONTEXT *mc = PsiddGetMonitorContext(Monitor);
    PSIDD_MONITOR *m = &mc->Device->Monitors[mc->Slot];
    UINT count = 0;
    NTSTATUS status = FillMonitorModes(m->Modes, m->ModeCount, pInArgs->DefaultMonitorModeBufferInputCount,
                                       pInArgs->pDefaultMonitorModes, &count);
    pOutArgs->DefaultMonitorModeBufferOutputCount = count;
    pOutArgs->PreferredMonitorModeIdx = 0;
    return status;
}

NTSTATUS PsiddQueryTargetModes(IDDCX_MONITOR Monitor, const IDARG_IN_QUERYTARGETMODES *pInArgs, IDARG_OUT_QUERYTARGETMODES *pOutArgs)
{
    PSIDD_MONITOR_CONTEXT *mc = PsiddGetMonitorContext(Monitor);
    PSIDD_MONITOR *m = &mc->Device->Monitors[mc->Slot];
    const PSIDD_MODE *modes = m->ModeCount ? m->Modes : &DefaultMode;
    UINT n = m->ModeCount ? m->ModeCount : 1;
    UINT i;

    pOutArgs->TargetModeBufferOutputCount = n;
    if (pInArgs->TargetModeBufferInputCount == 0) {
        return STATUS_SUCCESS;
    }
    if (pInArgs->TargetModeBufferInputCount < n) {
        return STATUS_BUFFER_TOO_SMALL;
    }
    for (i = 0; i < n; i++) {
        IDDCX_TARGET_MODE *t = &pInArgs->pTargetModes[i];
        RtlZeroMemory(t, sizeof(*t));
        t->Size = sizeof(*t);
        t->TargetVideoSignalInfo.targetVideoSignalInfo = PsiddSignalInfo(&modes[i], FALSE);
    }
    return STATUS_SUCCESS;
}

/* ---- swap chains ------------------------------------------------------------------- */

/* Assign, Unassign and RemoveMonitor run on different threads (IddCx callbacks vs. the
 * service's IOCTL): the slot's SwapLock serialises every start, stop and section teardown,
 * so a thread handle is waited on and closed exactly once, and the section is never
 * unmapped while a swap-chain thread still writes it. */
NTSTATUS PsiddAssignSwapChain(IDDCX_MONITOR Monitor, const IDARG_IN_SETSWAPCHAIN *pInArgs)
{
    PSIDD_MONITOR_CONTEXT *mc = PsiddGetMonitorContext(Monitor);
    PSIDD_MONITOR *m = &mc->Device->Monitors[mc->Slot];
    SRWLOCK *lock = &mc->Device->SwapLocks[mc->Slot];
    NTSTATUS status;

    AcquireSRWLockExclusive(lock);
    PsiddStopSwapChain(m);
    if (m->Header == NULL) {
        /* RemoveMonitor already tore the slot down: a late assign gets no thread. Like the
         * other failed-assign paths (PsiddStartSwapChain) the swap chain is not deleted here:
         * the docs do not say who owns it after a failed assign, and a leak beats a double
         * delete. Untested on hardware. */
        status = STATUS_DEVICE_REMOVED;
    } else {
        status = PsiddStartSwapChain(m, Monitor, pInArgs);
    }
    ReleaseSRWLockExclusive(lock);
    return status;
}

NTSTATUS PsiddUnassignSwapChain(IDDCX_MONITOR Monitor)
{
    PSIDD_MONITOR_CONTEXT *mc = PsiddGetMonitorContext(Monitor);
    SRWLOCK *lock = &mc->Device->SwapLocks[mc->Slot];

    AcquireSRWLockExclusive(lock);
    PsiddStopSwapChain(&mc->Device->Monitors[mc->Slot]);
    ReleaseSRWLockExclusive(lock);
    return STATUS_SUCCESS;
}

/* ---- control IOCTLs ---------------------------------------------------------------- */

static uint32_t SlotOf(uint32_t seat, uint32_t connector)
{
    return (seat - 1) * PSIDD_MAX_CONNECTORS_PER_SEAT + connector;
}

/* IddCx connector index for a new monitor: unique on the adapter, below
 * PSIDD_ADAPTER_MAX_MONITORS, and the same for the same output across restarts where
 * possible: a seat's first output takes seat - 1, further outputs the highest free index.
 * Caller holds ctx->Lock exclusive. Returns PSIDD_ADAPTER_MAX_MONITORS when all are used. */
static UINT PickConnectorIndex(const PSIDD_DEVICE_CONTEXT *ctx, uint32_t seat, uint32_t connector)
{
    BOOL used[PSIDD_ADAPTER_MAX_MONITORS];
    RtlZeroMemory(used, sizeof(used));
    for (UINT k = 0; k < PSIDD_MAX_MONITORS; k++) {
        if (ctx->Monitors[k].InUse && ctx->Monitors[k].ConnectorIndex < PSIDD_ADAPTER_MAX_MONITORS) {
            used[ctx->Monitors[k].ConnectorIndex] = TRUE;
        }
    }
    if (connector == 0 && seat >= 1 && seat - 1 < PSIDD_ADAPTER_MAX_MONITORS && !used[seat - 1]) {
        return seat - 1;
    }
    for (UINT i = PSIDD_ADAPTER_MAX_MONITORS; i-- > 0;) {
        if (!used[i]) {
            return i;
        }
    }
    return PSIDD_ADAPTER_MAX_MONITORS;
}

static NTSTATUS AddMonitor(PSIDD_DEVICE_CONTEXT *ctx, const PSIDD_ADD_MONITOR *req)
{
    IDDCX_MONITOR_INFO info;
    IDARG_IN_MONITORCREATE in;
    IDARG_OUT_MONITORCREATE out;
    IDARG_OUT_MONITORARRIVAL arrival;
    WDF_OBJECT_ATTRIBUTES attr;
    PSIDD_MONITOR *m;
    uint32_t slot;
    UINT connectorIndex;
    NTSTATUS status;

    if (req->Size != sizeof(*req) || req->Seat < 1 || req->Seat > PSIDD_MAX_SEATS ||
        req->Connector >= PSIDD_MAX_CONNECTORS_PER_SEAT || req->EdidLength > PSIDD_MAX_EDID ||
        req->ModeCount > PSIDD_MAX_MODES || !ModesValid(req->Modes, req->ModeCount) ||
        (req->EdidLength % 128) != 0 || (req->Flags & ~PSIDD_ADD_FLAGS_KNOWN) != 0) {
        return STATUS_INVALID_PARAMETER;
    }
    slot = SlotOf(req->Seat, req->Connector);

    AcquireSRWLockExclusive(&ctx->Lock);
    if (!ctx->AdapterReady || ctx->Adapter == NULL) {
        ReleaseSRWLockExclusive(&ctx->Lock);
        return STATUS_DEVICE_NOT_READY;
    }
    m = &ctx->Monitors[slot];
    if (m->InUse) { /* also while a RemoveMonitor of the slot is still cleaning up */
        ReleaseSRWLockExclusive(&ctx->Lock);
        return STATUS_OBJECT_NAME_COLLISION;
    }
    connectorIndex = PickConnectorIndex(ctx, req->Seat, req->Connector);
    if (connectorIndex >= PSIDD_ADAPTER_MAX_MONITORS) {
        ReleaseSRWLockExclusive(&ctx->Lock);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    if (req->RenderAdapterLuid != 0) {
        /* One render GPU per adapter (minor 3): IddCxAdapterSetRenderAdapter is adapter-wide,
         * so a monitor of another GPU belongs on another adapter (the service creates one
         * per GPU). An adapter without monitors is re-bound. */
        LUID want = LuidFrom(req->RenderAdapterLuid);
        if (ctx->Bound && ctx->MonitorCount > 0 && !LuidEq(ctx->BoundLuid, want)) {
            ReleaseSRWLockExclusive(&ctx->Lock);
            return STATUS_DEVICE_BUSY;
        }
        if (!ctx->Bound || !LuidEq(ctx->BoundLuid, want)) {
            IDARG_IN_ADAPTERSETRENDERADAPTER ra;
            RtlZeroMemory(&ra, sizeof(ra));
            ra.PreferredRenderAdapter = want;
            IddCxAdapterSetRenderAdapter(ctx->Adapter, &ra);
            ctx->Bound = TRUE;
            ctx->BoundLuid = want;
        }
    }
    RtlZeroMemory(m, sizeof(*m));
    m->InUse = TRUE;
    m->ConnectorIndex = connectorIndex;
    m->Seat = req->Seat;
    m->Connector = req->Connector;
    m->Flags = req->Flags;
    m->EdidLength = req->EdidLength;
    memcpy(m->Edid, req->Edid, req->EdidLength);
    PatchEdidSerial(m, slot);
    m->ModeCount = req->ModeCount;
    memcpy(m->Modes, req->Modes, sizeof(PSIDD_MODE) * req->ModeCount);
    m->Has110 = ctx->Has110;

    status = PsiddCreateSection(m, req->ReaderSid);
    if (!NT_SUCCESS(status)) {
        m->InUse = FALSE;
        ReleaseSRWLockExclusive(&ctx->Lock);
        return status;
    }

    RtlZeroMemory(&info, sizeof(info));
    info.Size = sizeof(info);
    /* INDIRECT_VIRTUAL makes IddCxMonitorCreate fail with STATUS_INVALID_PARAMETER on this
     * (non remote-session) adapter; INDIRECT_WIRED is accepted (VM 2026-11-15). */
    info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED;
    info.ConnectorIndex = m->ConnectorIndex;
    info.MonitorDescription.Size = sizeof(info.MonitorDescription);
    info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    info.MonitorDescription.DataSize = m->EdidLength;
    info.MonitorDescription.pData = m->EdidLength ? m->Edid : NULL;
    /* Stable container per seat output, so Windows keeps per-monitor settings. */
    info.MonitorContainerId.Data1 = 0x50534944u; /* "PSID" */
    info.MonitorContainerId.Data2 = (USHORT)req->Seat;
    info.MonitorContainerId.Data3 = (USHORT)req->Connector;

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, PSIDD_MONITOR_CONTEXT);
    RtlZeroMemory(&in, sizeof(in));
    in.ObjectAttributes = &attr;
    in.pMonitorInfo = &info;

    ctx->PendingThread = GetCurrentThreadId();
    ctx->Pending = m; /* volatile store after the slot is complete */
    status = IddCxMonitorCreate(ctx->Adapter, &in, &out);
    if (NT_SUCCESS(status)) {
        PSIDD_MONITOR_CONTEXT *mc = PsiddGetMonitorContext(out.MonitorObject);
        mc->Device = ctx;
        mc->Slot = slot;
        m->Object = out.MonitorObject;
        /* Arrival calls back into Parse / QueryTargetModes on this thread. */
        status = IddCxMonitorArrival(out.MonitorObject, &arrival);
        if (!NT_SUCCESS(status)) {
            /* Created but never arrived: delete it so its context cannot point at a slot
             * that is reused later. */
            WdfObjectDelete((WDFOBJECT)out.MonitorObject);
            m->Object = NULL;
        }
    }
    ctx->Pending = NULL;
    ctx->PendingThread = 0;
    if (!NT_SUCCESS(status)) {
        /* Stop an assign that slipped in before the failed arrival, then tear down. */
        AcquireSRWLockExclusive(&ctx->SwapLocks[slot]);
        PsiddStopSwapChain(m);
        PsiddCloseSection(m);
        ReleaseSRWLockExclusive(&ctx->SwapLocks[slot]);
        m->InUse = FALSE;
    } else {
        ctx->MonitorCount++;
    }
    ReleaseSRWLockExclusive(&ctx->Lock);
    return status;
}

static NTSTATUS RemoveMonitor(PSIDD_DEVICE_CONTEXT *ctx, const PSIDD_MONITOR_REF *ref)
{
    PSIDD_MONITOR *m;
    IDDCX_MONITOR obj;
    NTSTATUS status;
    uint32_t slot;
    if (ref->Seat < 1 || ref->Seat > PSIDD_MAX_SEATS || ref->Connector >= PSIDD_MAX_CONNECTORS_PER_SEAT) {
        return STATUS_INVALID_PARAMETER;
    }
    slot = SlotOf(ref->Seat, ref->Connector);
    AcquireSRWLockExclusive(&ctx->Lock);
    m = &ctx->Monitors[slot];
    if (!m->InUse || m->Removing || m->Object == NULL) {
        ReleaseSRWLockExclusive(&ctx->Lock);
        return STATUS_NOT_FOUND;
    }
    /* Claim the removal under the lock: a concurrent remover now gets NOT_FOUND and an
     * add of the same slot gets OBJECT_NAME_COLLISION (InUse stays TRUE) until cleanup.
     * Clearing Object is safe for the swap-chain thread: it uses SwapMonitor. */
    obj = m->Object;
    m->Object = NULL;
    m->Removing = TRUE;
    ReleaseSRWLockExclusive(&ctx->Lock);

    /* Departure unassigns the swap chain (UnassignSwapChain stops the thread). */
    status = IddCxMonitorDeparture(obj);
    if (!NT_SUCCESS(status)) {
        /* Nothing was torn down: give the slot back as it was, the caller may retry. */
        AcquireSRWLockExclusive(&ctx->Lock);
        m->Object = obj;
        m->Removing = FALSE;
        ReleaseSRWLockExclusive(&ctx->Lock);
        return status;
    }

    AcquireSRWLockExclusive(&ctx->Lock);
    /* Unassign normally stopped the thread already; the SwapLock makes this stop and the
     * section teardown exclusive with a concurrent Unassign or a late Assign. */
    AcquireSRWLockExclusive(&ctx->SwapLocks[slot]);
    PsiddStopSwapChain(m);
    PsiddCloseSection(m);
    ReleaseSRWLockExclusive(&ctx->SwapLocks[slot]);
    m->Removing = FALSE;
    m->InUse = FALSE;
    if (ctx->MonitorCount > 0) {
        ctx->MonitorCount--;
    }
    ReleaseSRWLockExclusive(&ctx->Lock);
    return STATUS_SUCCESS;
}

VOID PsiddIoControl(WDFDEVICE Device, WDFREQUEST Request, size_t OutputBufferLength, size_t InputBufferLength, ULONG IoControlCode)
{
    PSIDD_DEVICE_CONTEXT *ctx = PsiddGetDeviceContext(Device);
    NTSTATUS status = STATUS_INVALID_DEVICE_REQUEST;
    size_t info = 0;
    void *in = NULL;
    void *out = NULL;

    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case IOCTL_PSIDD_GET_VERSION:
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(PSIDD_VERSION), &out, NULL);
        if (NT_SUCCESS(status)) {
            PSIDD_VERSION *v = (PSIDD_VERSION *)out;
            v->Major = PSIDD_INTERFACE_MAJOR;
            v->Minor = PSIDD_INTERFACE_MINOR;
            v->Build = PSIDD_BUILD;
            info = sizeof(*v);
        }
        break;
    case IOCTL_PSIDD_ADD_MONITOR:
        status = WdfRequestRetrieveInputBuffer(Request, sizeof(PSIDD_ADD_MONITOR), &in, NULL);
        if (NT_SUCCESS(status)) {
            status = AddMonitor(ctx, (const PSIDD_ADD_MONITOR *)in);
        }
        break;
    case IOCTL_PSIDD_REMOVE_MONITOR:
        status = WdfRequestRetrieveInputBuffer(Request, sizeof(PSIDD_MONITOR_REF), &in, NULL);
        if (NT_SUCCESS(status)) {
            status = RemoveMonitor(ctx, (const PSIDD_MONITOR_REF *)in);
        }
        break;
    case IOCTL_PSIDD_SET_RENDER_ADAPTER:
        status = WdfRequestRetrieveInputBuffer(Request, sizeof(PSIDD_RENDER_ADAPTER), &in, NULL);
        if (NT_SUCCESS(status)) {
            const PSIDD_RENDER_ADAPTER *r = (const PSIDD_RENDER_ADAPTER *)in;
            IDARG_IN_ADAPTERSETRENDERADAPTER ra;
            RtlZeroMemory(&ra, sizeof(ra));
            ra.PreferredRenderAdapter = LuidFrom(r->Luid);
            AcquireSRWLockExclusive(&ctx->Lock);
            if (!ctx->AdapterReady || ctx->Adapter == NULL) {
                status = STATUS_DEVICE_NOT_READY;
            } else if (r->Luid == 0) {
                status = STATUS_INVALID_PARAMETER;
            } else if (ctx->Bound && ctx->MonitorCount > 0 && !LuidEq(ctx->BoundLuid, ra.PreferredRenderAdapter)) {
                status = STATUS_DEVICE_BUSY; /* monitors of another GPU still plugged here */
            } else {
                IddCxAdapterSetRenderAdapter(ctx->Adapter, &ra);
                ctx->Bound = TRUE;
                ctx->BoundLuid = ra.PreferredRenderAdapter;
            }
            ReleaseSRWLockExclusive(&ctx->Lock);
        }
        break;
    case IOCTL_PSIDD_GET_ADAPTER_INFO:
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(PSIDD_ADAPTER_INFO), &out, NULL);
        if (NT_SUCCESS(status)) {
            PSIDD_ADAPTER_INFO *a = (PSIDD_ADAPTER_INFO *)out;
            RtlZeroMemory(a, sizeof(*a));
            a->Size = sizeof(*a);
            AcquireSRWLockShared(&ctx->Lock);
            a->Flags = (ctx->AdapterReady ? PSIDD_ADAPTER_READY : 0) | (ctx->Bound ? PSIDD_ADAPTER_BOUND : 0) |
                       (ctx->Has110 ? PSIDD_ADAPTER_FP16 : 0);
            a->RenderLuid = ctx->Bound ? LuidTo(ctx->BoundLuid) : 0;
            a->MonitorCount = ctx->MonitorCount;
            a->IddCxVersion = ctx->IddCxVersion;
            ReleaseSRWLockShared(&ctx->Lock);
            info = sizeof(*a);
        }
        break;
    default:
        break;
    }
    if (NT_SUCCESS(status) && IoControlCode != IOCTL_PSIDD_GET_VERSION && IoControlCode != IOCTL_PSIDD_GET_ADAPTER_INFO &&
        NT_SUCCESS(WdfRequestRetrieveOutputBuffer(Request, sizeof(UINT32), &out, NULL))) {
        *(UINT32 *)out = 0;
        info = sizeof(UINT32);
    }
    WdfRequestCompleteWithInformation(Request, status, info);
}
