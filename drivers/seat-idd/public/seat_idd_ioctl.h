/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat IddCx driver: control interface and frame-section layout.
 *
 * Shared contract with crates/seat-display/src/idd_proto.rs. Any change here must be made
 * there too; the Rust size / offset tests pin the layout. Changes inside one interface
 * major are additive only; a new major gets a new interface GUID (drivers/README.md).
 */
#pragma once

#include <stdint.h>

#define PSIDD_INTERFACE_MAJOR 1
#define PSIDD_INTERFACE_MINOR 3 /* 1: Flags, CursorShapeId, cursor seqlock, frame-section collision check
                                 * 2: ready-event name carries ReadyNonce (unsquattable)
                                 * 3: one adapter per render GPU (GET_ADAPTER_INFO, bound render
                                 *    LUID), HDR flag + FP16 frames + HDR10 metadata, frame
                                 *    timestamps (2026-11-17) */

/* {3250655a-777f-450a-9ed6-9bd48c2a93b6} = seat_core::consts::IDD_INTERFACE_GUID */
#define PSIDD_INTERFACE_GUID_INIT \
    { 0x3250655a, 0x777f, 0x450a, { 0x9e, 0xd6, 0x9b, 0xd4, 0x8c, 0x2a, 0x93, 0xb6 } }

#define PSIDD_CTL(fn) CTL_CODE(FILE_DEVICE_UNKNOWN, (fn), METHOD_BUFFERED, FILE_READ_DATA | FILE_WRITE_DATA)

/* Out: PSIDD_VERSION. Always the first call of a client. */
#define IOCTL_PSIDD_GET_VERSION        PSIDD_CTL(0x900)
/* In: PSIDD_ADD_MONITOR. Out: UINT32 (0).
 * STATUS_OBJECT_NAME_COLLISION when the slot is in use or being removed, or when the
 * frame section name still exists (a reader still holds the previous instance: close
 * every IddFrameReader of the slot first, then retry). The ready event has a random name
 * per add (minor 2), so it never collides. */
#define IOCTL_PSIDD_ADD_MONITOR        PSIDD_CTL(0x901)
/* In: PSIDD_MONITOR_REF. Out: UINT32 (0). */
#define IOCTL_PSIDD_REMOVE_MONITOR     PSIDD_CTL(0x902)
/* In: PSIDD_RENDER_ADAPTER. Out: UINT32 (0). Binds the adapter to that render GPU (minor 3):
 * STATUS_DEVICE_BUSY while monitors of another GPU are plugged. An ADD_MONITOR with a
 * RenderAdapterLuid binds an unbound adapter the same way and is refused with
 * STATUS_DEVICE_BUSY on an adapter bound to another GPU that still has monitors. */
#define IOCTL_PSIDD_SET_RENDER_ADAPTER PSIDD_CTL(0x903)
/* Out: PSIDD_ADAPTER_INFO (minor 3). */
#define IOCTL_PSIDD_GET_ADAPTER_INFO   PSIDD_CTL(0x904)

#define PSIDD_MAX_EDID  512
#define PSIDD_MAX_MODES 16
#define PSIDD_MAX_SID   96
#define PSIDD_MAX_SEATS 16
#define PSIDD_MAX_CONNECTORS_PER_SEAT 4
#define PSIDD_MAX_MONITORS (PSIDD_MAX_SEATS * PSIDD_MAX_CONNECTORS_PER_SEAT)
/* Monitors at the same time on the one IddCx adapter. IddCx refuses an adapter with
 * MaxMonitorsSupported above 16 (IddCxAdapterInitAsync -> STATUS_INVALID_PARAMETER: 17, 20,
 * 24, 32 and 64 fail, 16 works; Windows 11 build 26300 ARM64 VM, 2026-11-15), so the 64
 * seat / connector slots share 16 IddCx connector indexes; a 17th monitor is refused with
 * STATUS_INSUFFICIENT_RESOURCES. */
#define PSIDD_ADAPTER_MAX_MONITORS 16

/* Mode limits the driver accepts (larger values are rejected with
 * STATUS_INVALID_PARAMETER; they would size multi-GB sections). */
#define PSIDD_MAX_MODE_DIM      16384u
#define PSIDD_MAX_REFRESH_MHZ   1000000u

/* PSIDD_ADD_MONITOR.Flags (minor 1). */
#define PSIDD_ADD_FLAG_SOFTWARE_CURSOR 0x1u /* skip IddCxMonitorSetupHardwareCursor: DWM draws the pointer into the frames */
#define PSIDD_ADD_FLAG_HDR             0x2u /* minor 3: offer HDR10 / SDR wide colour (needs IddCx 1.10; the
                                             * EDID's HDR blocks tell the OS what the panel can do) */
