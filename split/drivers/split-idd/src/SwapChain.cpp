// SPDX-License-Identifier: GPL-3.0-only
//
// Swap-chain processing thread: acquire each DWM frame (IddCx 1.10 "2" call when
// available, FP16 surfaces for HDR / WCG), publish it to the frame channel, finish the
// frame. Never blocks DWM on the consumer.
// UNBUILT · UNTESTED · UNSIGNED.

#include "Driver.h"

#include <avrt.h>

namespace pulsar {

SwapChainWorker::SwapChainWorker(AdapterContext* adapter, IDDCX_SWAPCHAIN swapChain, HANDLE newFrameEvent)
    : m_adapter(adapter), m_swapChain(swapChain), m_newFrame(newFrameEvent)
{
    m_stop = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    m_thread = std::thread([this] { Run(); });
}

SwapChainWorker::~SwapChainWorker()
{
    if (m_stop) SetEvent(m_stop);
    if (m_thread.joinable()) m_thread.join();
    if (m_stop) CloseHandle(m_stop);
    // The OS owns the swap chain object; deleting it tells IddCx we are done with it.
    WdfObjectDelete(reinterpret_cast<WDFOBJECT>(m_swapChain));
}

HRESULT SwapChainWorker::CreateDevice()
{
    ComPtr<IDXGIFactory4> factory;
    HRESULT hr = CreateDXGIFactory2(0, IID_PPV_ARGS(&factory));
    if (FAILED(hr)) return hr;
    // The OS picks the render adapter (honouring IddCxAdapterSetRenderAdapter); the swap
    // chain's adapter is reported through IddCxSwapChainSetDevice's requirement that the
    // device sits on the same LUID, so enumerate by the LUID we asked for, else default.
    ComPtr<IDXGIAdapter1> adapter;
    LUID want = m_adapter->RenderAdapter();
    if (want.LowPart || want.HighPart) {
        factory->EnumAdapterByLuid(want, IID_PPV_ARGS(&adapter));
    }
    if (!adapter) {
        hr = factory->EnumAdapters1(0, &adapter);
        if (FAILED(hr)) return hr;
    }
    ComPtr<ID3D11Device> dev;
    ComPtr<ID3D11DeviceContext> ctx;
    D3D_FEATURE_LEVEL fl;
    hr = D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0,
                           D3D11_SDK_VERSION, &dev, &fl, &ctx);
    if (FAILED(hr)) return hr;
    hr = dev.As(&m_device);
    if (FAILED(hr)) return hr;
    hr = ctx.As(&m_context);
    if (FAILED(hr)) return hr;

    ComPtr<IDXGIDevice> dxgi;
    hr = m_device.As(&dxgi);
    if (FAILED(hr)) return hr;
    IDARG_IN_SWAPCHAINSETDEVICE sd{};
    sd.pDevice = dxgi.Get();
    hr = IddCxSwapChainSetDevice(m_swapChain, &sd);
    if (FAILED(hr)) return hr;
    return S_OK;
}

void SwapChainWorker::Run()
{
    DWORD task = 0;
    HANDLE mmcss = AvSetMmThreadCharacteristicsW(L"Distribution", &task);

    if (FAILED(CreateDevice())) {
        // Deleting the swap chain (destructor) makes the OS retry with a new one.
        if (mmcss) AvRevertMmThreadCharacteristics(mmcss);
        return;
    }

    // The channel's section + FrameReady are created once per seat and outlive swap chains
    // (the stream server keeps them open); this worker only binds its own device to it.
    // Until the seat is bound, frames are released unpublished. Only this thread touches
    // the channel while the worker lives (see FrameChannel in Driver.h).
    auto& channel = m_adapter->Channel();
    bool channelOpen = false;
    uint64_t frames = m_adapter->LastFrameNumber(); // monotonic per seat across swap chains

    HANDLE waits[2] = {m_newFrame, m_stop};
    for (;;) {
        ComPtr<ID3D11Texture2D> surface;
        uint64_t presentNumber = 0;
        HRESULT hr;
        if (m_adapter->Has110()) {
            IDARG_IN_RELEASEANDACQUIREBUFFER2 in{};
            in.Size = sizeof(in);
            in.AcquireSystemMemoryBuffer = FALSE;
            IDARG_OUT_RELEASEANDACQUIREBUFFER2 out{};
            hr = IddCxSwapChainReleaseAndAcquireBuffer2(m_swapChain, &in, &out);
            if (SUCCEEDED(hr)) {
                presentNumber = out.MetaData.PresentationFrameNumber;
                if (out.MetaData.pSurface) out.MetaData.pSurface->QueryInterface(IID_PPV_ARGS(&surface));
            }
        } else {
            IDARG_OUT_RELEASEANDACQUIREBUFFER out{};
            hr = IddCxSwapChainReleaseAndAcquireBuffer(m_swapChain, &out);
            if (SUCCEEDED(hr)) {
                presentNumber = out.MetaData.PresentationFrameNumber;
                if (out.MetaData.pSurface) out.MetaData.pSurface->QueryInterface(IID_PPV_ARGS(&surface));
            }
        }

        if (hr == E_PENDING) {
            DWORD w = WaitForMultipleObjects(2, waits, FALSE, kHeartbeatMs);
            if (w == WAIT_OBJECT_0 + 1) break;
            if (w == WAIT_TIMEOUT && channelOpen) {
                channel.Heartbeat(true, m_adapter->HdrActive()); // at least every 500 ms
            }
            continue;
        }
        if (FAILED(hr)) {
            break; // swap chain abandoned (mode change, adapter stop): the OS assigns a new one
        }

        if (!channelOpen) {
            // Frame objects are named after the seat GUID, so publish only once the seat is bound.
            GUID seat{};
            if (m_adapter->SeatGuid(&seat)) {
                HRESULT hr2 = S_OK;
                if (!channel.IsOpenFor(seat)) {
                    hr2 = channel.Open(seat); // first swap chain of this seat: create + squat check
                }
                if (SUCCEEDED(hr2) && !channel.HasDevice(m_device.Get())) {
                    LUID luid{};
                    ComPtr<IDXGIDevice> dx;
                    ComPtr<IDXGIAdapter> ad;
                    if (SUCCEEDED(m_device.As(&dx)) && SUCCEEDED(dx->GetAdapter(&ad))) {
                        DXGI_ADAPTER_DESC desc{};
                        ad->GetDesc(&desc);
                        luid = desc.AdapterLuid; // the GPU actually rendering, reported in the header
                    }
                    hr2 = channel.AttachDevice(m_device.Get(), luid); // new ring + fence generation
                }
                channelOpen = SUCCEEDED(hr2);
            }
        }

        if (channelOpen && surface) {
            ++frames;
            if (SUCCEEDED(channel.Publish(m_context.Get(), surface.Get(), frames, m_adapter->RefreshMilliHz(), m_adapter->HdrActive(),
                                          m_adapter->HdrMeta()))) {
                m_adapter->OnFramePublished(frames);
            }
        }
        (void)presentNumber; // the OS number restarts per swap chain; the header uses ours
        IddCxSwapChainFinishedProcessingFrame(m_swapChain);
        if (WaitForSingleObject(m_stop, 0) == WAIT_OBJECT_0) break;
    }
    if (mmcss) AvRevertMmThreadCharacteristics(mmcss);
}

} // namespace pulsar
