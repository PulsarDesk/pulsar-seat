/* SPDX-License-Identifier: GPL-3.0-only */
/*
 * Pulsar Seat input filter - control device contract (interface major 1).
 *
 * Shared by the driver (drivers/seat-input-filter) and documented for the service, whose
 * Rust mirror is crates/seat-input/src/filter_proto.rs (its tests pin every size and IOCTL
 * code below). Little-endian, natural alignment, every struct a multiple of 8 bytes.
 * Changes inside a major version are additive: new IOCTLs, or new trailing fields guarded
 * by the leading Size member.
 *
 * Control device: \Device\PulsarSeatInput, symbolic link \DosDevices\PulsarSeatInput
 * (user mode: \\.\PulsarSeatInput). DACL SYSTEM only (SDDL_DEVOBJ_SYS_ALL), exclusive
 * open: only the Pulsar Seat service talks to the filter.
 */
#pragma once

#define SEATINPUT_INTERFACE_MAJOR 1
#define SEATINPUT_INTERFACE_MINOR 1   /* 1: SEATINPUT_CONFIG.EmergencyScan (2026-11-13) */

#define SEATINPUT_DEVICE_NAME   L"\\Device\\PulsarSeatInput"
#define SEATINPUT_SYMLINK_NAME  L"\\DosDevices\\PulsarSeatInput"

/* Private DEVPROPKEY {0527596e-0b5b-4dcf-88b3-118399aa27b1}: pid 2 = seat mask (UINT32),
 * pid 3 = config revision (UINT64). Written by the service on the unit's devnode. */
#define SEATINPUT_DEVPROP_GUID_INIT \
    { 0x0527596e, 0x0b5b, 0x4dcf, { 0x88, 0xb3, 0x11, 0x83, 0x99, 0xaa, 0x27, 0xb1 } }
#define SEATINPUT_DEVPROP_MASK_PID      2
#define SEATINPUT_DEVPROP_REVISION_PID  3

#define SEATINPUT_RW_ACCESS (FILE_READ_ACCESS | FILE_WRITE_ACCESS)

/* Out: SEATINPUT_VERSION. Always the first call. */
#define IOCTL_SEATINPUT_GET_VERSION   CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
/* In: SEATINPUT_ATTACH. Ends fail-open mode until reboot. */
#define IOCTL_SEATINPUT_ATTACH        CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, SEATINPUT_RW_ACCESS)
/* In: SEATINPUT_CONFIG. */
#define IOCTL_SEATINPUT_SET_CONFIG    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, SEATINPUT_RW_ACCESS)
/* In: SEATINPUT_UNIT_MASK. */
#define IOCTL_SEATINPUT_SET_UNIT      CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, SEATINPUT_RW_ACCESS)
/* In: SEATINPUT_SESSIONS. */
#define IOCTL_SEATINPUT_SET_SESSIONS  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_BUFFERED, SEATINPUT_RW_ACCESS)
/* Out: SEATINPUT_PACKET[]. Inverted call: pended until packets exist. */
#define IOCTL_SEATINPUT_READ_PACKETS  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x805, METHOD_BUFFERED, SEATINPUT_RW_ACCESS)
/* Out: SEATINPUT_UNIT_LIST header followed by SEATINPUT_UNIT_INFO[]. */
#define IOCTL_SEATINPUT_LIST_UNITS    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x806, METHOD_BUFFERED, SEATINPUT_RW_ACCESS)

/* SEATINPUT_VERSION.Flags */
#define SEATINPUT_VF_PASS_THROUGH   0x1   /* Safe Mode or Parameters\PassThrough != 0 */
#define SEATINPUT_VF_ATTACHED_ONCE  0x2
#define SEATINPUT_VF_ATTACHED       0x4

typedef struct _SEATINPUT_VERSION {
    USHORT InterfaceMajor;
    USHORT InterfaceMinor;
    ULONG  Build;
    ULONG  Flags;
    ULONG  Reserved;
} SEATINPUT_VERSION;

typedef struct _SEATINPUT_ATTACH {
    ULONG  Size;
    USHORT Major;
    USHORT Minor;
} SEATINPUT_ATTACH;

/* SEATINPUT_CONFIG.Flags */
#define SEATINPUT_CF_WIZARD_CAPTURE        0x1
#define SEATINPUT_CF_ALLOW_SYSTEM_BUTTONS  0x2
#define SEATINPUT_CF_HOTKEY_ENABLED        0x4

#define SEATINPUT_MAX_UNFILTERED 16

/* Unfiltered HID collection usage; Usage 0 (reserved in every page) matches the whole page. */
typedef struct _SEATINPUT_USAGE {
    USHORT Page;
    USHORT Usage;
} SEATINPUT_USAGE;