#define PSIDD_ADD_FLAGS_KNOWN          (PSIDD_ADD_FLAG_SOFTWARE_CURSOR | PSIDD_ADD_FLAG_HDR)

/* PSIDD_ADAPTER_INFO.Flags (minor 3). */
#define PSIDD_ADAPTER_READY  0x1u /* EvtIddCxAdapterInitFinished succeeded */
#define PSIDD_ADAPTER_BOUND  0x2u /* RenderLuid is valid */
#define PSIDD_ADAPTER_FP16   0x4u /* IddCx 1.10 at run time: FP16 surfaces, HDR monitors possible */

/* Largest number of adapters (devnodes) the service creates: one per render GPU. */
#define PSIDD_MAX_ADAPTERS 8

/* PSIDD_FRAME_HEADER.Format */
#define PSIDD_FORMAT_BGRA8   0u /* DXGI_FORMAT_B8G8R8A8_UNORM, 4 bytes per pixel */
#define PSIDD_FORMAT_RGBA16F 1u /* DXGI_FORMAT_R16G16B16A16_FLOAT, 8 bytes per pixel (minor 3) */
#define PSIDD_FORMAT_RGB10A2 2u /* DXGI_FORMAT_R10G10B10A2_UNORM, 4 bytes per pixel (minor 3) */

typedef struct _PSIDD_VERSION {
    uint16_t Major;
    uint16_t Minor;
    uint32_t Build;
} PSIDD_VERSION;

typedef struct _PSIDD_MODE {
    uint32_t Width;
    uint32_t Height;
    uint32_t RefreshMilliHz;
} PSIDD_MODE;

typedef struct _PSIDD_ADD_MONITOR {
    uint32_t Size;                 /* sizeof(PSIDD_ADD_MONITOR) = 928 */
    uint32_t Seat;                 /* 1..16 */
    uint32_t Connector;            /* 0..3, output index of the seat */
    uint32_t EdidLength;
    uint8_t  Edid[PSIDD_MAX_EDID]; /* physical monitor's EDID, whole 128-byte blocks (may be empty).
                                    * The driver patches the base block's serial number per slot
                                    * (and the checksum) so identical monitor models stay distinct. */
    uint32_t ModeCount;
    uint32_t Flags;                /* PSIDD_ADD_FLAG_* (minor 1; was Reserved = 0) */
    PSIDD_MODE Modes[PSIDD_MAX_MODES];
    uint64_t RenderAdapterLuid;    /* GPU of the physical monitor */
    uint16_t ReaderSid[PSIDD_MAX_SID]; /* UTF-16 string SID granted read access; empty = none */
} PSIDD_ADD_MONITOR;

typedef struct _PSIDD_MONITOR_REF {
    uint32_t Seat;
    uint32_t Connector;
} PSIDD_MONITOR_REF;

typedef struct _PSIDD_RENDER_ADAPTER {
    uint64_t Luid;
} PSIDD_RENDER_ADAPTER;

typedef struct _PSIDD_ADAPTER_INFO {
    uint32_t Size;         /* sizeof(PSIDD_ADAPTER_INFO) = 24 */
    uint32_t Flags;        /* PSIDD_ADAPTER_* */
    uint64_t RenderLuid;   /* bound render GPU (HighPart << 32 | LowPart); 0 = unbound */
    uint32_t MonitorCount; /* monitors plugged on this adapter */
    uint32_t IddCxVersion; /* IddCxGetVersion (0 = unknown) */
} PSIDD_ADAPTER_INFO;

/* ---- frame section -------------------------------------------------------------------
 * Name:  Global\PulsarSeat.Idd.S<seat 2 digits>.C<connector>
 * Event: <name>.Ready.<ReadyNonce as 32 lower-case hex digits, byte 0 first> (minor 2;
 *        auto-reset, set after every frame / cursor update). The nonce is random per add
 *        and only readable from the section, so no other user can create the event name
 *        first (events, unlike sections, need no privilege in Global\). Readers map the
 *        section, read ReadyNonce, then open the event.
 * Layout: header (4096 bytes) | cursor shape (256*256*4, BGRA straight alpha) | frame (Format:
 *         BGRA8, or with PSIDD_ADD_FLAG_HDR on IddCx 1.10 also RGBA16F / RGB10A2; an HDR
 *         monitor's section holds 8 bytes per pixel of its largest mode)
 * Sequence locks (minor 1): Seq guards the frame fields and pixels, CursorSeq guards the
 * cursor fields and shape. Each is odd while the driver writes; the driver increments it
 * with an interlocked op and a full barrier on both sides. Readers load it with acquire
 * semantics, copy, issue an acquire fence, re-read it and retry on odd / change.
 * CursorSeq changes on every cursor update (position, visibility, shape); CursorShapeId
 * changes only when the shape changes.
 * FrameCapacity = mapped view size - FrameOffset (VirtualQuery), not the requested size.
 */
