// SPDX-License-Identifier: GPL-3.0-only
//
// Adapter + monitor lifetime, commit handling, readiness events.
// UNBUILT · UNTESTED · UNSIGNED.

#include "Driver.h"

namespace pulsar {

AdapterContext::AdapterContext(WDFDEVICE device) : m_device(device)
{
    // Manual queue that holds pended WAIT_EVENT requests until an event arrives.
    WDF_IO_QUEUE_CONFIG qc;
    WDF_IO_QUEUE_CONFIG_INIT(&qc, WdfIoQueueDispatchManual);
    qc.PowerManaged = WdfFalse;
    if (!NT_SUCCESS(WdfIoQueueCreate(device, &qc, WDF_NO_OBJECT_ATTRIBUTES, &m_eventQueue))) {
        m_eventQueue = nullptr;
    }
}

AdapterContext::~AdapterContext()
{
    std::lock_guard<std::mutex> l(m_lifecycle);
    m_shutdown = true;
    m_worker.reset(); // joins the worker before the channel closes
    m_channel.Close();
}

NTSTATUS AdapterContext::InitAdapter()
{
    IDARG_OUT_GETVERSION ver{};
    if (NT_SUCCESS(IddCxGetVersion(&ver))) {
        m_iddcxVersion = ver.IddCxVersion;
    }
    // Run-time check of the 1.10 functions this driver needs for HDR (spec 40 §8.4).
    m_has110 = IDD_IS_FUNCTION_AVAILABLE(IddCxSwapChainReleaseAndAcquireBuffer2) &&
               IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorUpdateModes2) &&
               IDD_IS_FUNCTION_AVAILABLE(IddCxAdapterDisplayConfigUpdate2);

    IDDCX_ADAPTER_CAPS caps{};
    caps.Size = sizeof(caps);
    // Remote-session driver (the OS created this device for a seat session, spec 40 §2.3):
    // REMOTE_SESSION_DRIVER + USE_SMALLEST_MODE are required; target modes only.
    caps.Flags = static_cast<IDDCX_ADAPTER_FLAGS>(IDDCX_ADAPTER_FLAGS_REMOTE_SESSION_DRIVER | IDDCX_ADAPTER_FLAGS_USE_SMALLEST_MODE);
    if (m_has110) {
        caps.Flags = static_cast<IDDCX_ADAPTER_FLAGS>(caps.Flags | IDDCX_ADAPTER_FLAGS_CAN_PROCESS_FP16 |
                                                      IDDCX_ADAPTER_FLAGS_ALL_TARGET_MODES_MONITOR_COMPATIBLE);
    }
    caps.MaxMonitorsSupported = kMaxMonitors;
    caps.MaxDisplayPipelineRate = kMaxPipelineRate;
    caps.EndPointDiagnostics.Size = sizeof(caps.EndPointDiagnostics);
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName = L"Pulsar Split";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"Pulsar Split";
    caps.EndPointDiagnostics.pEndPointModelName = L"Seat display";
    static IDDCX_ENDPOINT_VERSION version{sizeof(IDDCX_ENDPOINT_VERSION), kDriverVersion[0], kDriverVersion[1], kDriverVersion[2]};
    caps.EndPointDiagnostics.pFirmwareVersion = &version;
    caps.EndPointDiagnostics.pHardwareVersion = &version;

    WDF_OBJECT_ATTRIBUTES attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, AdapterObjectWrapper);
    IDARG_IN_ADAPTER_INIT in{};
    in.WdfDevice = m_device;
    in.pCaps = &caps;
    in.ObjectAttributes = &attr;
    IDARG_OUT_ADAPTER_INIT out{};
    NTSTATUS status = IddCxAdapterInitAsync(&in, &out);
    if (NT_SUCCESS(status)) {
        m_adapter = out.AdapterObject;
        GetAdapterWrapper(m_adapter)->ctx = this;
    } else {
        std::lock_guard<std::mutex> g(m_lock);
        m_lastError = status;
    }
    return status;
}

void AdapterContext::OnInitFinished(const IDARG_IN_ADAPTER_INIT_FINISHED* args)
{
    if (!NT_SUCCESS(args->AdapterInitStatus)) {
        std::lock_guard<std::mutex> g(m_lock);
        m_lastError = args->AdapterInitStatus;
        return;
    }
    {
        std::lock_guard<std::mutex> g(m_lock);
        m_adapterReady = true;
        if (m_mode.width == 0) {
            m_mode = ComputeMode(kDefaultWidth, kDefaultHeight, kDefaultHz, false);
        }
    }
    Event(PULSAR_EVENT_READY, 0);
    // The seat session needs a display right away; the seat binding (render GPU, HDR
    // values, frame channel names) follows with SET_SEAT_CONFIG.
    NTSTATUS st = STATUS_SUCCESS;
    {
        std::lock_guard<std::mutex> control(m_control);
        if (!m_monitor) { // SET_SEAT_CONFIG may have created it already
            st = CreateMonitor();
        }
    }
    if (!NT_SUCCESS(st)) {
        std::lock_guard<std::mutex> g(m_lock);
        m_lastError = st;
    }
}

