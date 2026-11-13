// SPDX-License-Identifier: GPL-3.0-only
//
// Control channel (spec 40 §7): versioned buffered IOCTLs from the service.
// UNBUILT · UNTESTED · UNSIGNED.
//
// Locking (see Driver.h): m_control -> m_lifecycle -> m_lock. WDF request calls that can
// run a cancel routine synchronously (WdfRequestMarkCancelableEx) or complete a request
// are never made while holding m_lock, because the cancel routine takes m_lock.

#include "Driver.h"

namespace pulsar {

template <typename T> static bool InputOk(const T* p, size_t len)
{
    return p && len >= sizeof(T) && p->Header.Version == PULSAR_BUFFER_VERSION && p->Header.Size >= sizeof(T) &&
           p->Header.Size <= len;
}

static bool HeaderOk(const PULSAR_BUF_HEADER* h, size_t len)
{
    return h && len >= sizeof(*h) && h->Version == PULSAR_BUFFER_VERSION && h->Size >= sizeof(*h) && h->Size <= len;
}

template <typename T> static void InitOut(T& o)
{
    o = {};
    o.Header.Size = sizeof(T);
    o.Header.Version = PULSAR_BUFFER_VERSION;
}

template <typename T> static void CompleteWith(WDFREQUEST req, const T& out)
{
    void* buf = nullptr;
    NTSTATUS st = WdfRequestRetrieveOutputBuffer(req, sizeof(T), &buf, nullptr);
    if (!NT_SUCCESS(st)) {
        WdfRequestComplete(req, STATUS_INVALID_PARAMETER);
        return;
    }
    memcpy(buf, &out, sizeof(T));
    WdfRequestCompleteWithInformation(req, STATUS_SUCCESS, sizeof(T));
}

// Caller holds m_lock. Never touches the WDF request itself.
WDFREQUEST AdapterContext::TakePendingModeLocked(uint32_t status, PULSAR_MODE_OUT* out)
{
    if (!m_pendingModeRequest) {
        return nullptr;
    }
    if (!m_pendingModeArmed) {
        // Not yet cancelable: the REQUEST_MODE handler finishes it right after marking.
        m_pendingModeDeferred = true;
        m_pendingModeDeferredStatus = status;
        return nullptr;
    }
    InitOut(*out);
    out->RequestId = m_pendingModeId;
    out->Status = status;
    out->Width = m_mode.width;
    out->Height = m_mode.height;
    out->RefreshHz = m_mode.hz;
    out->Hdr = m_hdrActive ? 1 : 0;
    out->RenderWidth = m_mode.renderWidth;
    out->RenderHeight = m_mode.renderHeight;
    out->DxgiFormat = m_hdrActive ? DXGI_FORMAT_R16G16B16A16_FLOAT : DXGI_FORMAT_B8G8R8A8_UNORM;
    out->ScalePercent = m_mode.scalePercent;
    WDFREQUEST req = m_pendingModeRequest;
    m_pendingModeRequest = nullptr;
    m_pendingModeArmed = false;
    m_pendingModeDeferred = false;

    PULSAR_EVENT_OUT e;
    InitOut(e);
    e.Kind = (status == PULSAR_MODE_OK || status == PULSAR_MODE_OK_SDR_FALLBACK) ? PULSAR_EVENT_MODE_APPLIED : PULSAR_EVENT_MODE_REJECTED;
    e.Data0 = out->RequestId;
    e.Data1 = m_frameNumber;
    if (m_events.size() >= 64) {
        m_events.pop_front();
    }
    m_events.push_back(e); // delivered by DrainEvents once m_lock is released
    return req;
}

// Caller holds m_lock. True when the committed path equals the pending target mode.
bool AdapterContext::PendingModeCommittedLocked(uint32_t* status) const
{
    const ModeState& p = m_pendingMode;
    if (!m_pathActive || m_mode.renderWidth != p.renderWidth || m_mode.renderHeight != p.renderHeight || m_committedHz != p.hz) {
        return false;
    }
    if (m_hdrActive == p.hdr) {
        *status = PULSAR_MODE_OK;
        return true;
    }
    // HDR asked, the OS committed SDR at the target size: its answer, if the commit
    // reported the colour mode at all (COMMITMODES2).
    if (p.hdr && !m_hdrActive && m_lastCommitColorKnown) {
        *status = PULSAR_MODE_OK_SDR_FALLBACK;
        return true;
    }
    return false;
}

// Without m_lock. A request already in its cancel routine is completed there.
void AdapterContext::FinishModeRequest(WDFREQUEST req, const PULSAR_MODE_OUT& out)
{
    if (NT_SUCCESS(WdfRequestUnmarkCancelable(req))) {
        CompleteWith(req, out);
    }
}

void AdapterContext::FinishTaken(WDFREQUEST req, const PULSAR_MODE_OUT& out)
{
    if (req) {
        FinishModeRequest(req, out);
    }
    DrainEvents();
}

// Cancel routine (runs only after WdfRequestMarkCancelableEx succeeded). Takes m_lock,
// so nothing that can run it synchronously may hold m_lock.
void AdapterContext::CancelPendingMode(WDFREQUEST request)
{
    {
        std::lock_guard<std::mutex> g(m_lock);
        if (m_pendingModeRequest == request) {
            m_pendingModeRequest = nullptr; // never touch the handle again after completion
            m_pendingModeArmed = false;
            m_pendingModeDeferred = false;
        }
    }
    WdfRequestComplete(request, STATUS_CANCELLED);
}

static VOID EvtModeRequestCancel(WDFREQUEST request)
{
    // The service gave up (its 5 s timeout cancels the I/O).
    WDFDEVICE dev = WdfIoQueueGetDevice(WdfRequestGetIoQueue(request));
    GetDeviceWrapper(dev)->ctx->CancelPendingMode(request);
}

static bool SeatValuesOk(const PULSAR_SEAT_CONFIG_IN* in)
{
    auto pct = [](uint32_t v) { return v >= 100 && v <= 500; };
    if (in->SdrWhiteNits < 80 || in->SdrWhiteNits > 480) return false;
    if (!pct(in->SuperSamplingPercent)) return false;
    if (in->ScaleStatic && !pct(in->ScalePercent)) return false;
    // luminance rule of spec 40 §8.2: min < max full-frame <= max
    uint64_t ff = static_cast<uint64_t>(in->MaxFullFrameNits) * 10000u;
    if (!(in->MinLuminanceX10000 < ff && in->MaxFullFrameNits <= in->MaxLuminanceNits)) return false;
    return true;
}

void AdapterContext::DeviceIoControl(WDFREQUEST request, size_t outLen, size_t inLen, ULONG code)
{
    void* inBuf = nullptr;
    if (inLen) {
        if (!NT_SUCCESS(WdfRequestRetrieveInputBuffer(request, inLen, &inBuf, nullptr))) {
            WdfRequestComplete(request, STATUS_INVALID_PARAMETER);
            return;
        }
    }
    auto* hdr = static_cast<const PULSAR_BUF_HEADER*>(inBuf);

    switch (code) {
    case IOCTL_PULSAR_GET_INFO: {
        if (!HeaderOk(hdr, inLen) || outLen < sizeof(PULSAR_INFO_OUT)) break;
        PULSAR_INFO_OUT o;
        InitOut(o);
        memcpy(o.DriverVersion, kDriverVersion, sizeof(o.DriverVersion));
        o.IddCxVersion = m_iddcxVersion;
        o.Flags = PULSAR_INFO_REMOTE_SESSION;
        if (m_has110) o.Flags |= PULSAR_INFO_FP16_CAPABLE | PULSAR_INFO_HDR_CAPABLE | PULSAR_INFO_IDDCX_1_10_FUNCTIONS;
        o.MaxPipelineRate = kMaxPipelineRate;
        CompleteWith(request, o);
        return;
    }
    case IOCTL_PULSAR_SET_SEAT_CONFIG: {
        auto* in = static_cast<const PULSAR_SEAT_CONFIG_IN*>(inBuf);
        if (!InputOk(in, inLen) || !SeatValuesOk(in)) break;
        std::lock_guard<std::mutex> control(m_control);
        bool adapterChanged = false;
        bool adapterReady = false;
        {
            std::lock_guard<std::mutex> g(m_lock);
            SeatConfig s;
            memcpy(&s.seat, in->SeatGuid, sizeof(GUID));
            s.configured = true;
            s.hdrAllowed = (in->Flags & PULSAR_SEAT_HDR_ALLOWED) != 0;
            s.adapterPinned = (in->Flags & PULSAR_SEAT_ADAPTER_PINNED) != 0;
            s.adapter.LowPart = in->AdapterLuidLow;
            s.adapter.HighPart = in->AdapterLuidHigh;
            s.sdrWhiteNits = in->SdrWhiteNits;
            s.minLuminanceX10000 = in->MinLuminanceX10000;
            s.maxLuminanceNits = in->MaxLuminanceNits;
            s.maxFullFrameNits = in->MaxFullFrameNits;
            s.scaleStatic = in->ScaleStatic != 0;
            s.scalePercent = in->ScalePercent;
            s.superSamplingPercent = in->SuperSamplingPercent;
            adapterChanged = s.adapterPinned &&
                             (!m_seat.adapterPinned || s.adapter.LowPart != m_seat.adapter.LowPart || s.adapter.HighPart != m_seat.adapter.HighPart);
            m_seat = s;
            adapterReady = m_adapterReady;
        }
        {
            // A configured seat accepts swap chains again (after a SHUTDOWN_SEAT).
            std::lock_guard<std::mutex> l(m_lifecycle);
            m_shutdown = false;
        }
        // Render GPU through the documented call (IddCx 1.4+, spec 40 §5); the OS then
        // re-creates the swap chain on that adapter.
        if (adapterChanged && m_adapter) {
            IDARG_IN_ADAPTERSETRENDERADAPTER ra{};
            ra.PreferredRenderAdapter = RenderAdapter();
            IddCxAdapterSetRenderAdapter(m_adapter, &ra);
        }
        // Re-attach after SHUTDOWN_SEAT on the same device: the monitor was departed.
        if (!m_monitor && adapterReady && m_adapter) {
            NTSTATUS st = CreateMonitor();
            if (!NT_SUCCESS(st)) {
                {
                    std::lock_guard<std::mutex> g(m_lock);
                    m_lastError = st;
                }
                WdfRequestComplete(request, st);
                return;
            }
        }
        WdfRequestComplete(request, STATUS_SUCCESS);
        return;
    }
    case IOCTL_PULSAR_REQUEST_MODE: {
        auto* in = static_cast<const PULSAR_MODE_IN*>(inBuf);
        if (!InputOk(in, inLen) || outLen < sizeof(PULSAR_MODE_OUT)) break;
        if (in->Width < 320 || in->Height < 320 || in->Width > 16384 || in->Height > 16384 || in->RefreshHz < kMinHz || in->RefreshHz > kMaxHz) {
            break; // the service clamps first; anything else is a bad request
        }
        ModeState m;
        NTSTATUS early = STATUS_SUCCESS; // completed below, outside the lock
        {
            std::lock_guard<std::mutex> g(m_lock);
            if (!m_seat.configured || !m_adapterReady || !m_monitorPresent) {
                early = STATUS_DEVICE_NOT_READY;
            } else if (m_pendingModeRequest) {
                early = STATUS_DEVICE_BUSY;
            } else {
                m = ComputeMode(in->Width, in->Height, in->RefreshHz, in->Hdr != 0);
                // Stored first, armed after marking (outside the lock, see below).
                m_pendingMode = m;
                m_pendingModeId = in->RequestId;
                m_pendingModeRequest = request;
                m_pendingModeArmed = false;
                m_pendingModeDeferred = false;
            }
        }
        if (early != STATUS_SUCCESS) {
            WdfRequestComplete(request, early);
            return;
        }

        // Outside m_lock: the framework never runs the cancel routine from the Ex call;
        // an already-cancelled request comes back as STATUS_CANCELLED for us to complete.
        NTSTATUS mk = WdfRequestMarkCancelableEx(request, EvtModeRequestCancel);
        WDFREQUEST finish = nullptr;
        PULSAR_MODE_OUT out{};
        {
            std::lock_guard<std::mutex> g(m_lock);
            if (!NT_SUCCESS(mk)) {
                if (m_pendingModeRequest == request) {
                    m_pendingModeRequest = nullptr;
                    m_pendingModeDeferred = false;
                }
            } else if (m_pendingModeRequest == request) {
                // (not equal: the cancel routine already ran and completed it)
                m_pendingModeArmed = true;
                if (m_pendingModeDeferred) {
                    // completed (shutdown / commit) while we were marking it
                    finish = TakePendingModeLocked(m_pendingModeDeferredStatus, &out);
                }
            }
        }
        if (!NT_SUCCESS(mk)) {
            WdfRequestComplete(request, mk);
            return;
        }
        if (finish) {
            FinishTaken(finish, out);
            return;
        }

        NTSTATUS st;
        {
            std::lock_guard<std::mutex> control(m_control);
            st = ApplyMode(m); // may run OnCommitModes synchronously (takes m_lock only)
        }
        {
            std::lock_guard<std::mutex> g(m_lock);
            uint32_t status = PULSAR_MODE_OK;
            if (!NT_SUCCESS(st)) {
                m_lastError = st;
                finish = TakePendingModeLocked(PULSAR_MODE_REJECTED, &out);
            } else if (m_pendingModeRequest == request && PendingModeCommittedLocked(&status)) {
                // The path already equals the target: the OS may not commit again.
                m_mode = m_pendingMode;
                m_mode.hdr = m_hdrActive;
                finish = TakePendingModeLocked(status, &out);
            }
        }
        if (finish) {
            FinishTaken(finish, out);
        }
        // else completed from OnCommitModes once the OS committed the path, or cancelled
        // by the service's 5 s timeout.
        return;
    }
    case IOCTL_PULSAR_GET_STATE: {
        if (!HeaderOk(hdr, inLen) || outLen < sizeof(PULSAR_STATE_OUT)) break;
        PULSAR_STATE_OUT o;
        InitOut(o);
        bool configured;
        {
            std::lock_guard<std::mutex> g(m_lock);
            configured = m_seat.configured;
            o.Flags = StateFlags();
            o.Width = m_mode.width;
            o.Height = m_mode.height;
            o.RefreshHz = m_mode.hz;
            o.RenderWidth = m_mode.renderWidth;
            o.RenderHeight = m_mode.renderHeight;
            o.DxgiFormat = m_pathActive ? (m_hdrActive ? DXGI_FORMAT_R16G16B16A16_FLOAT : DXGI_FORMAT_B8G8R8A8_UNORM) : 0;
            o.FrameNumber = m_frameNumber;
            o.LastError = static_cast<uint32_t>(m_lastError);
        }
        if (!configured) {
            WdfRequestComplete(request, STATUS_DEVICE_NOT_READY);
            return;
        }
        CompleteWith(request, o);
        return;
    }
    case IOCTL_PULSAR_WAIT_EVENT: {
        if (!HeaderOk(hdr, inLen) || outLen < sizeof(PULSAR_EVENT_OUT)) break;
        PULSAR_EVENT_OUT e;
        bool have = false;
        NTSTATUS fw = STATUS_SUCCESS;
        {
            // Check and park under one lock: Event() queues under the same lock and then
            // drains into parked requests, so no event can slip between the two steps.
            // WdfRequestForwardToIoQueue calls nothing of ours.
            std::lock_guard<std::mutex> g(m_lock);
            if (!m_events.empty()) {
                e = m_events.front();
                m_events.pop_front();
                have = true;
            } else if (m_eventQueue) {
                fw = WdfRequestForwardToIoQueue(request, m_eventQueue);
            } else {
                fw = STATUS_DEVICE_BUSY;
            }
        }
        if (have) {
            CompleteWith(request, e);
        } else if (!NT_SUCCESS(fw)) {
            WdfRequestComplete(request, STATUS_DEVICE_BUSY);
        }
        return;
    }
    case IOCTL_PULSAR_SHUTDOWN_SEAT: {
        if (!HeaderOk(hdr, inLen)) break;
        std::lock_guard<std::mutex> control(m_control);
        {
            // Stop and join the worker before the channel goes; refuse new swap chains
            // until the seat is configured again.
            std::lock_guard<std::mutex> l(m_lifecycle);
            m_shutdown = true;
            m_worker.reset();
            m_channel.MarkStopping();
            m_channel.Close();
        }
        // Outside m_lifecycle: departure may unassign the swap chain synchronously.
        IDDCX_MONITOR monitor = m_monitor;
        m_monitor = nullptr;
        if (monitor) {
            IddCxMonitorDeparture(monitor);
        }
        WDFREQUEST finish = nullptr;
        PULSAR_MODE_OUT out{};
        {
            std::lock_guard<std::mutex> g(m_lock);
            m_monitorPresent = m_pathActive = m_hdrActive = m_firstFrame = false;
            m_seat = SeatConfig{};
            finish = TakePendingModeLocked(PULSAR_MODE_NO_DISPLAY, &out);
        }
        if (finish) {
            FinishTaken(finish, out);
        }
        Event(PULSAR_EVENT_DEVICE_STOPPING, 0);
        if (m_eventQueue) {
            WdfIoQueuePurgeSynchronously(m_eventQueue); // completes other pended WAIT_EVENTs (cancelled)
            WdfIoQueueStart(m_eventQueue);
        }
        WdfRequestComplete(request, STATUS_SUCCESS);
        return;
    }
    default:
        break;
    }
    WdfRequestComplete(request, STATUS_INVALID_PARAMETER);
}

} // namespace pulsar