#define PSIDD_FRAME_MAGIC       0x52465350u /* "PSFR" */
#define PSIDD_FRAME_VERSION     1u
#define PSIDD_HEADER_SIZE       4096u
#define PSIDD_CURSOR_MAX        256u
#define PSIDD_CURSOR_CAPACITY   (PSIDD_CURSOR_MAX * PSIDD_CURSOR_MAX * 4u)
#define PSIDD_FRAME_OFFSET      (PSIDD_HEADER_SIZE + PSIDD_CURSOR_CAPACITY)
#define PSIDD_READY_NONCE_BYTES 16u

typedef struct _PSIDD_FRAME_HEADER {
    uint32_t Magic;
    uint32_t Version;
    uint32_t HeaderSize;
    uint32_t CursorOffset;
    uint32_t CursorCapacity;
    uint32_t FrameOffset;
    uint64_t FrameCapacity;
    volatile uint64_t Seq;
    uint32_t Width;
    uint32_t Height;
    uint32_t Stride;
    uint32_t Format;       /* 0 = BGRA8 */
    uint32_t Rotation;     /* degrees; always 0: IddCx hands the swap chain already in scan-out
                            * orientation (HYPOTHESIS, unverified on hardware) */
    int32_t  CursorX;
    int32_t  CursorY;
    uint32_t CursorVisible;
    uint32_t CursorWidth;
    uint32_t CursorHeight;
    uint32_t CursorHotX;
    uint32_t CursorHotY;
    volatile uint32_t CursorSeq; /* cursor sequence lock (see above) */
    uint32_t Connected;    /* 1 while a swap chain is assigned */
    /* minor 1 (additive; the header area is HeaderSize = 4096 bytes) */
    uint32_t CursorShapeId; /* bumped when the shape changes */
    uint32_t Flags;         /* copy of PSIDD_ADD_MONITOR.Flags */
    /* minor 2 */
    uint8_t  ReadyNonce[PSIDD_READY_NONCE_BYTES]; /* ready-event name suffix (see above) */
    /* minor 3 (frame fields: under Seq like the pixels) */
    uint32_t ColorSpace;    /* DXGI_COLOR_SPACE_TYPE of the frame: 0 = sRGB (RGB_FULL_G22_NONE_P709),
                             * 1 = scRGB linear (RGB_FULL_G10_NONE_P709, FP16), 12 = HDR10
                             * (RGB_FULL_G2084_NONE_P2020) */
    uint32_t SdrWhiteLevel; /* IDDCX_METADATA2::SdrWhiteLevel as delivered (1000 = 80 nits); 0 = unknown */
    uint32_t Hdr10Valid;    /* 1 = Hdr10 holds the frame's (or the monitor's default) HDR10 metadata */
    uint16_t Hdr10[12];     /* IDDCX_HDR10_METADATA order: R x,y, G x,y, B x,y, W x,y (0.00002 units),
                             * MaxMasteringLuminance (nits), MinMasteringLuminance (0.0001 nits),
                             * MaxContentLightLevel, MaxFrameAverageLightLevel (nits) */
    uint32_t Reserved3;
    uint64_t AcquireQpc;    /* QueryPerformanceCounter when the driver got the frame */
    uint64_t PresentQpc;    /* IDDCX_METADATA2::PresentDisplayQPCTime (0 down-level) */
    uint64_t WrittenQpc;    /* QueryPerformanceCounter after the copy into the section */
    uint32_t FrameNumber;   /* OS PresentationFrameNumber */
    uint32_t Reserved4;
} PSIDD_FRAME_HEADER;      /* 192 bytes */

#ifdef __cplusplus
static_assert(sizeof(PSIDD_ADD_MONITOR) == 928, "PSIDD_ADD_MONITOR layout");
static_assert(sizeof(PSIDD_FRAME_HEADER) == 192, "PSIDD_FRAME_HEADER layout");
static_assert(sizeof(PSIDD_ADAPTER_INFO) == 24, "PSIDD_ADAPTER_INFO layout");
#else
_Static_assert(sizeof(PSIDD_ADD_MONITOR) == 928, "PSIDD_ADD_MONITOR layout");
_Static_assert(sizeof(PSIDD_FRAME_HEADER) == 192, "PSIDD_FRAME_HEADER layout");
_Static_assert(sizeof(PSIDD_ADAPTER_INFO) == 24, "PSIDD_ADAPTER_INFO layout");
#endif
