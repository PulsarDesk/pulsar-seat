// SPDX-License-Identifier: GPL-3.0-only
// Pulsar Seat credential provider + filter: shared declarations.
//
// Written from the public Microsoft Learn documentation of ICredentialProvider,
// ICredentialProviderCredential2, ICredentialProviderFilter, KERB_INTERACTIVE_UNLOCK_LOGON
// and the named-pipe APIs. No sample code is copied.
#pragma once

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <unknwn.h>
#include <credentialprovider.h>
#define SECURITY_WIN32
#include <ntsecapi.h>
#include <sspi.h>
#include <wincred.h>
#include <strsafe.h>

#include <string>
#include <vector>

namespace pulsar {

// {C086BD57-C066-4FBA-ACED-7679C157E3DC}; same value as seat_session::credprov::PROVIDER_CLSID.
inline constexpr CLSID kProviderClsid = {0xc086bd57, 0xc066, 0x4fba, {0xac, 0xed, 0x76, 0x79, 0xc1, 0x57, 0xe3, 0xdc}};
// {FDC3F428-E8C8-455F-AA01-6CEB41E3155E}; seat_session::credprov::FILTER_CLSID.
inline constexpr CLSID kFilterClsid = {0xfdc3f428, 0xe8c8, 0x455f, {0xaa, 0x01, 0x6c, 0xeb, 0x41, 0xe3, 0x15, 0x5e}};

// Object / lock count for DllCanUnloadNow.
void DllAddRef();
void DllRelease();

// True only inside %SystemRoot%\System32\LogonUI.exe. Everywhere else (CredUI hosts,
// anything that loads the DLL by accident) provider and filter do nothing.
bool InLogonUI();

// Wide string whose buffer is wiped on destruction / clear. Used for passwords.
class SecretW {
public:
    SecretW() = default;
    SecretW(const SecretW&) = delete;
    SecretW& operator=(const SecretW&) = delete;
    ~SecretW() { Clear(); }
    void Assign(const wchar_t* s, size_t n) {
        Clear();
        // One allocation of the final size: no reallocation leaves an unwiped copy behind.
        buf_.reserve(n + 1);
        buf_.assign(s, s + n);
        buf_.push_back(L'\0');
    }
    void Clear() {
        if (!buf_.empty()) {
            SecureZeroMemory(buf_.data(), buf_.size() * sizeof(wchar_t));
        }
        buf_.clear();
    }
    bool Empty() const { return buf_.size() <= 1; }
    const wchar_t* c_str() const { return buf_.empty() ? L"" : buf_.data(); }
    size_t size() const { return buf_.empty() ? 0 : buf_.size() - 1; }

private:
    std::vector<wchar_t> buf_;
};

// Wipe a narrow string (UTF-8 JSON that carried a secret).
inline void Wipe(std::string& s) {
    // Whole capacity (resize within it never reallocates).
    s.resize(s.capacity());
    if (!s.empty()) {
        SecureZeroMemory(s.data(), s.size());
    }
    s.clear();
}

// Wipes a narrow string when it goes out of scope (every return path).
class WipeOnExit {
public:
    explicit WipeOnExit(std::string& s) : s_(s) {}
    WipeOnExit(const WipeOnExit&) = delete;
    WipeOnExit& operator=(const WipeOnExit&) = delete;
    ~WipeOnExit() { Wipe(s_); }

private:
    std::string& s_;
};

// CoTaskMemAlloc copy (LogonUI frees field strings with CoTaskMemFree).
HRESULT DupString(const wchar_t* s, wchar_t** out);

}  // namespace pulsar