typedef struct _SEATINPUT_CONFIG {
    ULONG  Size;
    ULONG  Flags;
    USHORT HotkeyScan;        /* set-1 make code, 0 = off */
    USHORT HotkeyFlags;       /* KEY_E0 (2) for an extended key */
    ULONG  GapMs;             /* service-gap buffer age limit */
    ULONG  GapPackets;        /* service-gap buffer size per unit while detached */
    ULONG  UnfilteredCount;
    SEATINPUT_USAGE Unfiltered[SEATINPUT_MAX_UNFILTERED];
    /* ---- minor 1 (Size >= sizeof(SEATINPUT_CONFIG)) ---- */
    USHORT EmergencyScan;     /* Ctrl+Shift+key on a seat-1 unit = emergency press, checked
                                 on its own (also with the move hotkey off); 0 = none */
    USHORT EmergencyFlags;    /* KEY_E0 (2) for an extended key */
    ULONG  Reserved1;
} SEATINPUT_CONFIG;

/* Size of a minor-0 SEATINPUT_CONFIG (no emergency fields). The driver accepts it and then
 * uses the move hotkey as the emergency key, as minor 0 did. */
#define SEATINPUT_CONFIG_V0_SIZE 88

#define SEATINPUT_MAX_INSTANCE_ID 200   /* MAX_DEVICE_ID_LEN, including the NUL */

typedef struct _SEATINPUT_UNIT_MASK {
    ULONG     Size;
    ULONG     Mask;                     /* bit 0 = seat 1; 0 = spare; 0xFFFFFFFF = shared */
    ULONGLONG Revision;
    WCHAR     InstanceId[SEATINPUT_MAX_INSTANCE_ID];
} SEATINPUT_UNIT_MASK;

#define SEATINPUT_MAX_SEATS 16
#define SEATINPUT_NO_SESSION 0xFFFFFFFFu

typedef struct _SEATINPUT_SESSIONS {
    ULONG Size;
    ULONG Count;                        /* 16 */
    ULONG SessionOfSeat[SEATINPUT_MAX_SEATS];
} SEATINPUT_SESSIONS;

/* SEATINPUT_PACKET.Kind */
#define SEATINPUT_PK_KEYBOARD 1
#define SEATINPUT_PK_MOUSE    2
#define SEATINPUT_PK_HOTKEY   3         /* seat-1 unit pressed Ctrl + hotkey (swallowed); A = SEATINPUT_HK_* */

/* SEATINPUT_PACKET.A of a SEATINPUT_PK_HOTKEY packet */
#define SEATINPUT_HK_SHIFT    0x1       /* Shift was down: emergency press, not the move hotkey */

/* SEATINPUT_PACKET.Flags */
#define SEATINPUT_PF_GAP           0x1  /* packets of this unit were dropped before this one */
#define SEATINPUT_PF_CONSOLE_COPY  0x2  /* copy of a seat-1 packet (wizard / activity) */
#define SEATINPUT_PF_WIZARD        0x4  /* spare unit captured for the wizard */

typedef struct _SEATINPUT_PACKET {
    ULONG     UnitId;
    UCHAR     Kind;
    UCHAR     Seat;                     /* 1-based owner seat, 0 = none */
    USHORT    Flags;
    ULONGLONG Time;                     /* KeQueryInterruptTime, 100 ns */
    USHORT    A;                        /* keyboard: MakeCode; mouse: Flags; hotkey: SEATINPUT_HK_* */
    USHORT    B;                        /* keyboard: Flags;    mouse: ButtonFlags */
    USHORT    C;                        /* mouse: ButtonData */
    USHORT    Reserved;
    LONG      LastX;
    LONG      LastY;
} SEATINPUT_PACKET;

/* SEATINPUT_UNIT_INFO.Role */
#define SEATINPUT_ROLE_KEYBOARD 1
#define SEATINPUT_ROLE_MOUSE    2
#define SEATINPUT_ROLE_HID      3
#define SEATINPUT_ROLE_EXCLUDED 4

typedef struct _SEATINPUT_UNIT_INFO {
    ULONG     UnitId;
    ULONG     Role;
    ULONG     Mask;
    ULONG     Reserved;
    ULONGLONG Packets;
    ULONGLONG LastActivity;             /* KeQueryInterruptTime of the last packet */
    WCHAR     InstanceId[SEATINPUT_MAX_INSTANCE_ID];
} SEATINPUT_UNIT_INFO;

typedef struct _SEATINPUT_UNIT_LIST {
    ULONG Size;                         /* bytes written */
    ULONG Count;                        /* entries that follow */
    ULONG Total;                        /* units known to the driver */
    ULONG Reserved;
} SEATINPUT_UNIT_LIST;

C_ASSERT(sizeof(SEATINPUT_VERSION) == 16);
C_ASSERT(sizeof(SEATINPUT_ATTACH) == 8);
C_ASSERT(sizeof(SEATINPUT_CONFIG) == 96);
C_ASSERT(FIELD_OFFSET(SEATINPUT_CONFIG, EmergencyScan) == SEATINPUT_CONFIG_V0_SIZE);
C_ASSERT(sizeof(SEATINPUT_UNIT_MASK) == 416);
C_ASSERT(sizeof(SEATINPUT_SESSIONS) == 72);
C_ASSERT(sizeof(SEATINPUT_PACKET) == 32);
C_ASSERT(sizeof(SEATINPUT_UNIT_INFO) == 432);
C_ASSERT(sizeof(SEATINPUT_UNIT_LIST) == 16);
