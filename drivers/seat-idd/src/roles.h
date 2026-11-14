/* SPDX-License-Identifier: GPL-3.0-only */
/* Roles of the one Pulsar Seat IddCx driver (roles.cpp). */
#pragma once

#include <stddef.h>

/* Hardware ids, one per role (seat-idd.inf models). The remote id is also
 * crates/seat-display/src/remote/ioctl.rs PULSAR_IDD_HARDWARE_ID. */
#define PSIDD_HWID_LOCAL  L"Root\\PulsarSeatIdd"
#define PSIDD_HWID_REMOTE L"PulsarSplit_IndirectDisplay"

typedef enum _PSIDD_ROLE {
    PsiddRoleUnknown = 0,
    PsiddRoleLocal = 1,
    PsiddRoleRemote = 2,
} PSIDD_ROLE;

/* Role of a REG_MULTI_SZ hardware id list of `cch` characters (the first known id wins). */
PSIDD_ROLE PsiddRoleOfHardwareIds(const WCHAR *multiSz, size_t cch);

/* Remote role device add (remote/Driver.cpp). */
namespace pulsar {
NTSTATUS RemoteDeviceAdd(WDFDRIVER driver, PWDFDEVICE_INIT deviceInit);
}