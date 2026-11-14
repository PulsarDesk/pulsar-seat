// SPDX-License-Identifier: GPL-3.0-only
//
// Frame channel producer (spec 40 §6.4): named page-file section with the v1 header,
// FrameReady event, ring of 3 named NT-shared textures and a named shared fence. Every
// object carries the protected DACL SYSTEM + LOCAL SERVICE (no NULL DACL, no Everyone;
// the LOCAL SERVICE grant is open question display-7). No object is ever used if its name
// already existed (squatting): the section and FrameReady fail the seat, a texture / fence
// name collision moves on to the next generation's names.
// Built unsigned (WDK 10.0.28000), untested, never loaded.

#include "Driver.h"

#include <sddl.h>
#include <string>

namespace pulsar {

static std::wstring GuidText(const GUID& g)
{
    wchar_t buf[40] = {};
    swprintf_s(buf, L"%08lx-%04hx-%04hx-%02x%02x-%02x%02x%02x%02x%02x%02x", g.Data1, g.Data2, g.Data3, g.Data4[0], g.Data4[1],
               g.Data4[2], g.Data4[3], g.Data4[4], g.Data4[5], g.Data4[6], g.Data4[7]);
    return buf;
}

FrameChannel::~FrameChannel() { Close(); }

std::wstring FrameChannel::Name(const wchar_t* suffix) const
{
    return L"Global\\PulsarSplit.Display." + m_seat + L"." + suffix;
}

HRESULT FrameChannel::CreateSecurity(const std::wstring& readerSid)
{
    if (m_sd) {
        LocalFree(m_sd);
        m_sd = nullptr;
    }
    // SYSTEM only, plus the seat's frame reader when the service named one (SET_SEAT_CONFIG
    // v2; the SID characters were checked there).
    std::wstring sddl = PULSAR_OBJECT_SDDL;
    if (!readerSid.empty()) {
        sddl += L"(A;;GA;;;" + readerSid + L")";
    }
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), SDDL_REVISION_1, &m_sd, nullptr)) {
        return HRESULT_FROM_WIN32(GetLastError());
    }
    m_sa.nLength = sizeof(m_sa);
    m_sa.lpSecurityDescriptor = m_sd;
    m_sa.bInheritHandle = FALSE;
    return S_OK;
}

bool FrameChannel::IsOpenFor(const GUID& seat) const
{
    return m_header != nullptr && IsEqualGUID(seat, m_seatGuid);
}

// Once per seat (first swap chain after SET_SEAT_CONFIG); kept until SHUTDOWN_SEAT.
HRESULT FrameChannel::Open(const GUID& seat, const std::wstring& readerSid)
{
    Close();
    HRESULT hr = CreateSecurity(readerSid);
    if (FAILED(hr)) return hr;
    m_seatGuid = seat;
    m_seat = GuidText(seat);

    // Global\ objects from session 0: the UMDF host (LOCAL SERVICE) holds SeCreateGlobalPrivilege.
    m_section = CreateFileMappingW(INVALID_HANDLE_VALUE, &m_sa, PAGE_READWRITE, 0, PULSAR_FRAME_SECTION_SIZE, Name(L"Header").c_str());
    if (!m_section) return HRESULT_FROM_WIN32(GetLastError());
    if (GetLastError() == ERROR_ALREADY_EXISTS) {
        // Someone created it first (squatting, or a consumer still holding the section of
        // an earlier driver host): refuse to publish into a foreign object.
        Close();
        return HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS);
    }
    m_header = static_cast<PULSAR_FRAME_HEADER_V1*>(MapViewOfFile(m_section, FILE_MAP_ALL_ACCESS, 0, 0, PULSAR_FRAME_SECTION_SIZE));
    if (!m_header) {
        hr = HRESULT_FROM_WIN32(GetLastError());
        Close();
        return hr;
    }
    m_frameReady = CreateEventW(&m_sa, FALSE, FALSE, Name(L"FrameReady").c_str());
    if (!m_frameReady || GetLastError() == ERROR_ALREADY_EXISTS) {
        hr = m_frameReady ? HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS) : HRESULT_FROM_WIN32(GetLastError());
        Close();
        return hr;
    }

    memset(m_header, 0, sizeof(*m_header));
    memcpy(m_header->Magic, PULSAR_FRAME_MAGIC, 4);
    m_header->Version = PULSAR_FRAME_VERSION;
    m_header->HeaderSize = sizeof(PULSAR_FRAME_HEADER_V1);
    memcpy(m_header->SeatGuid, &seat, sizeof(GUID));
    m_header->RingSize = PULSAR_FRAME_RING_SIZE;
    m_header->DxgiFormat = DXGI_FORMAT_B8G8R8A8_UNORM;
    m_header->ColorSpace = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
    Heartbeat(false, false);
    return S_OK;
}

