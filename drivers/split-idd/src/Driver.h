// SPDX-License-Identifier: GPL-3.0-only
//
// Pulsar Split remote-session indirect display driver (UMDF 2 + IddCx 1.10).
// UNBUILT · UNTESTED · UNSIGNED — do not install. See ../README.md.
//
// Written from Microsoft Learn's IddCx documentation (no sample code copied, see OQ 26).
// Behaviour: spec/40-display-hdr.md §2.3, §3.3, §4.4, §5, §6.4, §7, §8.4.

#pragma once

#define NOMINMAX
#include <windows.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <iddcx.h>

#include <d3d11_4.h>
#include <dxgi1_6.h>
#include <wrl/client.h>

#include <atomic>
#include <deque>
#include <memory>
#include <mutex>
#include <thread>

#include "PulsarIddShared.h"

namespace pulsar {

using Microsoft::WRL::ComPtr;

constexpr uint16_t kDriverVersion[4] = {0, 1, 0, 0};
constexpr UINT kMaxMonitors = 1;               // one monitor per seat session
constexpr UINT kMinHz = 15, kMaxHz = 500;
constexpr UINT kDefaultWidth = 1920, kDefaultHeight = 1080, kDefaultHz = 60;
constexpr uint64_t kMaxPipelineRate = 7680ull * 4320ull * 120ull; // pixels/s; also capped by the service
constexpr DWORD kHeartbeatMs = 500;

// Seat configuration from SET_SEAT_CONFIG (all values validated).
struct SeatConfig {
    GUID seat{};
    bool configured = false;
    bool hdrAllowed = false;
    bool adapterPinned = false;
    LUID adapter{};
    UINT sdrWhiteNits = 400;
    UINT minLuminanceX10000 = 0;
    UINT maxLuminanceNits = 1000;
    UINT maxFullFrameNits = 1000;
    bool scaleStatic = false;
    UINT scalePercent = 100;
    UINT superSamplingPercent = 100;
};

struct ModeState {
    UINT width = 0, height = 0, hz = 0;     // stream size
    UINT renderWidth = 0, renderHeight = 0; // target mode
    UINT scalePercent = 100;
    bool hdr = false;
};

// Named per-seat objects of spec 40 §6.4 (section, event, texture ring, fence).
//
// The header section and FrameReady live for the whole seat: created once (Open, with the
// squatting check) on the first swap chain after SET_SEAT_CONFIG and kept until
// SHUTDOWN_SEAT, because the stream server holds them open across swap chains. A new swap
// chain / D3D device only re-binds the ring and fence (AttachDevice: next frame = new
// generation, same header).
//
// Threading: no lock of its own. Only the one live SwapChainWorker thread touches it;
// AdapterContext calls MarkStopping / Close only under m_lifecycle after that worker has
// been joined (worker lifetimes never overlap, also under m_lifecycle).
class FrameChannel {
public:
    ~FrameChannel();
    bool IsOpenFor(const GUID& seat) const;
    HRESULT Open(const GUID& seat);                   // section + FrameReady; refuses existing names
    HRESULT AttachDevice(ID3D11Device5* device, LUID adapter); // drops ring + fence of the old device
    bool HasDevice(const ID3D11Device5* device) const { return m_device.Get() == device; }
    HRESULT EnsureRing(UINT width, UINT height, DXGI_FORMAT format); // bumps the generation on change
    // Copies `surface` into a free ring slot, signals the fence, updates the header, sets FrameReady.
    HRESULT Publish(ID3D11DeviceContext4* ctx, ID3D11Texture2D* surface, uint64_t frameNumber, UINT refreshMilliHz,
                    bool hdr, const PULSAR_HDR_META_V1& meta);
    void Heartbeat(bool pathActive, bool hdr);
    void MarkStopping();
    void Close();

private:
    HRESULT CreateSecurity();
    HRESULT CreateRing(UINT width, UINT height, DXGI_FORMAT format);
    void ReleaseRing();
    std::wstring Name(const wchar_t* suffix) const;

    std::wstring m_seat;
    GUID m_seatGuid{};
    SECURITY_ATTRIBUTES m_sa{};
    PSECURITY_DESCRIPTOR m_sd = nullptr;
    HANDLE m_section = nullptr;
    HANDLE m_frameReady = nullptr;
    PULSAR_FRAME_HEADER_V1* m_header = nullptr;
    ComPtr<ID3D11Device5> m_device;
    ComPtr<ID3D11Texture2D> m_ring[PULSAR_FRAME_RING_SIZE];
    HANDLE m_ringHandles[PULSAR_FRAME_RING_SIZE] = {};
    ComPtr<ID3D11Fence> m_fence;
    HANDLE m_fenceHandle = nullptr;
    uint64_t m_fenceValue = 0;
    uint32_t m_generation = 0;
    UINT m_width = 0, m_height = 0;
    DXGI_FORMAT m_format = DXGI_FORMAT_UNKNOWN;
    uint32_t m_nextSlot = 0;
    uint64_t m_adapterLuid = 0;
    uint32_t m_hdrMetaGeneration = 0;
    PULSAR_HDR_META_V1 m_lastMeta{};
};

class AdapterContext;

// One processing thread per assigned swap chain.
class SwapChainWorker {
public:
    SwapChainWorker(AdapterContext* adapter, IDDCX_SWAPCHAIN swapChain, HANDLE newFrameEvent);
    ~SwapChainWorker();

private:
    void Run();
    HRESULT CreateDevice();
    AdapterContext* m_adapter;
    IDDCX_SWAPCHAIN m_swapChain;
    HANDLE m_newFrame;
    HANDLE m_stop;
    ComPtr<ID3D11Device5> m_device;
    ComPtr<ID3D11DeviceContext4> m_context;
    std::thread m_thread;
};

// Per-device state (one adapter, one monitor, one seat).
class AdapterContext {
public:
    explicit AdapterContext(WDFDEVICE device);
    ~AdapterContext();

