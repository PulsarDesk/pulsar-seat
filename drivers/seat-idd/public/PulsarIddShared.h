// SPDX-License-Identifier: GPL-3.0-only
//
// Remote role of the Pulsar Seat IddCx driver (drivers/seat-idd, formerly Pulsar Split's
// split-idd): wire contract shared with the service (crates/seat-display/src/remote/ioctl.rs
// and src/frame.rs). Spec: spec/remote/40-display-hdr.md §6.4, §7.
//
// Built unsigned, untested, never loaded. Both sides must change together; the Rust unit
// tests pin every offset and size below.

#pragma once

#include <stddef.h>
#include <stdint.h>

// ---- device interface + hardware id ------------------------------------------------------

// {B5FCB55F-1E09-4C81-A58F-0362792CFE99}  (generated for this project; never reuse)
#define PULSAR_IDD_INTERFACE_GUID_INIT \
    { 0xb5fcb55f, 0x1e09, 0x4c81, { 0xa5, 0x8f, 0x03, 0x62, 0x79, 0x2c, 0xfe, 0x99 } }
#define PULSAR_IDD_HARDWARE_ID L"PulsarSplit_IndirectDisplay"

// ---- IOCTLs: CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800 + n, METHOD_BUFFERED, READ|WRITE) -------

#define PULSAR_IOCTL(n) ((0x22u << 16) | (3u << 14) | ((0x800u + (n)) << 2) | 0u)
#define IOCTL_PULSAR_GET_INFO        PULSAR_IOCTL(0) // 0x0022E000
#define IOCTL_PULSAR_SET_SEAT_CONFIG PULSAR_IOCTL(1) // 0x0022E004
#define IOCTL_PULSAR_REQUEST_MODE    PULSAR_IOCTL(2) // 0x0022E008
#define IOCTL_PULSAR_GET_STATE       PULSAR_IOCTL(3) // 0x0022E00C
#define IOCTL_PULSAR_WAIT_EVENT      PULSAR_IOCTL(4) // 0x0022E010
#define IOCTL_PULSAR_SHUTDOWN_SEAT   PULSAR_IOCTL(5) // 0x0022E014

#define PULSAR_BUFFER_VERSION 1

#pragma pack(push, 1)

typedef struct PULSAR_BUF_HEADER {
    uint32_t Size;     // bytes of the whole buffer
    uint16_t Version;  // PULSAR_BUFFER_VERSION
    uint16_t Reserved;
} PULSAR_BUF_HEADER;   // 8

// GET_INFO output
#define PULSAR_INFO_FP16_CAPABLE        0x1u
#define PULSAR_INFO_HDR_CAPABLE         0x2u
#define PULSAR_INFO_IDDCX_1_10_FUNCTIONS 0x4u
#define PULSAR_INFO_REMOTE_SESSION      0x8u
typedef struct PULSAR_INFO_OUT {
    PULSAR_BUF_HEADER Header;
    uint16_t DriverVersion[4];  // major, minor, build, revision
    uint32_t IddCxVersion;      // IddCxGetVersion
    uint32_t Flags;             // PULSAR_INFO_*
    uint64_t MaxPipelineRate;   // pixels per second over all monitors
} PULSAR_INFO_OUT;              // 32

// SET_SEAT_CONFIG input
#define PULSAR_SEAT_HDR_ALLOWED   0x1u
#define PULSAR_SEAT_ADAPTER_PINNED 0x2u
typedef struct PULSAR_SEAT_CONFIG_IN {
    PULSAR_BUF_HEADER Header;
    uint8_t  SeatGuid[16];         // GUID in memory order
    uint32_t AdapterLuidLow;
    int32_t  AdapterLuidHigh;
    uint32_t Flags;                // PULSAR_SEAT_*
    uint32_t SdrWhiteNits;         // 80..480
    uint32_t MinLuminanceX10000;   // 1/10 000 nit
    uint32_t MaxLuminanceNits;
    uint32_t MaxFullFrameNits;
    uint32_t ScaleStatic;          // 0 dynamic, 1 static
    uint32_t ScalePercent;         // static scale 100..500
    uint32_t SuperSamplingPercent; // 100..500
} PULSAR_SEAT_CONFIG_IN;           // 64

