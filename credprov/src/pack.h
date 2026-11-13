// SPDX-License-Identifier: GPL-3.0-only
// Logon serialization for Negotiate: a KERB_INTERACTIVE_UNLOCK_LOGON (ntsecapi.h) packed
// into one CoTaskMemAlloc buffer, UNICODE_STRING buffers given as offsets from the start
// of the buffer (the documented "packed" form LogonUI hands to LsaLogonUser).
#pragma once

#include "common.h"

namespace pulsar {

// `domain` empty or "." = this computer. The password is protected with CredProtectW
// for the logon scenario unless it already is. `out` is CoTaskMemAlloc'd (LogonUI frees it).
HRESULT PackLogon(const std::wstring& domain, const std::wstring& user, const SecretW& password,
                  CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, BYTE** out, ULONG* outSize);

// LsaLookupAuthenticationPackage("Negotiate").
HRESULT NegotiatePackage(ULONG* package);

// "DOMAIN\user" -> domain, user ("user" alone -> "", user).
void SplitAccount(const std::wstring& account, std::wstring& domain, std::wstring& user);

}  // namespace pulsar