    NTSTATUS InitAdapter();
    void OnInitFinished(const IDARG_IN_ADAPTER_INIT_FINISHED* args);
    NTSTATUS CreateMonitor();
    // colorKnown: the COMMITMODES2 path, which reports the colour mode (hdr is meaningful).
    void OnCommitModes(bool pathActive, const DISPLAYCONFIG_TARGET_MODE* mode, bool hdr, bool colorKnown);
    NTSTATUS AssignSwapChain(IDDCX_SWAPCHAIN swapChain, HANDLE newFrameEvent);
    void UnassignSwapChain();

    // IOCTL handlers (Control.cpp)
    void DeviceIoControl(WDFREQUEST request, size_t outLen, size_t inLen, ULONG code);
    void CancelPendingMode(WDFREQUEST request);

    // Mode rules (Modes.cpp)
    ModeState ComputeMode(UINT w, UINT h, UINT hz, bool wantHdr) const;
    NTSTATUS ApplyMode(const ModeState& m);
    void FillTargetModes2(IDDCX_TARGET_MODE2* modes, UINT count) const;
    void FillTargetModes(IDDCX_TARGET_MODE* modes, UINT count) const;

    // Used by the swap-chain worker
    LUID RenderAdapter() const;
    bool SeatGuid(GUID* out) const;
    bool Has110() const { return m_has110; }
    FrameChannel& Channel() { return m_channel; }
    PULSAR_HDR_META_V1 HdrMeta() const;
    UINT RefreshMilliHz() const;
    void OnFramePublished(uint64_t frameNumber);
    uint64_t LastFrameNumber() const;
    bool HdrActive() const;

    IDDCX_ADAPTER Adapter() const { return m_adapter; }

private:
    void Event(uint32_t kind, uint32_t data0);
    void DrainEvents();
    // Pending REQUEST_MODE, all under m_lock. Take...Locked returns the request to finish
    // (after releasing m_lock) or nullptr; while the request is not yet marked cancelable
    // it only records the status, and the REQUEST_MODE handler finishes it after marking.
    WDFREQUEST TakePendingModeLocked(uint32_t status, PULSAR_MODE_OUT* out);
    bool PendingModeCommittedLocked(uint32_t* status) const;
    static void FinishModeRequest(WDFREQUEST req, const PULSAR_MODE_OUT& out);
    void FinishTaken(WDFREQUEST req, const PULSAR_MODE_OUT& out);
    uint32_t StateFlags() const;

    WDFDEVICE m_device;
    IDDCX_ADAPTER m_adapter = nullptr;
    WDFQUEUE m_eventQueue = nullptr;   // manual queue of pended WAIT_EVENT requests

    // Lock order: m_control -> m_lifecycle -> m_lock. No IddCx call is made while holding
    // m_lifecycle or m_lock (IddCx may call back into AssignSwapChain / OnCommitModes on
    // the same thread); m_control is held across IddCx calls, and no callback takes it.
    //
    // m_control serialises the control paths that create, use or depart the monitor:
    // SET_SEAT_CONFIG, REQUEST_MODE (ApplyMode), SHUTDOWN_SEAT and CreateMonitor.
    std::mutex m_control;
    IDDCX_MONITOR m_monitor = nullptr;   // guarded by m_control
    // m_lifecycle serialises AssignSwapChain, UnassignSwapChain, SHUTDOWN_SEAT and the
    // destructor: the worker is stopped and joined before the next one starts and before
    // the channel closes. Guards m_worker, m_shutdown and the channel's Close.
    std::mutex m_lifecycle;
    bool m_shutdown = false;             // after SHUTDOWN_SEAT until the next SET_SEAT_CONFIG

    mutable std::mutex m_lock;
    SeatConfig m_seat;
    ModeState m_mode;            // requested / committed mode
    ModeState m_pendingMode;
    WDFREQUEST m_pendingModeRequest = nullptr;
    uint32_t m_pendingModeId = 0;
    bool m_pendingModeArmed = false;     // WdfRequestMarkCancelableEx succeeded
    bool m_pendingModeDeferred = false;  // completion requested before it was armed
    uint32_t m_pendingModeDeferredStatus = 0;
    UINT m_committedHz = 0;              // refresh of the last committed path
    bool m_lastCommitColorKnown = false;
    bool m_adapterReady = false;
    bool m_monitorPresent = false;
    bool m_pathActive = false;
    bool m_hdrActive = false;
    bool m_firstFrame = false;
    uint64_t m_frameNumber = 0;
    NTSTATUS m_lastError = 0;
    UINT m_iddcxVersion = 0;
    bool m_has110 = false;
    std::deque<PULSAR_EVENT_OUT> m_events;

    FrameChannel m_channel;
    std::unique_ptr<SwapChainWorker> m_worker;

};

struct DeviceContextWrapper {
    AdapterContext* ctx;
};
struct AdapterObjectWrapper {
    AdapterContext* ctx;
};
struct MonitorObjectWrapper {
    AdapterContext* ctx;
};

} // namespace pulsar

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(pulsar::DeviceContextWrapper, GetDeviceWrapper);
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(pulsar::AdapterObjectWrapper, GetAdapterWrapper);
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(pulsar::MonitorObjectWrapper, GetMonitorWrapper);