// SET_SEAT_CONFIG input with the frame reader (2026-11-14, additive by size, buffer version
// stays 1): the v1 fields, then the string SID that may open the seat's frame objects
// besides SYSTEM. Empty = SYSTEM only (the stream server runs as SYSTEM). A 64-byte v1
// buffer is still accepted and means the same as an empty SID.
#define PULSAR_MAX_SID 96
typedef struct PULSAR_SEAT_CONFIG_IN_V2 {
    PULSAR_SEAT_CONFIG_IN Base;
    uint16_t ReaderSid[PULSAR_MAX_SID]; // UTF-16, NUL-terminated, characters S 0-9 - only
} PULSAR_SEAT_CONFIG_IN_V2;        // 256

// REQUEST_MODE input (stream size, already clamped by the service)
typedef struct PULSAR_MODE_IN {
    PULSAR_BUF_HEADER Header;
    uint32_t RequestId;
    uint32_t Width;
    uint32_t Height;
    uint32_t RefreshHz;
    uint32_t Hdr;
    uint32_t Reserved;
} PULSAR_MODE_IN;                  // 32

// ModeStatus (display.mode.result status)
#define PULSAR_MODE_OK              0u
#define PULSAR_MODE_OK_SDR_FALLBACK 1u
#define PULSAR_MODE_CLAMPED         2u
#define PULSAR_MODE_NO_DISPLAY      3u
#define PULSAR_MODE_REJECTED        4u
#define PULSAR_MODE_TIMEOUT         5u
#define PULSAR_MODE_UNSUPPORTED     6u

typedef struct PULSAR_MODE_OUT {
    PULSAR_BUF_HEADER Header;
    uint32_t RequestId;
    uint32_t Status;        // PULSAR_MODE_*
    uint32_t Width;
    uint32_t Height;
    uint32_t RefreshHz;
    uint32_t Hdr;
    uint32_t RenderWidth;
    uint32_t RenderHeight;
    uint32_t DxgiFormat;    // 87 or 10
    uint32_t ScalePercent;
} PULSAR_MODE_OUT;          // 48

// GET_STATE output
#define PULSAR_STATE_ADAPTER_READY   0x01u
#define PULSAR_STATE_MONITOR_PRESENT 0x02u
#define PULSAR_STATE_PATH_ACTIVE     0x04u
#define PULSAR_STATE_HDR_ACTIVE      0x08u
#define PULSAR_STATE_FIRST_FRAME     0x10u
#define PULSAR_STATE_SEAT_CONFIGURED 0x20u
typedef struct PULSAR_STATE_OUT {
    PULSAR_BUF_HEADER Header;
    uint32_t Flags;
    uint32_t Width;
    uint32_t Height;
    uint32_t RefreshHz;
    uint32_t RenderWidth;
    uint32_t RenderHeight;
    uint32_t DxgiFormat;   // 0 = no committed mode
    uint32_t Reserved0;
    uint64_t FrameNumber;
    uint32_t LastError;    // last NTSTATUS, 0 = none
    uint32_t Reserved1;
} PULSAR_STATE_OUT;        // 56

// WAIT_EVENT output
#define PULSAR_EVENT_READY           1u // Data0 = phase (0 adapter, 1 monitor, 2 path, 3 first frame)
#define PULSAR_EVENT_MODE_APPLIED    2u // Data0 = request id
#define PULSAR_EVENT_MODE_REJECTED   3u // Data0 = request id
#define PULSAR_EVENT_HDR_UNAVAILABLE 4u // Data0 = NTSTATUS
#define PULSAR_EVENT_DEVICE_STOPPING 5u // Data0 = NTSTATUS
typedef struct PULSAR_EVENT_OUT {
    PULSAR_BUF_HEADER Header;
    uint32_t Kind;
    uint32_t Data0;
    uint64_t Data1;        // frame number at the event
} PULSAR_EVENT_OUT;        // 24

// ---- frame channel header v1 (spec 40 §6.4), at offset 0 of the 4 KiB section ---------

