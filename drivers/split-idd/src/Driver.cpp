// SPDX-License-Identifier: GPL-3.0-only
//
// Driver entry, device creation and the IddCx callback table.
// UNBUILT · UNTESTED · UNSIGNED.

#include "Driver.h"

using namespace pulsar;

extern "C" DRIVER_INITIALIZE DriverEntry;

static EVT_WDF_DRIVER_DEVICE_ADD EvtDeviceAdd;
static EVT_WDF_DEVICE_D0_ENTRY EvtDeviceD0Entry;
static EVT_WDF_OBJECT_CONTEXT_CLEANUP EvtDeviceCleanup;

static EVT_IDD_CX_DEVICE_IO_CONTROL EvtIddDeviceIoControl;
static EVT_IDD_CX_ADAPTER_INIT_FINISHED EvtAdapterInitFinished;
static EVT_IDD_CX_ADAPTER_COMMIT_MODES EvtAdapterCommitModes;
static EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION EvtParseMonitorDescription;
static EVT_IDD_CX_MONITOR_GET_DEFAULT_DESCRIPTION_MODES EvtMonitorGetDefaultModes;
static EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES EvtMonitorQueryTargetModes;
static EVT_IDD_CX_MONITOR_ASSIGN_SWAPCHAIN EvtMonitorAssignSwapChain;
static EVT_IDD_CX_MONITOR_UNASSIGN_SWAPCHAIN EvtMonitorUnassignSwapChain;
// IddCx 1.10 (registered only when available at run time)
static EVT_IDD_CX_ADAPTER_QUERY_TARGET_INFO EvtAdapterQueryTargetInfo;
static EVT_IDD_CX_ADAPTER_COMMIT_MODES2 EvtAdapterCommitModes2;
static EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION2 EvtParseMonitorDescription2;
static EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES2 EvtMonitorQueryTargetModes2;
static EVT_IDD_CX_MONITOR_SET_GAMMA_RAMP EvtMonitorSetGammaRamp;

static AdapterContext* FromDevice(WDFDEVICE d) { return GetDeviceWrapper(d)->ctx; }
static AdapterContext* FromAdapter(IDDCX_ADAPTER a) { return GetAdapterWrapper(a)->ctx; }
static AdapterContext* FromMonitor(IDDCX_MONITOR m) { return GetMonitorWrapper(m)->ctx; }

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driverObject, PUNICODE_STRING registryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    return WdfDriverCreate(driverObject, registryPath, &attributes, &config, WDF_NO_HANDLE);
}

