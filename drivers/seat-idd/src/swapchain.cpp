/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat IddCx driver: frame sections and swap-chain processing.
 *
 * Per virtual monitor one thread:
 *   D3D11 device on the render adapter LUID the OS hands us → IddCxSwapChainSetDevice →
 *   loop { IddCxSwapChainReleaseAndAcquireBuffer; E_PENDING → wait for the next surface;
 *          copy the surface to a CPU staging texture; Map; write it into the section under
 *          the sequence lock; IddCxSwapChainFinishedProcessingFrame; signal Ready }.
 * The hardware cursor (IddCxMonitorSetupHardwareCursor / QueryHardwareCursor) is read on
 * its own event in the same wait and written to the section's cursor area.
 *
 * CPU copy through shared memory is the documented fallback of spec 40 §6.3.4; passing a
 * shared GPU texture to the presenter is open question 2. Built unsigned, never loaded,
 * untested on hardware.
 */
#include "driver.h"

static void SectionName(const PSIDD_MONITOR *m, WCHAR *out, size_t cch)
{
    /* Global\PulsarSeat.Idd.S<NN>.C<n> — same as idd_proto::section_name. */
    swprintf_s(out, cch, L"Global\\PulsarSeat.Idd.S%02u.C%u", m->Seat, m->Connector);
}

/* Global\PulsarSeat.Idd.S<NN>.C<n>.Ready.<32 lower-case hex digits of ReadyNonce> — same as
 * idd_proto::event_name_with_nonce.
 *
 * Why a nonce (minor 2): creating a section in Global\ needs SeCreateGlobalPrivilege, an
 * event does not. With a fixed event name any interactive user could create it first and
 * keep another seat's monitor from ever coming up (CreateEventW reports ERROR_ALREADY_EXISTS
 * or access denied, the add fails, the service retries for ever). The nonce comes from the
 * system RNG per add and is published only in the section header, which only SYSTEM,
 * Administrators and the reader SID can map, so nobody else can predict the name. */
static void ReadyEventName(const PSIDD_MONITOR *m, const uint8_t nonce[PSIDD_READY_NONCE_BYTES], WCHAR *out, size_t cch)
{
    static const WCHAR hex[] = L"0123456789abcdef";
    WCHAR suffix[PSIDD_READY_NONCE_BYTES * 2 + 1];
    uint32_t i;
    for (i = 0; i < PSIDD_READY_NONCE_BYTES; i++) {
        suffix[i * 2] = hex[nonce[i] >> 4];
        suffix[i * 2 + 1] = hex[nonce[i] & 0xf];
    }
    suffix[PSIDD_READY_NONCE_BYTES * 2] = 0;
    swprintf_s(out, cch, L"Global\\PulsarSeat.Idd.S%02u.C%u.Ready.%ls", m->Seat, m->Connector, suffix);
}

static BOOL BuildSa(SECURITY_ATTRIBUTES *sa, const uint16_t *readerSid, const WCHAR *readerRights)
{
    /* SYSTEM + Administrators full; the seat host's user (if given) read / synchronize. */
    WCHAR sddl[256];
    WCHAR sid[PSIDD_MAX_SID + 1];
    size_t i;
    for (i = 0; i < PSIDD_MAX_SID && readerSid[i] != 0; i++) {
        WCHAR c = (WCHAR)readerSid[i];
        /* Accept only S-1-... characters: never let a request inject SDDL. */
        if (!((c >= L'0' && c <= L'9') || c == L'-' || c == L'S')) {
            return FALSE;
        }
        sid[i] = c;
    }
    sid[i] = 0;
    if (i > 0) {
        swprintf_s(sddl, 256, L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;%ls;;;%ls)", readerRights, sid);
    } else {
        swprintf_s(sddl, 256, L"D:P(A;;GA;;;SY)(A;;GA;;;BA)");
    }
    sa->nLength = sizeof(*sa);
    sa->bInheritHandle = FALSE;
    return ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &sa->lpSecurityDescriptor, NULL);
}

