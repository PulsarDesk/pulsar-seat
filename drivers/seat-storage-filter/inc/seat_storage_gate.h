/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat storage minifilter: communication-port protocol.
 *
 * Shared contract with crates/seat-media/src/gate/minifilter.rs (module `proto`). Any change
 * here must be made there too; the Rust `message_layout` test pins the sizes. Changes inside
 * one interface major are additive only; a new major gets a new port name
 * (drivers/README.md).
 *
 * All integers little endian; structures packed (no padding). Every message starts with
 * PSSG_HEADER; Size is the total message size including the header.
 */
#pragma once

#define PSSG_PORT_NAME L"\\PulsarSeatStorageGate" /* seat_core::consts::MINIFILTER_PORT_NAME */

#define PSSG_MAGIC 0x47535350u /* 'P' 'S' 'S' 'G' read as little-endian UINT32 */
#define PSSG_INTERFACE_MAJOR 1  /* seat_core::consts::DRIVER_INTERFACE_MAJOR */
#define PSSG_INTERFACE_MINOR 0

#define PSSG_MAX_SESSIONS 64
#define PSSG_MAX_VOLUMES 256
#define PSSG_VOLUME_GUID_CHARS 40 /* "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}" + NUL padding */

/* Commands. Reply: PSSG_VERSION_REPLY for QUERY_VERSION, else one INT32 NTSTATUS. */
#define PSSG_CMD_QUERY_VERSION 1 /* no payload; always the first message of a client */
#define PSSG_CMD_BEGIN_PLAN 2    /* PSSG_BEGIN_PLAN: clears the staged plan */
#define PSSG_CMD_SET_SESSION 3   /* PSSG_SET_SESSION: session id -> seat bit (staged) */
#define PSSG_CMD_SET_VOLUME 4    /* PSSG_SET_VOLUME: volume GUID -> owner seat mask (staged) */
#define PSSG_CMD_COMMIT_PLAN 5   /* no payload: staged plan becomes active, filter armed */
#define PSSG_CMD_PASS_ALL 6      /* no payload: disarm (pass everything) until the next commit */

#define PSSG_FLAG_ADMINS_BYPASS 0x1u

/* All 32 bits set = shared by every seat, including seats added later. Same rule as
 * seat_core::SeatMask::is_shared and seat_media::gate::is_shared (pinned by the Rust test
 * gate::tests::sddl_building). A mask listing all 16 seats bit by bit is NOT shared. */
#define PSSG_SHARED_MASK 0xFFFFFFFFu

#pragma pack(push, 1)

typedef struct _PSSG_HEADER {
    unsigned long Magic;
    unsigned short Major;
    unsigned short Minor;
    unsigned long Command;
    unsigned long Size;
} PSSG_HEADER;

typedef struct _PSSG_VERSION_REPLY {
    unsigned short Major;
    unsigned short Minor;
    unsigned long Build;
} PSSG_VERSION_REPLY;

typedef struct _PSSG_BEGIN_PLAN {
    PSSG_HEADER Header;
    unsigned long Flags;
} PSSG_BEGIN_PLAN;

typedef struct _PSSG_SET_SESSION {
    PSSG_HEADER Header;
    unsigned long SessionId;
    unsigned long SeatBit; /* bit 0 = seat 1 */
} PSSG_SET_SESSION;

typedef struct _PSSG_SET_VOLUME {
    PSSG_HEADER Header;
    unsigned long SeatMask;
    wchar_t VolumeGuid[PSSG_VOLUME_GUID_CHARS]; /* lower case, braces included */
} PSSG_SET_VOLUME;

#pragma pack(pop)

/* Layout checks (match the Rust test). */
typedef char PSSG_ASSERT_HEADER[(sizeof(PSSG_HEADER) == 16) ? 1 : -1];
typedef char PSSG_ASSERT_VOLUME[(sizeof(PSSG_SET_VOLUME) == 16 + 4 + 80) ? 1 : -1];
typedef char PSSG_ASSERT_VERSION[(sizeof(PSSG_VERSION_REPLY) == 8) ? 1 : -1];