// Caller holds m_control (guards m_monitor; serialises against SHUTDOWN_SEAT).
NTSTATUS AdapterContext::CreateMonitor()
{
    WDF_OBJECT_ATTRIBUTES attr;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, MonitorObjectWrapper);

    IDDCX_MONITOR_INFO info{};
    info.Size = sizeof(info);
    info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED;
    info.ConnectorIndex = 0;
    // No EDID: remote drivers may omit monitor descriptions (target modes only).
    info.MonitorDescription.Size = sizeof(info.MonitorDescription);
    info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    info.MonitorDescription.DataSize = 0;
    info.MonitorDescription.pData = nullptr;
    CoCreateGuid(&info.MonitorContainerId);

    IDARG_IN_MONITORCREATE in{};
    in.ObjectAttributes = &attr;
    in.pMonitorInfo = &info;
    IDARG_OUT_MONITORCREATE out{};
    NTSTATUS st = IddCxMonitorCreate(m_adapter, &in, &out);
    if (!NT_SUCCESS(st)) {
        return st;
    }
    m_monitor = out.MonitorObject;
    GetMonitorWrapper(m_monitor)->ctx = this;
    IDARG_OUT_MONITORARRIVAL arrival{};
    st = IddCxMonitorArrival(m_monitor, &arrival);
    if (NT_SUCCESS(st)) {
        {
            std::lock_guard<std::mutex> g(m_lock);
            m_monitorPresent = true;
        }
        Event(PULSAR_EVENT_READY, 1);
    }
    return st;
}

void AdapterContext::OnCommitModes(bool pathActive, const DISPLAYCONFIG_TARGET_MODE* mode, bool hdr, bool colorKnown)
{
    bool becameActive = false;
    WDFREQUEST finish = nullptr;
    PULSAR_MODE_OUT out{};
    {
        std::lock_guard<std::mutex> g(m_lock);
        becameActive = pathActive && !m_pathActive;
        m_pathActive = pathActive;
        m_hdrActive = pathActive && hdr;
        m_lastCommitColorKnown = colorKnown;
        if (mode) {
            const auto& s = mode->targetVideoSignalInfo;
            m_mode.renderWidth = s.activeSize.cx;
            m_mode.renderHeight = s.activeSize.cy;
            const auto& v = s.vSyncFreq;
            m_committedHz = v.Denominator ? (v.Numerator + v.Denominator / 2) / v.Denominator : 0;
        }
        // A pended REQUEST_MODE completes once its size, refresh and colour mode are
        // committed (not on an unrelated commit that only shares the size).
        uint32_t status = PULSAR_MODE_OK;
        if (m_pendingModeRequest && PendingModeCommittedLocked(&status)) {
            m_mode = m_pendingMode;
            m_mode.hdr = m_hdrActive;
            finish = TakePendingModeLocked(status, &out);
        }
    }
    if (becameActive) {
        Event(PULSAR_EVENT_READY, 2);
    }
    if (finish) {
        FinishTaken(finish, out);
    }
}

NTSTATUS AdapterContext::AssignSwapChain(IDDCX_SWAPCHAIN swapChain, HANDLE newFrameEvent)
{
    std::lock_guard<std::mutex> l(m_lifecycle);
    m_worker.reset(); // stop + join the previous worker before the next one starts
    if (m_shutdown) {
        // Seat shut down: hand the swap chain straight back (same as a worker whose
        // device creation failed). VERIFY on hardware that the OS does not retry in a loop.
        WdfObjectDelete(reinterpret_cast<WDFOBJECT>(swapChain));
        return STATUS_SUCCESS;
    }
    m_worker = std::make_unique<SwapChainWorker>(this, swapChain, newFrameEvent);
    return STATUS_SUCCESS;
}

void AdapterContext::UnassignSwapChain()
{
    std::lock_guard<std::mutex> l(m_lifecycle);
    m_worker.reset();
}

LUID AdapterContext::RenderAdapter() const
{
    std::lock_guard<std::mutex> g(m_lock);
    return m_seat.adapterPinned ? m_seat.adapter : LUID{};
}