// Per swap chain / D3D device: the old ring and fence belong to the old device. The next
// EnsureRing creates them on `device` under a new generation; the header stays.
HRESULT FrameChannel::AttachDevice(ID3D11Device5* device, LUID adapter)
{
    if (!m_header) return E_UNEXPECTED;
    ReleaseRing();
    m_device = device;
    m_adapterLuid = (static_cast<uint64_t>(static_cast<uint32_t>(adapter.HighPart)) << 32) | adapter.LowPart;
    m_header->AdapterLuid = m_adapterLuid;
    return S_OK;
}

void FrameChannel::ReleaseRing()
{
    for (UINT i = 0; i < PULSAR_FRAME_RING_SIZE; ++i) {
        if (m_ringHandles[i]) CloseHandle(m_ringHandles[i]);
        m_ringHandles[i] = nullptr;
        m_ring[i].Reset();
    }
    if (m_fenceHandle) CloseHandle(m_fenceHandle);
    m_fenceHandle = nullptr;
    m_fence.Reset();
    m_width = m_height = 0;
    m_format = DXGI_FORMAT_UNKNOWN;
}

static bool NameTaken(HRESULT hr)
{
    // DXGI reports an existing name as DXGI_ERROR_NAME_ALREADY_EXISTS; the fence path is
    // VERIFY on hardware (may surface the Win32 code instead).
    return hr == DXGI_ERROR_NAME_ALREADY_EXISTS || hr == HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS);
}

HRESULT FrameChannel::CreateRing(UINT width, UINT height, DXGI_FORMAT format)
{
    D3D11_TEXTURE2D_DESC d{};
    d.Width = width;
    d.Height = height;
    d.MipLevels = 1;
    d.ArraySize = 1;
    d.Format = format;
    d.SampleDesc.Count = 1;
    d.Usage = D3D11_USAGE_DEFAULT;
    d.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    d.MiscFlags = D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE;
    for (UINT i = 0; i < PULSAR_FRAME_RING_SIZE; ++i) {
        HRESULT hr = m_device->CreateTexture2D(&d, nullptr, &m_ring[i]);
        if (FAILED(hr)) return hr;
        ComPtr<IDXGIResource1> res;
        hr = m_ring[i].As(&res);
        if (FAILED(hr)) return hr;
        std::wstring n = Name((L"Tex" + std::to_wstring(m_generation) + L"." + std::to_wstring(i)).c_str());
        hr = res->CreateSharedHandle(&m_sa, DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, n.c_str(), &m_ringHandles[i]);
        if (FAILED(hr)) return hr;
    }
    HRESULT hr = m_device->CreateFence(0, D3D11_FENCE_FLAG_SHARED, IID_PPV_ARGS(&m_fence));
    if (FAILED(hr)) return hr;
    std::wstring fn = Name((L"Fence" + std::to_wstring(m_generation)).c_str());
    return m_fence->CreateSharedHandle(&m_sa, GENERIC_ALL, fn.c_str(), &m_fenceHandle);
}

HRESULT FrameChannel::EnsureRing(UINT width, UINT height, DXGI_FORMAT format)
{
    if (!m_device || !m_header) return E_UNEXPECTED;
    if (m_ring[0] && width == m_width && height == m_height && format == m_format) return S_OK;

    // New generation. Old textures are released here; the consumer re-opens by name after
    // it sees the new generation (spec allows release after ack or 1 s; we release at once
    // because the consumer keeps its own references to opened textures).
    ReleaseRing();
    HRESULT hr = E_FAIL;
    for (int attempt = 0; attempt < 8; ++attempt) {
        ++m_generation;
        hr = CreateRing(width, height, format);
        if (!NameTaken(hr)) break;
        // A foreign object holds one of this generation's names: never publish into it;
        // the consumer only opens the generation the header names, so skip to the next.
        ReleaseRing();
    }
    if (FAILED(hr)) {
        ReleaseRing();
        return hr;
    }
    m_fenceValue = 0;
    m_width = width;
    m_height = height;
    m_format = format;
    m_nextSlot = 0;
    m_header->Generation = m_generation;
    return S_OK;
}

