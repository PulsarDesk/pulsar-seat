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
 *
 * Minor 3 (IddCx 1.10 at run time): frames come from IddCxSwapChainReleaseAndAcquireBuffer2
 * with their colour space, SDR white level and HDR10 metadata. An HDR monitor
 * (PSIDD_ADD_FLAG_HDR) gets FP16 / 10-bit pixels as they are (its section holds 8 bytes per
 * pixel); any other monitor that receives an FP16 surface gets a BGRA8 conversion (linear
 * scRGB to sRGB through a 64 K entry table, clipped to [0, 1]), so SDR readers never see
 * FP16. Every frame carries QPC timestamps for latency measurement.
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

/* Bytes per pixel a monitor's section is sized for: 8 for HDR monitors on a 1.10 adapter
 * (FP16), else 4. */
static uint32_t SectionBpp(const PSIDD_MONITOR *m)
{
    return (m->Has110 && (m->Flags & PSIDD_ADD_FLAG_HDR) != 0) ? 8u : 4u;
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
    size = PSIDD_FRAME_OFFSET + maxPixels * SectionBpp(m);

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

/* IEEE half (bits) to float. */
static float HalfToFloat(uint16_t h)
{
    uint32_t sign = (uint32_t)(h & 0x8000u) << 16;
    uint32_t exp = (h >> 10) & 0x1fu;
    uint32_t man = h & 0x3ffu;
    uint32_t bits;
    float f;
    if (exp == 0) {
        /* subnormal / zero: man * 2^-24 */
        f = (float)man * (1.0f / 16777216.0f);
        return sign ? -f : f;
    }
    if (exp == 31) {
        bits = sign | 0x7f800000u | (man << 13);
    } else {
        bits = sign | ((exp + 112u) << 23) | (man << 13);
    }
    memcpy(&f, &bits, sizeof(f));
    return f;
}

/* Linear scRGB (1.0 = SDR white) to an 8-bit sRGB code, clipped. Table built once. */
static uint8_t g_HalfToSrgb[65536];
static INIT_ONCE g_HalfToSrgbOnce = INIT_ONCE_STATIC_INIT;

static BOOL CALLBACK BuildHalfToSrgb(PINIT_ONCE once, PVOID param, PVOID *ctx)
{
    UNREFERENCED_PARAMETER(once);
    UNREFERENCED_PARAMETER(param);
    UNREFERENCED_PARAMETER(ctx);
    for (uint32_t i = 0; i < 65536u; i++) {
        float v = HalfToFloat((uint16_t)i);
        float e;
        if (!(v > 0.0f)) {
            v = 0.0f; /* negatives and NaN */
        }
        if (v > 1.0f) {
            v = 1.0f;
        }
        e = v <= 0.0031308f ? v * 12.92f : 1.055f * powf(v, 1.0f / 2.4f) - 0.055f;
        g_HalfToSrgb[i] = (uint8_t)(e * 255.0f + 0.5f);
    }
    return TRUE;
}

/* Section format code of a surface format; FALSE = not supported (frame dropped). */
static BOOL FormatCode(DXGI_FORMAT f, uint32_t *code, uint32_t *bpp)
{
    switch (f) {
    case DXGI_FORMAT_B8G8R8A8_UNORM:
        *code = PSIDD_FORMAT_BGRA8;
        *bpp = 4;
        return TRUE;
    case DXGI_FORMAT_R16G16B16A16_FLOAT:
        *code = PSIDD_FORMAT_RGBA16F;
        *bpp = 8;
        return TRUE;
    case DXGI_FORMAT_R10G10B10A2_UNORM:
        *code = PSIDD_FORMAT_RGB10A2;
        *bpp = 4;
        return TRUE;
    default:
        return FALSE;
    }
}

/* Per-frame metadata from IDDCX_METADATA2 (zero / defaults down-level). */
typedef struct _PSIDD_FRAME_META {
    uint32_t ColorSpace;
    uint32_t SdrWhiteLevel;
    BOOL Hdr10Valid;
    uint16_t Hdr10[12];
    uint64_t AcquireQpc;
    uint64_t PresentQpc;
    uint32_t FrameNumber;
} PSIDD_FRAME_META;

static uint64_t Qpc(void)
{
    LARGE_INTEGER t;
    QueryPerformanceCounter(&t);
    return (uint64_t)t.QuadPart;
}

static void WriteFrame(PSIDD_MONITOR *m, ID3D11DeviceContext *ctx, ID3D11Texture2D *staging, UINT w, UINT h, DXGI_FORMAT fmt,
                       const PSIDD_FRAME_META *meta)
{
    D3D11_MAPPED_SUBRESOURCE map;
    PSIDD_FRAME_HEADER *hd = m->Header;
    UINT y;
    uint32_t code = 0, srcBpp = 4, outBpp, outCode;
    BOOL toSdr;
    uint64_t need;

    if (hd == NULL || !FormatCode(fmt, &code, &srcBpp)) {
        return;
    }
    /* SDR monitors (4-byte sections) never get FP16: convert to BGRA8. A 10-bit surface on
     * an SDR monitor is dropped (not expected: the OS sends 10-bit only to HDR targets). */
    toSdr = code == PSIDD_FORMAT_RGBA16F && SectionBpp(m) < 8;
    if (code == PSIDD_FORMAT_RGB10A2 && SectionBpp(m) < 8) {
        return;
    }
    outBpp = toSdr ? 4u : srcBpp;
    outCode = toSdr ? PSIDD_FORMAT_BGRA8 : code;
    need = (uint64_t)w * h * outBpp;
    if (need > hd->FrameCapacity) {
        return; /* mode larger than announced: drop (the service re-adds with new modes) */
    }
    if (toSdr) {
        InitOnceExecuteOnce(&g_HalfToSrgbOnce, BuildHalfToSrgb, NULL, NULL);
    }
    if (FAILED(ctx->Map(staging, 0, D3D11_MAP_READ, 0, &map))) {
        return;
    }
    InterlockedIncrement64((volatile LONG64 *)&hd->Seq); /* odd: writing */
    MemoryBarrier();
    for (y = 0; y < h; y++) {
        const uint8_t *src = (const uint8_t *)map.pData + (size_t)y * map.RowPitch;
        uint8_t *dst = m->Frame + (size_t)y * w * outBpp;
        if (toSdr) {
            const uint16_t *px = (const uint16_t *)src;
            for (UINT x = 0; x < w; x++) {
                dst[x * 4 + 0] = g_HalfToSrgb[px[x * 4 + 2]]; /* B */
                dst[x * 4 + 1] = g_HalfToSrgb[px[x * 4 + 1]]; /* G */
                dst[x * 4 + 2] = g_HalfToSrgb[px[x * 4 + 0]]; /* R */
                dst[x * 4 + 3] = 255;
            }
        } else {
            memcpy(dst, src, (size_t)w * outBpp);
        }
    }
    hd->Width = w;
    hd->Height = h;
    hd->Stride = w * outBpp;
    hd->Format = outCode;
    hd->ColorSpace = toSdr ? 0u : meta->ColorSpace;
    hd->SdrWhiteLevel = meta->SdrWhiteLevel;
    hd->Hdr10Valid = (!toSdr && meta->Hdr10Valid) ? 1u : 0u;
    memcpy((void *)hd->Hdr10, meta->Hdr10, sizeof(hd->Hdr10));
    hd->AcquireQpc = meta->AcquireQpc;
    hd->PresentQpc = meta->PresentQpc;
    hd->FrameNumber = meta->FrameNumber;
    hd->WrittenQpc = Qpc();
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
    /* HDR10 metadata of the last frame that sent NEW metadata (UNCHANGED reuses it). */
    uint16_t lastHdr[12] = { 0 };
    BOOL lastHdrValid = FALSE;

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
        IDXGIResource *surface = NULL;
        PSIDD_FRAME_META meta;
        RtlZeroMemory(&meta, sizeof(meta));
        if (m->Has110) {
            /* 1.10: colour space, SDR white level and HDR10 metadata per frame (minor 3). */
            IDARG_IN_RELEASEANDACQUIREBUFFER2 in2;
            IDARG_OUT_RELEASEANDACQUIREBUFFER2 out2;
            RtlZeroMemory(&in2, sizeof(in2));
            RtlZeroMemory(&out2, sizeof(out2));
            in2.Size = sizeof(in2);
            in2.AcquireSystemMemoryBuffer = FALSE;
            hr = IddCxSwapChainReleaseAndAcquireBuffer2(m->SwapChain, &in2, &out2);
            if (SUCCEEDED(hr) && hr != E_PENDING) {
                const IDDCX_METADATA2 *md = &out2.MetaData;
                surface = md->pSurface;
                meta.AcquireQpc = Qpc();
                meta.PresentQpc = md->PresentDisplayQPCTime;
                meta.FrameNumber = md->PresentationFrameNumber;
                meta.ColorSpace = (uint32_t)md->SurfaceColorSpace;
                meta.SdrWhiteLevel = md->SdrWhiteLevel;
                if ((md->ValidFlags & IDDCX_METADATA2_VALID_FLAGS_HDR10METADATA) != 0) {
                    const IDDCX_HDR10_FRAME_METADATA *fm = &md->Hdr10FrameMetaData;
                    if (fm->Type == IDDCX_HDR10_FRAME_METADATA_TYPE_NEW) {
                        const IDDCX_HDR10_METADATA *h = &fm->NewMetaData;
                        const uint16_t v[12] = { h->RedPrimary[0], h->RedPrimary[1], h->GreenPrimary[0], h->GreenPrimary[1],
                                                 h->BluePrimary[0], h->BluePrimary[1], h->WhitePoint[0], h->WhitePoint[1],
                                                 h->MaxMasteringLuminance, h->MinMasteringLuminance, h->MaxContentLightLevel,
                                                 h->MaxFrameAverageLightLevel };
                        memcpy(lastHdr, v, sizeof(lastHdr));
                        lastHdrValid = TRUE;
                    } else if (fm->Type == IDDCX_HDR10_FRAME_METADATA_TYPE_DEFAULT) {
                        lastHdrValid = FALSE; /* the monitor default below */
                    }
                    /* UNCHANGED: keep the last one */
                }
                if (lastHdrValid) {
                    memcpy(meta.Hdr10, lastHdr, sizeof(meta.Hdr10));
                    meta.Hdr10Valid = TRUE;
                } else if (m->DefaultHdrValid) {
                    /* Written by SetDefaultHdrMetaData under the slot's SwapLock, which the
                     * assign path holds only around start / stop: a torn read here can at
                     * worst mix two metadata blocks for one frame. */
                    memcpy(meta.Hdr10, (const void *)m->DefaultHdr, sizeof(meta.Hdr10));
                    meta.Hdr10Valid = TRUE;
                }
            }
        } else {
            IDARG_OUT_RELEASEANDACQUIREBUFFER buf;
            RtlZeroMemory(&buf, sizeof(buf));
            hr = IddCxSwapChainReleaseAndAcquireBuffer(m->SwapChain, &buf);
            if (SUCCEEDED(hr) && hr != E_PENDING) {
                surface = buf.MetaData.pSurface;
                meta.AcquireQpc = Qpc();
                meta.FrameNumber = buf.MetaData.PresentationFrameNumber;
            }
        }
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
        if (surface != NULL) {
            ID3D11Texture2D *tex = NULL;
            if (SUCCEEDED(surface->QueryInterface(__uuidof(ID3D11Texture2D), (void **)&tex))) {
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
                if (staging != NULL) {
                    ctx->CopyResource(staging, tex);
                    WriteFrame(m, ctx, staging, d.Width, d.Height, d.Format, &meta);
                }
                tex->Release();
            }
            /* The surface reference is ours until the next ReleaseAndAcquireBuffer. */
            surface->Release();
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
