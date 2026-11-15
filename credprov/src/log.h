// SPDX-License-Identifier: GPL-3.0-only
// Diagnostics log of the credential provider (added after the 2026-11-15 VM test):
// %ProgramData%\Pulsar Seat\logs\credprov.log, rotated to credprov.log.1 at 1 MiB.
// Written only inside LogonUI (SYSTEM, which may write the service's logs folder).
// Never log passwords, typed text or serialization buffers: only scenarios, seat numbers,
// counts, flags, SIDs and HRESULTs.
#pragma once

namespace pulsar {

// printf-style (wide); one timestamped line with pid and session.
void Log(const wchar_t* fmt, ...);

}  // namespace pulsar