#define PULSAR_FRAME_MAGIC "PSDH"
#define PULSAR_FRAME_VERSION 1
#define PULSAR_FRAME_SECTION_SIZE 4096
#define PULSAR_FRAME_RING_SIZE 3
#define PULSAR_FRAME_FLAG_PATH_ACTIVE     0x1u
#define PULSAR_FRAME_FLAG_HDR             0x2u
#define PULSAR_FRAME_FLAG_DRIVER_STOPPING 0x4u
// Frame objects: SYSTEM, plus GA for the reader SID of SET_SEAT_CONFIG v2 when one is set
// ("(A;;GA;;;<sid>)" appended). No LOCAL SERVICE grant any more (Split OQ display-7): the
// driver keeps its creation handles, and the stream server reads as SYSTEM.
#define PULSAR_OBJECT_SDDL L"D:P(A;;GA;;;SY)"

typedef struct PULSAR_HDR_META_V1 {
    uint16_t Chromaticity[8];       // R x,y G x,y B x,y W x,y; CIE * 1024 (truncated)
    uint32_t MinLuminanceX10000;
    uint32_t MaxLuminanceX10000;
    uint32_t MaxFullFrameLuminanceX10000;
    uint32_t SdrWhiteNits;
    uint8_t  Reserved[8];
} PULSAR_HDR_META_V1;               // 40

typedef struct PULSAR_FRAME_HEADER_V1 {
    char     Magic[4];              // 0x00
    uint16_t Version;               // 0x04
    uint16_t HeaderSize;            // 0x06
    uint8_t  SeatGuid[16];          // 0x08
    uint32_t Generation;            // 0x18
    uint32_t RingSize;              // 0x1C
    uint64_t FrameNumber;           // 0x20
    uint32_t LatestIndex;           // 0x28
    uint32_t Flags;                 // 0x2C
    uint64_t FenceValue;            // 0x30
    uint64_t PresentQpc;            // 0x38
    uint32_t Width;                 // 0x40
    uint32_t Height;                // 0x44
    uint32_t RefreshMilliHz;        // 0x48
    uint32_t DxgiFormat;            // 0x4C
    uint32_t ColorSpace;            // 0x50
    uint64_t AdapterLuid;           // 0x54
    uint32_t HdrMetaGeneration;     // 0x5C
    PULSAR_HDR_META_V1 HdrMeta;     // 0x60
    uint64_t ProducerHeartbeat;     // 0x88
    uint64_t ConsumerHeartbeat;     // 0x90 (consumer writes)
    uint64_t ConsumerFrame;         // 0x98 (consumer writes)
} PULSAR_FRAME_HEADER_V1;           // 0xA0

#pragma pack(pop)

#ifdef __cplusplus
static_assert(sizeof(PULSAR_BUF_HEADER) == 8, "buffer header");
static_assert(sizeof(PULSAR_INFO_OUT) == 32, "GET_INFO");
static_assert(sizeof(PULSAR_SEAT_CONFIG_IN) == 64, "SET_SEAT_CONFIG");
static_assert(sizeof(PULSAR_SEAT_CONFIG_IN_V2) == 256, "SET_SEAT_CONFIG v2");
static_assert(sizeof(PULSAR_MODE_IN) == 32, "REQUEST_MODE in");
static_assert(sizeof(PULSAR_MODE_OUT) == 48, "REQUEST_MODE out");
static_assert(sizeof(PULSAR_STATE_OUT) == 56, "GET_STATE");
static_assert(sizeof(PULSAR_EVENT_OUT) == 24, "WAIT_EVENT");
static_assert(sizeof(PULSAR_HDR_META_V1) == 40, "hdr meta");
static_assert(sizeof(PULSAR_FRAME_HEADER_V1) == 0xA0, "frame header");
static_assert(offsetof(PULSAR_FRAME_HEADER_V1, AdapterLuid) == 0x54, "luid offset");
static_assert(offsetof(PULSAR_FRAME_HEADER_V1, HdrMeta) == 0x60, "meta offset");
static_assert(offsetof(PULSAR_FRAME_HEADER_V1, ConsumerFrame) == 0x98, "consumer offset");
#endif