bool AdapterContext::SeatGuid(GUID* out) const
{
    std::lock_guard<std::mutex> g(m_lock);
    if (!m_seat.configured) return false;
    *out = m_seat.seat;
    return true;
}

bool AdapterContext::HdrActive() const
{
    std::lock_guard<std::mutex> g(m_lock);
    return m_hdrActive;
}

UINT AdapterContext::RefreshMilliHz() const
{
    std::lock_guard<std::mutex> g(m_lock);
    return m_mode.hz * 1000;
}

static uint16_t Cie(uint32_t x10000) { return static_cast<uint16_t>(x10000 * 1024 / 10000); }

PULSAR_HDR_META_V1 AdapterContext::HdrMeta() const
{
    std::lock_guard<std::mutex> g(m_lock);
    PULSAR_HDR_META_V1 m{};
    const uint32_t c[8] = {7080, 2920, 1700, 7970, 1310, 460, 3127, 3290}; // Rec. 2020 + D65
    for (int i = 0; i < 8; ++i) {
        m.Chromaticity[i] = Cie(c[i]);
    }
    m.MinLuminanceX10000 = m_seat.minLuminanceX10000;
    m.MaxLuminanceX10000 = m_seat.maxLuminanceNits * 10000;
    m.MaxFullFrameLuminanceX10000 = m_seat.maxFullFrameNits * 10000;
    m.SdrWhiteNits = m_seat.sdrWhiteNits;
    return m;
}

void AdapterContext::OnFramePublished(uint64_t frameNumber)
{
    bool first = false;
    {
        std::lock_guard<std::mutex> g(m_lock);
        m_frameNumber = frameNumber;
        first = !m_firstFrame;
        m_firstFrame = true;
    }
    if (first) {
        Event(PULSAR_EVENT_READY, 3);
    }
}

uint64_t AdapterContext::LastFrameNumber() const
{
    std::lock_guard<std::mutex> g(m_lock);
    return m_frameNumber;
}

uint32_t AdapterContext::StateFlags() const
{
    uint32_t f = 0;
    if (m_adapterReady) f |= PULSAR_STATE_ADAPTER_READY;
    if (m_monitorPresent) f |= PULSAR_STATE_MONITOR_PRESENT;
    if (m_pathActive) f |= PULSAR_STATE_PATH_ACTIVE;
    if (m_hdrActive) f |= PULSAR_STATE_HDR_ACTIVE;
    if (m_firstFrame) f |= PULSAR_STATE_FIRST_FRAME;
    if (m_seat.configured) f |= PULSAR_STATE_SEAT_CONFIGURED;
    return f;
}

// Queues an event, then hands queued events to pended WAIT_EVENT requests.
void AdapterContext::Event(uint32_t kind, uint32_t data0)
{
    PULSAR_EVENT_OUT e{};
    e.Header.Size = sizeof(e);
    e.Header.Version = PULSAR_BUFFER_VERSION;
    e.Kind = kind;
    e.Data0 = data0;
    {
        std::lock_guard<std::mutex> g(m_lock);
        e.Data1 = m_frameNumber;
        if (m_events.size() >= 64) {
            m_events.pop_front(); // never grow without bound if nobody waits
        }
        m_events.push_back(e);
    }
    DrainEvents();
}

// Pairs queued events with parked WAIT_EVENT requests until one side is empty. A request
// is only taken from the queue together with an event (under m_lock), so a waiter is never
// completed without one; the WAIT_EVENT handler parks under the same lock.
void AdapterContext::DrainEvents()
{
    if (!m_eventQueue) {
        return;
    }
    for (;;) {
        WDFREQUEST req = nullptr;
        PULSAR_EVENT_OUT next{};
        {
            std::lock_guard<std::mutex> g(m_lock);
            if (m_events.empty()) {
                return;
            }
            if (!NT_SUCCESS(WdfIoQueueRetrieveNextRequest(m_eventQueue, &req))) {
                return; // nobody waiting; the event stays queued
            }
            next = m_events.front();
            m_events.pop_front();
        }
        void* buf = nullptr;
        if (NT_SUCCESS(WdfRequestRetrieveOutputBuffer(req, sizeof(next), &buf, nullptr))) {
            memcpy(buf, &next, sizeof(next));
            WdfRequestCompleteWithInformation(req, STATUS_SUCCESS, sizeof(next));
        } else {
            WdfRequestComplete(req, STATUS_INVALID_PARAMETER);
        }
    }
}

} // namespace pulsar