NTSTATUS PsiddCreateSection(PSIDD_MONITOR *m, const uint16_t *readerSid)
{
    WCHAR name[128];
    uint8_t nonce[PSIDD_READY_NONCE_BYTES] = { 0 };
    SECURITY_ATTRIBUTES sa;
    uint64_t maxPixels = (uint64_t)1920 * 1080;
    uint64_t size;
    uint32_t i;
    void *view;
    MEMORY_BASIC_INFORMATION mbi;
    DWORD err;

    for (i = 0; i < m->ModeCount; i++) {
        uint64_t p = (uint64_t)m->Modes[i].Width * m->Modes[i].Height;
        if (p > maxPixels) {
            maxPixels = p;
        }
    }
    size = PSIDD_FRAME_OFFSET + maxPixels * 4;

    if (!BuildSa(&sa, readerSid, L"GR")) {
        return STATUS_INVALID_PARAMETER;
    }
    SectionName(m, name, 128);
    m->Section = CreateFileMappingW(INVALID_HANDLE_VALUE, &sa, PAGE_READWRITE, (DWORD)(size >> 32), (DWORD)size, name);
    err = GetLastError();
    LocalFree(sa.lpSecurityDescriptor);
    if (m->Section == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    if (err == ERROR_ALREADY_EXISTS) {
        /* The name survives while a reader (seat host) still holds the previous instance,
         * or another process squats it. That section has someone else's size and DACL:
         * never use it. The service closes the readers and retries. */
        CloseHandle(m->Section);
        m->Section = NULL;
        return STATUS_OBJECT_NAME_COLLISION;
    }
    view = MapViewOfFile(m->Section, FILE_MAP_WRITE, 0, 0, 0);
    if (view == NULL) {
        CloseHandle(m->Section);
        m->Section = NULL;
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    /* Trust the mapped size, not the requested one. */
    if (VirtualQuery(view, &mbi, sizeof(mbi)) != sizeof(mbi) || mbi.RegionSize < PSIDD_FRAME_OFFSET + (uint64_t)4) {
        UnmapViewOfFile(view);
        CloseHandle(m->Section);
        m->Section = NULL;
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    m->Header = (PSIDD_FRAME_HEADER *)view;
    m->Cursor = (uint8_t *)view + PSIDD_HEADER_SIZE;
    m->Frame = (uint8_t *)view + PSIDD_FRAME_OFFSET;
    RtlZeroMemory(m->Header, sizeof(*m->Header));
    m->Header->Magic = PSIDD_FRAME_MAGIC;
    m->Header->Version = PSIDD_FRAME_VERSION;
    m->Header->HeaderSize = PSIDD_HEADER_SIZE;
    m->Header->CursorOffset = PSIDD_HEADER_SIZE;
    m->Header->CursorCapacity = PSIDD_CURSOR_CAPACITY;
    m->Header->FrameOffset = PSIDD_FRAME_OFFSET;
    m->Header->FrameCapacity = (uint64_t)mbi.RegionSize - PSIDD_FRAME_OFFSET;
    m->Header->Flags = m->Flags;

    /* SYNCHRONIZE (0x00100000) | EVENT_QUERY_STATE (0x1) for the reader. */
    if (!BuildSa(&sa, readerSid, L"0x00100001")) {
        PsiddCloseSection(m);
        return STATUS_INVALID_PARAMETER;
    }
    /* A fresh nonce per try: a 128-bit collision means the name was guessed or squatted,
     * so a few tries are plenty and the loop ends. */
    for (i = 0; i < 4 && m->ReadyEvent == NULL; i++) {
        if (!BCRYPT_SUCCESS(BCryptGenRandom(NULL, nonce, sizeof(nonce), BCRYPT_USE_SYSTEM_PREFERRED_RNG))) {
            break;
        }
        ReadyEventName(m, nonce, name, 128);
        m->ReadyEvent = CreateEventW(&sa, FALSE, FALSE, name);
        err = GetLastError();
        if (m->ReadyEvent != NULL && err == ERROR_ALREADY_EXISTS) {
            CloseHandle(m->ReadyEvent); /* someone else's object: never signal it */
            m->ReadyEvent = NULL;
        }
    }
    LocalFree(sa.lpSecurityDescriptor);
    if (m->ReadyEvent == NULL) {
        PsiddCloseSection(m);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    /* Published after the event exists; readers open the section, then the event. */
    memcpy((void *)m->Header->ReadyNonce, nonce, sizeof(nonce));
    MemoryBarrier();
    return STATUS_SUCCESS;
}

void PsiddCloseSection(PSIDD_MONITOR *m)
{
    if (m->Header != NULL) {
        UnmapViewOfFile(m->Header);
        m->Header = NULL;
        m->Cursor = NULL;
        m->Frame = NULL;
    }
    if (m->Section != NULL) {
        CloseHandle(m->Section);
        m->Section = NULL;
    }
    if (m->ReadyEvent != NULL) {
        CloseHandle(m->ReadyEvent);
        m->ReadyEvent = NULL;
    }
}

/* ---- processing thread ------------------------------------------------------------- */

static HRESULT CreateDeviceOnLuid(LUID luid, ID3D11Device **dev, ID3D11DeviceContext **ctx)
{
    IDXGIFactory5 *factory = NULL;
    IDXGIAdapter1 *adapter = NULL;
    HRESULT hr = CreateDXGIFactory2(0, __uuidof(IDXGIFactory5), (void **)&factory);
    if (FAILED(hr)) {
        return hr;
    }
    hr = factory->EnumAdapterByLuid(luid, __uuidof(IDXGIAdapter1), (void **)&adapter);
    if (SUCCEEDED(hr)) {
        hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, NULL,
                               D3D11_CREATE_DEVICE_BGRA_SUPPORT, NULL, 0, D3D11_SDK_VERSION, dev, NULL, ctx);
        adapter->Release();
    }
    factory->Release();
    return hr;
}

static void WriteFrame(PSIDD_MONITOR *m, ID3D11DeviceContext *ctx, ID3D11Texture2D *staging, UINT w, UINT h)
{
    D3D11_MAPPED_SUBRESOURCE map;
    PSIDD_FRAME_HEADER *hd = m->Header;
    UINT y;
    uint64_t need = (uint64_t)w * h * 4;

    if (hd == NULL || need > hd->FrameCapacity) {
        return; /* mode larger than announced: drop (the service re-adds with new modes) */
    }
    if (FAILED(ctx->Map(staging, 0, D3D11_MAP_READ, 0, &map))) {
        return;
    }
    InterlockedIncrement64((volatile LONG64 *)&hd->Seq); /* odd: writing */
    MemoryBarrier();
    for (y = 0; y < h; y++) {
        memcpy(m->Frame + (size_t)y * w * 4, (const uint8_t *)map.pData + (size_t)y * map.RowPitch, (size_t)w * 4);
    }
    hd->Width = w;
    hd->Height = h;
    hd->Stride = w * 4;
    hd->Format = 0;
    /* IddCx delivers the surface in scan-out orientation (the OS rotates while composing;
     * HYPOTHESIS, unverified on hardware), so the presenter never rotates IDD frames. */
    hd->Rotation = 0;
    MemoryBarrier();
    InterlockedIncrement64((volatile LONG64 *)&hd->Seq); /* even: stable */
    ctx->Unmap(staging, 0);
    SetEvent(m->ReadyEvent);
}

/* Cursor fields and shape are written under their own sequence lock (CursorSeq odd while
 * writing), so the reader never sees a half-written shape or mismatched size. */
static void ReadCursor(PSIDD_MONITOR *m, UINT *lastShapeId, BYTE *shape)
{
    IDARG_IN_QUERY_HWCURSOR in;
    IDARG_OUT_QUERY_HWCURSOR out;
    PSIDD_FRAME_HEADER *hd = m->Header;

    if (hd == NULL) {
        return;
    }
    RtlZeroMemory(&in, sizeof(in));
    in.LastShapeId = *lastShapeId;
    in.ShapeBufferSizeInBytes = PSIDD_CURSOR_CAPACITY;
    in.pShapeBuffer = shape;
    if (!NT_SUCCESS(IddCxMonitorQueryHardwareCursor(m->SwapMonitor, &in, &out))) {
        return;
    }
    InterlockedIncrement((volatile LONG *)&hd->CursorSeq); /* odd: writing */
    MemoryBarrier();
    hd->CursorVisible = out.IsCursorVisible ? 1 : 0;
    hd->CursorX = out.X;
    hd->CursorY = out.Y;
    if (out.IsCursorShapeUpdated) {
        const IDDCX_CURSOR_SHAPE_INFO *ci = &out.CursorShapeInfo;
        UINT w = ci->Width, h = ci->Height, y;
        if (w <= PSIDD_CURSOR_MAX && h <= PSIDD_CURSOR_MAX) {
            for (y = 0; y < h; y++) {
                memcpy(m->Cursor + (size_t)y * w * 4, shape + (size_t)y * ci->Pitch, (size_t)w * 4);
            }
            if (ci->CursorType == IDDCX_CURSOR_SHAPE_TYPE_MASKED_COLOR) {
                /* Masked colour: alpha byte is an XOR flag, not opacity. Show it opaque. */
                UINT i;
                for (i = 0; i < w * h; i++) {
                    m->Cursor[i * 4 + 3] = m->Cursor[i * 4 + 3] ? 0 : 255;
                }
            }
            hd->CursorWidth = w;
            hd->CursorHeight = h;
            hd->CursorHotX = ci->XHot;
            hd->CursorHotY = ci->YHot;
            hd->CursorShapeId++;
        }
        *lastShapeId = ci->ShapeId;
    }
    MemoryBarrier();
    InterlockedIncrement((volatile LONG *)&hd->CursorSeq); /* even: stable */
    SetEvent(m->ReadyEvent);
}

static DWORD WINAPI SwapChainThread(LPVOID param)
{
    PSIDD_MONITOR *m = (PSIDD_MONITOR *)param;
    ID3D11Device *dev = NULL;
    ID3D11DeviceContext *ctx = NULL;
    IDXGIDevice *dxgi = NULL;
    ID3D11Texture2D *staging = NULL;
    D3D11_TEXTURE2D_DESC sdesc;
    IDARG_IN_SWAPCHAINSETDEVICE setDev;
    IDARG_IN_SETUP_HWCURSOR hwc;
    DWORD avTask = 0;
    HANDLE av = AvSetMmThreadCharacteristicsW(L"Distribution", &avTask);
    UINT lastShapeId = 0;
    BYTE *shape = (BYTE *)HeapAlloc(GetProcessHeap(), 0, PSIDD_CURSOR_CAPACITY);
    HRESULT hr;

    if (shape == NULL) {
        return 1;
    }

    RtlZeroMemory(&sdesc, sizeof(sdesc));
    hr = CreateDeviceOnLuid(m->RenderLuid, &dev, &ctx);
    if (SUCCEEDED(hr)) {
        hr = dev->QueryInterface(__uuidof(IDXGIDevice), (void **)&dxgi);
    }
    if (SUCCEEDED(hr)) {
        RtlZeroMemory(&setDev, sizeof(setDev));
        setDev.pDevice = dxgi;
        hr = IddCxSwapChainSetDevice(m->SwapChain, &setDev);
    }
    if (FAILED(hr)) {
        /* Without a device the OS re-creates the swap chain later. */
        goto done;
    }

    /* Per-seat software cursor option (PSIDD_ADD_FLAG_SOFTWARE_CURSOR, spec 40 §6.3.5):
     * no hardware cursor, DWM draws the pointer into the frames. */
    if ((m->Flags & PSIDD_ADD_FLAG_SOFTWARE_CURSOR) == 0) {
        RtlZeroMemory(&hwc, sizeof(hwc));
        hwc.CursorInfo.Size = sizeof(hwc.CursorInfo);
        /* We cannot render XOR (the presenter blends with alpha only): NONE makes the OS
         * draw XOR / inverting cursors into the frames itself. */
        hwc.CursorInfo.ColorXorCursorSupport = IDDCX_XOR_CURSOR_SUPPORT_NONE;
        hwc.CursorInfo.AlphaCursorSupport = TRUE;
        hwc.CursorInfo.MaxX = PSIDD_CURSOR_MAX;
        hwc.CursorInfo.MaxY = PSIDD_CURSOR_MAX;
        hwc.hNewCursorDataAvailable = m->CursorEvent;
        /* If refused, DWM draws the cursor into the frames (software cursor): the cursor
         * event then never fires and the section's cursor area stays empty. */
        if (!NT_SUCCESS(IddCxMonitorSetupHardwareCursor(m->SwapMonitor, &hwc))) {
            ResetEvent(m->CursorEvent);
        }
    }

    if (m->Header) {
        m->Header->Connected = 1;
    }
    for (;;) {
        IDARG_OUT_RELEASEANDACQUIREBUFFER buf;
        RtlZeroMemory(&buf, sizeof(buf));
        hr = IddCxSwapChainReleaseAndAcquireBuffer(m->SwapChain, &buf);
        if (hr == E_PENDING) {
            HANDLE waits[3] = { m->NextSurface, m->CursorEvent, m->StopEvent };
            DWORD r = WaitForMultipleObjects(3, waits, FALSE, PSIDD_SWAPCHAIN_WAIT_MS);
            if (r == WAIT_OBJECT_0 + 2) {
                break;
            }
            if (r == WAIT_OBJECT_0 + 1) {
                ReadCursor(m, &lastShapeId, shape);
            }
            continue;
        }
        if (FAILED(hr)) {
            break; /* swap chain abandoned: OS assigns a new one */
        }
        if (buf.MetaData.pSurface != NULL) {
            ID3D11Texture2D *tex = NULL;
            if (SUCCEEDED(buf.MetaData.pSurface->QueryInterface(__uuidof(ID3D11Texture2D), (void **)&tex))) {
                D3D11_TEXTURE2D_DESC d;
                tex->GetDesc(&d);
                if (staging == NULL || sdesc.Width != d.Width || sdesc.Height != d.Height || sdesc.Format != d.Format) {
                    if (staging) {
                        staging->Release();
                        staging = NULL;
                    }
                    sdesc = d;
                    sdesc.BindFlags = 0;
                    sdesc.MiscFlags = 0;
                    sdesc.Usage = D3D11_USAGE_STAGING;
                    sdesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
                    sdesc.MipLevels = 1;
                    sdesc.ArraySize = 1;
                    sdesc.SampleDesc.Count = 1;
                    sdesc.SampleDesc.Quality = 0;
                    dev->CreateTexture2D(&sdesc, NULL, &staging);
                }
                if (staging != NULL && d.Format == DXGI_FORMAT_B8G8R8A8_UNORM) {
                    ctx->CopyResource(staging, tex);
                    WriteFrame(m, ctx, staging, d.Width, d.Height);
                }
                tex->Release();
            }
            /* The surface reference is ours until the next ReleaseAndAcquireBuffer. */
            buf.MetaData.pSurface->Release();
        }
        if (FAILED(IddCxSwapChainFinishedProcessingFrame(m->SwapChain))) {
            break; /* swap chain lost: the OS assigns a new one */
        }
        if (WaitForSingleObject(m->CursorEvent, 0) == WAIT_OBJECT_0) {
            ReadCursor(m, &lastShapeId, shape);
        }
        if (WaitForSingleObject(m->StopEvent, 0) == WAIT_OBJECT_0) {
            break;
        }
    }

done:
    if (m->Header) {
        m->Header->Connected = 0;
        SetEvent(m->ReadyEvent);
    }
    if (staging) staging->Release();
    if (dxgi) dxgi->Release();
    if (ctx) ctx->Release();
    if (dev) dev->Release();
    /* The OS owns the swap chain object; deleting our reference ends it (IddCx docs). */
    WdfObjectDelete((WDFOBJECT)m->SwapChain);
    if (av) AvRevertMmThreadCharacteristics(av);
    HeapFree(GetProcessHeap(), 0, shape);
    return 0;
}

/* Caller holds the slot's SwapLock (driver.cpp). */
NTSTATUS PsiddStartSwapChain(PSIDD_MONITOR *m, IDDCX_MONITOR monitor, const IDARG_IN_SETSWAPCHAIN *args)
{
    /* The monitor the swap chain belongs to: RemoveMonitor clears m->Object before
     * IddCxMonitorDeparture while this thread still runs, so the thread never reads it. */
    m->SwapMonitor = monitor;
    m->SwapChain = args->hSwapChain;
    m->NextSurface = args->hNextSurfaceAvailable;
    m->RenderLuid = args->RenderAdapterLuid;
    m->StopEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    m->CursorEvent = CreateEventW(NULL, FALSE, FALSE, NULL);
    if (m->StopEvent == NULL || m->CursorEvent == NULL) {
        PsiddStopSwapChain(m);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    m->Thread = CreateThread(NULL, 0, SwapChainThread, m, 0, NULL);
    if (m->Thread == NULL) {
        PsiddStopSwapChain(m);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    return STATUS_SUCCESS;
}

/* Caller holds the slot's SwapLock (driver.cpp). The handle is also taken with an
 * interlocked exchange, so even a caller outside the lock could never wait on or close
 * it twice. */
void PsiddStopSwapChain(PSIDD_MONITOR *m)
{
    HANDLE thread = InterlockedExchangePointer(&m->Thread, NULL);
    if (thread != NULL) {
        SetEvent(m->StopEvent);
        WaitForSingleObject(thread, INFINITE);
        CloseHandle(thread);
    }
    if (m->StopEvent != NULL) {
        CloseHandle(m->StopEvent);
        m->StopEvent = NULL;
    }
    if (m->CursorEvent != NULL) {
        CloseHandle(m->CursorEvent);
        m->CursorEvent = NULL;
    }
    m->SwapChain = NULL;
    m->SwapMonitor = NULL;
}
