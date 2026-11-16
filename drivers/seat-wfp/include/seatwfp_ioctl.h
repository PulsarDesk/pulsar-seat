/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat source-IP callout - control device contract (interface major 1).
 *
 * Shared by the driver (drivers/seat-wfp) and documented for the service, whose Rust
 * mirror is crates/seat-compat/src/wfp.rs (its tests pin every size, offset and IOCTL code
 * below against this file). Little-endian, natural alignment, every struct a multiple of
 * 8 bytes. Changes inside a major version are additive: new IOCTLs, or new trailing
 * fields guarded by a leading Size member.
 *
 * Control device: \Device\PulsarSeatWfp, symbolic link \DosDevices\PulsarSeatWfp (user
 * mode: \\.\PulsarSeatWfp). DACL: the Pulsar Seat service's own service SID only
 * (NT SERVICE\PulsarSeat; the service runs with SERVICE_SID_TYPE_UNRESTRICTED), exclusive
 * open: one service handle at a time.
 */
#pragma once

#define SEATWFP_INTERFACE_MAJOR 1
#define SEATWFP_INTERFACE_MINOR 0

#define SEATWFP_DEVICE_NAME   L"\\Device\\PulsarSeatWfp"
#define SEATWFP_SYMLINK_NAME  L"\\DosDevices\\PulsarSeatWfp"

/* NT SERVICE\PulsarSeat = S-1-5-80 + SHA-1 of the upper-case service name (UTF-16 LE),
 * read as five little-endian 32-bit sub-authorities (pinned by the Rust tests). */
#define SEATWFP_SERVICE_SID   L"S-1-5-80-2060529063-4084430102-647711624-1206425353-118540754"
#define SEATWFP_DEVICE_SDDL   L"D:P(A;;GA;;;S-1-5-80-2060529063-4084430102-647711624-1206425353-118540754)"

#define SEATWFP_RW_ACCESS (FILE_READ_ACCESS | FILE_WRITE_ACCESS)

/* Out: SEATWFP_VERSION. Always the first call. */
#define IOCTL_SEATWFP_GET_VERSION  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
/* In: SEATWFP_MAP_HEADER + SEATWFP_SEAT[SeatCount] + SEATWFP_PATH[PathCount] + UTF-16
 * path characters. Replaces the whole map atomically. */
#define IOCTL_SEATWFP_SET_MAP      CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, SEATWFP_RW_ACCESS)
/* No buffers. Empties the map (nothing is rewritten afterwards). */
#define IOCTL_SEATWFP_CLEAR        CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, SEATWFP_RW_ACCESS)
/* Out: SEATWFP_STATS. */
#define IOCTL_SEATWFP_GET_STATS    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, SEATWFP_RW_ACCESS)

/* SEATWFP_VERSION.Flags / SEATWFP_STATS.Flags */
#define SEATWFP_VF_PASS_THROUGH     0x1   /* Safe Mode or Parameters\PassThrough != 0: no filters */
#define SEATWFP_VF_CALLOUTS         0x2   /* callouts registered with the filter engine */
#define SEATWFP_VF_FILTERS          0x4   /* sublayer + filters added (BFE was running) */
#define SEATWFP_VF_MAP              0x8   /* a non-empty map is in force */

typedef struct _SEATWFP_VERSION {
    USHORT InterfaceMajor;
    USHORT InterfaceMinor;
    ULONG  Build;
    ULONG  Flags;
    ULONG  Reserved;
} SEATWFP_VERSION;

/* Limits of one map (the driver refuses a larger buffer with STATUS_INVALID_PARAMETER). */
#define SEATWFP_MAX_SEATS       64
#define SEATWFP_MAX_PATHS       4096
#define SEATWFP_MAX_PATH_CHARS  1024
#define SEATWFP_MAX_MAP_BYTES   (1024 * 1024)

/* SEATWFP_SEAT.Rule: which processes of the session get the address. */
#define SEATWFP_RULE_ALL              0
#define SEATWFP_RULE_ONLY_LISTED      1
#define SEATWFP_RULE_ALL_EXCEPT_LISTED 2

/* SEATWFP_SEAT.Flags */
#define SEATWFP_SF_BIND     0x1   /* wildcard binds (0.0.0.0) of TCP / UDP get the address */
#define SEATWFP_SF_CONNECT  0x2   /* outbound connects get it as their local address */

typedef struct _SEATWFP_MAP_HEADER {
    ULONG  Size;          /* total bytes of the buffer, header included */
    USHORT Major;         /* SEATWFP_INTERFACE_MAJOR */
    USHORT Minor;
    ULONG  Generation;    /* service counter, reported back by GET_STATS */
    ULONG  SeatCount;
    ULONG  PathCount;
    ULONG  Reserved;
} SEATWFP_MAP_HEADER;

typedef struct _SEATWFP_SEAT {
    ULONG  SessionId;     /* Windows session id of the seat (one seat per session) */
    UCHAR  Address[4];    /* IPv4, octets in dotted order (network byte order) */
    USHORT Rule;          /* SEATWFP_RULE_* */
    USHORT Flags;         /* SEATWFP_SF_* */
    ULONG  FirstPath;     /* index into the SEATWFP_PATH table */
    ULONG  PathCount;     /* paths of this seat's list */
    ULONG  Seat;          /* seat number (diagnostics only) */
    ULONG  Reserved[2];
} SEATWFP_SEAT;

/* One list entry: an NT path (\device\harddiskvolume3\games\lan), lower case, no
 * trailing backslash. A process matches when its image path equals it or lies below it. */
typedef struct _SEATWFP_PATH {
    ULONG  Offset;        /* byte offset of the characters from the start of the buffer */
    USHORT Chars;         /* UTF-16 code units, no terminator */
    USHORT Reserved;
} SEATWFP_PATH;

typedef struct _SEATWFP_STATS {
    ULONGLONG BindRedirects;
    ULONGLONG ConnectRedirects;
    ULONGLONG Skipped;        /* matched a seat but could not be rewritten (rights, IRQL, errors) */
    ULONG     Generation;     /* of the map in force, 0 = none */
    ULONG     SeatCount;
    ULONG     Flags;          /* SEATWFP_VF_* */
    ULONG     Reserved;
} SEATWFP_STATS;

#if defined(C_ASSERT)
C_ASSERT(sizeof(SEATWFP_VERSION) == 16);
C_ASSERT(sizeof(SEATWFP_MAP_HEADER) == 24);
C_ASSERT(sizeof(SEATWFP_SEAT) == 32);
C_ASSERT(sizeof(SEATWFP_PATH) == 8);
C_ASSERT(sizeof(SEATWFP_STATS) == 40);
#endif