HRESULT FrameChannel::Publish(ID3D11DeviceContext4* ctx, ID3D11Texture2D* surface, uint64_t frameNumber, UINT refreshMilliHz,
                              bool hdr, const PULSAR_HDR_META_V1& meta)
{
    if (!m_header) return E_UNEXPECTED;
    D3D11_TEXTURE2D_DESC sd{};
    surface->GetDesc(&sd);
    // Only the two contract formats (spec 40 §6.4).
    DXGI_FORMAT fmt = sd.Format == DXGI_FORMAT_R16G16B16A16_FLOAT ? DXGI_FORMAT_R16G16B16A16_FLOAT : DXGI_FORMAT_B8G8R8A8_UNORM;
    HRESULT hr = EnsureRing(sd.Width, sd.Height, fmt);
    if (FAILED(hr)) return hr;

    // Never write the slot the consumer may still read (the one at consumerFrame); if no
    // slot is free the oldest unread one is overwritten — DWM is never blocked.
    uint32_t busy = UINT32_MAX;
    uint64_t consumed = m_header->ConsumerFrame;
    if (consumed != 0 && consumed + PULSAR_FRAME_RING_SIZE > m_header->FrameNumber) {
        busy = static_cast<uint32_t>(m_header->LatestIndex - (m_header->FrameNumber - consumed)) % PULSAR_FRAME_RING_SIZE;
    }
    uint32_t slot = m_nextSlot;
    if (slot == busy) slot = (slot + 1) % PULSAR_FRAME_RING_SIZE;
    m_nextSlot = (slot + 1) % PULSAR_FRAME_RING_SIZE;

    // IddCx hands B8G8R8A8 (SDR) or R16G16B16A16_FLOAT (FP16) surfaces; both copy 1:1.
    // VERIFY on hardware: an sRGB / typeless variant would need a format-compatible copy.
    ctx->CopyResource(m_ring[slot].Get(), surface);
    ++m_fenceValue;
    hr = ctx->Signal(m_fence.Get(), m_fenceValue);
    if (FAILED(hr)) return hr;
    ctx->Flush();

    if (memcmp(&meta, &m_lastMeta, sizeof(meta)) != 0) {
        m_lastMeta = meta;
        ++m_hdrMetaGeneration;
    }
    LARGE_INTEGER qpc;
    QueryPerformanceCounter(&qpc);
    m_header->LatestIndex = slot;
    m_header->FenceValue = m_fenceValue;
    m_header->PresentQpc = static_cast<uint64_t>(qpc.QuadPart);
    m_header->Width = sd.Width;
    m_header->Height = sd.Height;
    m_header->RefreshMilliHz = refreshMilliHz;
    m_header->DxgiFormat = fmt;
    m_header->ColorSpace = fmt == DXGI_FORMAT_R16G16B16A16_FLOAT ? DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 : DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
    m_header->HdrMetaGeneration = m_hdrMetaGeneration;
    m_header->HdrMeta = meta;
    m_header->Flags = PULSAR_FRAME_FLAG_PATH_ACTIVE | (hdr ? PULSAR_FRAME_FLAG_HDR : 0);
    m_header->ProducerHeartbeat = static_cast<uint64_t>(qpc.QuadPart);
    MemoryBarrier();
    m_header->FrameNumber = frameNumber; // written last: consumers key on it
    SetEvent(m_frameReady);
    return S_OK;
}

void FrameChannel::Heartbeat(bool pathActive, bool hdr)
{
    if (!m_header) return;
    LARGE_INTEGER qpc;
    QueryPerformanceCounter(&qpc);
    uint32_t f = m_header->Flags & PULSAR_FRAME_FLAG_DRIVER_STOPPING;
    if (pathActive) f |= PULSAR_FRAME_FLAG_PATH_ACTIVE;
    if (hdr) f |= PULSAR_FRAME_FLAG_HDR;
    m_header->Flags = f;
    m_header->ProducerHeartbeat = static_cast<uint64_t>(qpc.QuadPart);
}

void FrameChannel::MarkStopping()
{
    if (m_header) {
        m_header->Flags |= PULSAR_FRAME_FLAG_DRIVER_STOPPING;
        if (m_frameReady) SetEvent(m_frameReady);
    }
}

void FrameChannel::Close()
{
    ReleaseRing();
    if (m_header) UnmapViewOfFile(m_header);
    m_header = nullptr;
    if (m_section) CloseHandle(m_section);
    m_section = nullptr;
    if (m_frameReady) CloseHandle(m_frameReady);
    m_frameReady = nullptr;
    m_device.Reset();
    m_seatGuid = GUID{};
    if (m_sd) LocalFree(m_sd);
    m_sd = nullptr;
}

} // namespace pulsar