static NTSTATUS EvtDeviceAdd(WDFDRIVER, PWDFDEVICE_INIT deviceInit)
{
    WDF_PNPPOWER_EVENT_CALLBACKS pnp;
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp);
    pnp.EvtDeviceD0Entry = EvtDeviceD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(deviceInit, &pnp);

    IDD_CX_CLIENT_CONFIG cfg;
    IDD_CX_CLIENT_CONFIG_INIT(&cfg);
    cfg.EvtIddCxDeviceIoControl = EvtIddDeviceIoControl;
    cfg.EvtIddCxAdapterInitFinished = EvtAdapterInitFinished;
    cfg.EvtIddCxAdapterCommitModes = EvtAdapterCommitModes;
    cfg.EvtIddCxParseMonitorDescription = EvtParseMonitorDescription;
    cfg.EvtIddCxMonitorGetDefaultDescriptionModes = EvtMonitorGetDefaultModes;
    cfg.EvtIddCxMonitorQueryTargetModes = EvtMonitorQueryTargetModes;
    cfg.EvtIddCxMonitorAssignSwapChain = EvtMonitorAssignSwapChain;
    cfg.EvtIddCxMonitorUnassignSwapChain = EvtMonitorUnassignSwapChain;
    // 1.10 callbacks: only set the fields the running IddCx knows (spec 40 §8.4,
    // "Supporting a 1.10 driver running down level").
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterQueryTargetInfo)) {
        cfg.EvtIddCxAdapterQueryTargetInfo = EvtAdapterQueryTargetInfo;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterCommitModes2)) {
        cfg.EvtIddCxAdapterCommitModes2 = EvtAdapterCommitModes2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxParseMonitorDescription2)) {
        cfg.EvtIddCxParseMonitorDescription2 = EvtParseMonitorDescription2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorQueryTargetModes2)) {
        cfg.EvtIddCxMonitorQueryTargetModes2 = EvtMonitorQueryTargetModes2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorSetGammaRamp)) {
        cfg.EvtIddCxMonitorSetGammaRamp = EvtMonitorSetGammaRamp;
    }

    NTSTATUS status = IddCxDeviceInitConfig(deviceInit, &cfg);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, DeviceContextWrapper);
    attr.EvtCleanupCallback = EvtDeviceCleanup;

    WDFDEVICE device = nullptr;
    status = WdfDeviceCreate(&deviceInit, &attr, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    status = IddCxDeviceInitialize(device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    // Control channel for the service (spec 40 §7). Access is limited by the device
    // security in the INF (SYSTEM full, Administrators read).
    static const GUID iface = PULSAR_IDD_INTERFACE_GUID_INIT;
    status = WdfDeviceCreateDeviceInterface(device, &iface, nullptr);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    auto* ctx = new (std::nothrow) AdapterContext(device);
    if (!ctx) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    GetDeviceWrapper(device)->ctx = ctx;
    return STATUS_SUCCESS;
}

static void EvtDeviceCleanup(WDFOBJECT object)
{
    auto* w = GetDeviceWrapper(static_cast<WDFDEVICE>(object));
    delete w->ctx;
    w->ctx = nullptr;
}

static NTSTATUS EvtDeviceD0Entry(WDFDEVICE device, WDF_POWER_DEVICE_STATE)
{
    return FromDevice(device)->InitAdapter();
}

static VOID EvtIddDeviceIoControl(WDFDEVICE device, WDFREQUEST request, size_t outLen, size_t inLen, ULONG code)
{
    FromDevice(device)->DeviceIoControl(request, outLen, inLen, code);
}

static NTSTATUS EvtAdapterInitFinished(IDDCX_ADAPTER adapter, const IDARG_IN_ADAPTER_INIT_FINISHED* args)
{
    FromAdapter(adapter)->OnInitFinished(args);
    return STATUS_SUCCESS;
}

static NTSTATUS EvtAdapterCommitModes(IDDCX_ADAPTER adapter, const IDARG_IN_COMMITMODES* args)
{
    bool active = false;
    bool haveMode = false;
    DISPLAYCONFIG_TARGET_MODE tm{};
    for (UINT i = 0; i < args->PathCount; ++i) {
        const IDDCX_PATH& p = args->pPaths[i];
        if (p.Flags & IDDCX_PATH_FLAGS_ACTIVE) {
            active = true;
            haveMode = true;
            tm.targetVideoSignalInfo = p.TargetVideoSignalInfo;
        }
    }
    FromAdapter(adapter)->OnCommitModes(active, haveMode ? &tm : nullptr, false, false);
    return STATUS_SUCCESS;
}

static NTSTATUS EvtAdapterCommitModes2(IDDCX_ADAPTER adapter, const IDARG_IN_COMMITMODES2* args)
{
    bool active = false, hdr = false;
    DISPLAYCONFIG_TARGET_MODE tm{};
    bool haveMode = false;
    for (UINT i = 0; i < args->PathCount; ++i) {
        const IDDCX_PATH2& p = args->pPaths[i];
        if (p.Flags & IDDCX_PATH_FLAGS_ACTIVE) {
            active = true;
            tm.targetVideoSignalInfo = p.TargetVideoSignalInfo;
            haveMode = true;
            // FP16 / BT.2020 path => HDR or WCG active (IddCx 1.10).
            hdr = p.ColorimetryMode == IDDCX_COLORIMETRY_MODE_BT2100_RGB; // VERIFY name against iddcx.h
        }
    }
    FromAdapter(adapter)->OnCommitModes(active, haveMode ? &tm : nullptr, hdr, true);
    return STATUS_SUCCESS;
}

static NTSTATUS EvtAdapterQueryTargetInfo(IDDCX_ADAPTER, IDARG_IN_QUERYTARGET_INFO*, IDARG_OUT_QUERYTARGET_INFO* out)
{
    // Every target can do HDR10 and SDR WCG; the OS derives HDR modes from this.
    out->TargetCaps = IDDCX_TARGET_CAPS_HIGH_COLOR_SPACE | IDDCX_TARGET_CAPS_WIDE_COLOR_SPACE;
    out->DitheringSupport.Rgb = IDDCX_BITS_PER_COMPONENT_8 | IDDCX_BITS_PER_COMPONENT_10;
    return STATUS_SUCCESS;
}

// Remote driver with ALL_TARGET_MODES_MONITOR_COMPATIBLE: the OS uses target modes only
// and does not need monitor modes. Report none.
static NTSTATUS EvtParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION*, IDARG_OUT_PARSEMONITORDESCRIPTION* out)
{
    out->MonitorModeBufferOutputCount = 0;
    return STATUS_SUCCESS;
}

static NTSTATUS EvtParseMonitorDescription2(const IDARG_IN_PARSEMONITORDESCRIPTION2*, IDARG_OUT_PARSEMONITORDESCRIPTION* out)
{
    out->MonitorModeBufferOutputCount = 0;
    return STATUS_SUCCESS;
}

static NTSTATUS EvtMonitorGetDefaultModes(IDDCX_MONITOR, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES*, IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* out)
{
    out->DefaultMonitorModeBufferOutputCount = 0;
    return STATUS_SUCCESS;
}

// Exactly one target mode, at the current render size (spec 40 §4.3).
static NTSTATUS EvtMonitorQueryTargetModes(IDDCX_MONITOR monitor, const IDARG_IN_QUERYTARGETMODES* in, IDARG_OUT_QUERYTARGETMODES* out)
{
    out->TargetModeBufferOutputCount = 1;
    if (in->TargetModeBufferInputCount >= 1) {
        FromMonitor(monitor)->FillTargetModes(in->pTargetModes, 1);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS EvtMonitorQueryTargetModes2(IDDCX_MONITOR monitor, const IDARG_IN_QUERYTARGETMODES2* in, IDARG_OUT_QUERYTARGETMODES* out)
{
    out->TargetModeBufferOutputCount = 1;
    if (in->TargetModeBufferInputCount >= 1) {
        FromMonitor(monitor)->FillTargetModes2(in->pTargetModes, 1);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS EvtMonitorAssignSwapChain(IDDCX_MONITOR monitor, const IDARG_IN_SETSWAPCHAIN* in)
{
    return FromMonitor(monitor)->AssignSwapChain(in->hSwapChain, in->hNextSurfaceAvailable);
}

static NTSTATUS EvtMonitorUnassignSwapChain(IDDCX_MONITOR monitor)
{
    FromMonitor(monitor)->UnassignSwapChain();
    return STATUS_SUCCESS;
}

// Remote drivers receive no HDR matrix (the client applies its own); accept and ignore.
static NTSTATUS EvtMonitorSetGammaRamp(IDDCX_MONITOR, const IDARG_IN_SET_GAMMARAMP*)
{
    return STATUS_SUCCESS;
}
