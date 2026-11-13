// SPDX-License-Identifier: GPL-3.0-only
#include "pack.h"

namespace pulsar {
namespace {

// UNICODE_STRING lengths are USHORT byte counts.
constexpr size_t kMaxChars = 0x7FFE / sizeof(wchar_t);

std::wstring ComputerName() {
    wchar_t buf[MAX_COMPUTERNAME_LENGTH + 1] = {};
    DWORD n = ARRAYSIZE(buf);
    if (!GetComputerNameW(buf, &n)) return L".";
    return std::wstring(buf, n);
}

// The password as the LSA should see it: protected (CredProtectW, documented in wincred.h)
// for the logon scenario, plain for unlock, already-protected input kept as is.
HRESULT ProtectedPassword(const SecretW& pw, CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, SecretW& out) {
    if (cpus != CPUS_LOGON || pw.Empty()) {
        out.Assign(pw.c_str(), pw.size());
        return S_OK;
    }
    CRED_PROTECTION_TYPE type = CredUnprotected;
    // CredIsProtectedW takes a non-const pointer but does not change the input.
    if (CredIsProtectedW(const_cast<LPWSTR>(pw.c_str()), &type) && type != CredUnprotected) {
        out.Assign(pw.c_str(), pw.size());
        return S_OK;
    }
    DWORD need = 0;
    // Size query: fails with ERROR_INSUFFICIENT_BUFFER and sets `need` (characters).
    CredProtectW(FALSE, const_cast<LPWSTR>(pw.c_str()), static_cast<DWORD>(pw.size() + 1), nullptr, &need, nullptr);
    if (need == 0 || need > kMaxChars + 1) return HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER);
    std::vector<wchar_t> buf(need, L'\0');
    if (!CredProtectW(FALSE, const_cast<LPWSTR>(pw.c_str()), static_cast<DWORD>(pw.size() + 1), buf.data(), &need,
                      nullptr)) {
        SecureZeroMemory(buf.data(), buf.size() * sizeof(wchar_t));
        return HRESULT_FROM_WIN32(GetLastError());
    }
    size_t len = wcsnlen(buf.data(), buf.size());
    out.Assign(buf.data(), len);
    SecureZeroMemory(buf.data(), buf.size() * sizeof(wchar_t));
    return S_OK;
}

}  // namespace

void SplitAccount(const std::wstring& account, std::wstring& domain, std::wstring& user) {
    size_t k = account.find(L'\\');
    if (k == std::wstring::npos) {
        domain.clear();
        user = account;
    } else {
        domain = account.substr(0, k);
        user = account.substr(k + 1);
    }
}

HRESULT PackLogon(const std::wstring& domainIn, const std::wstring& user, const SecretW& password,
                  CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, BYTE** out, ULONG* outSize) {
    if (!out || !outSize) return E_POINTER;
    *out = nullptr;
    *outSize = 0;
    if (cpus != CPUS_LOGON && cpus != CPUS_UNLOCK_WORKSTATION) return E_INVALIDARG;
    std::wstring domain = (domainIn.empty() || domainIn == L".") ? ComputerName() : domainIn;
    if (user.empty() || user.size() > kMaxChars || domain.size() > kMaxChars) return E_INVALIDARG;
    SecretW pw;
    HRESULT hr = ProtectedPassword(password, cpus, pw);
    if (FAILED(hr)) return hr;
    if (pw.size() > kMaxChars) return E_INVALIDARG;

    const size_t head = sizeof(KERB_INTERACTIVE_UNLOCK_LOGON);
    const size_t dBytes = domain.size() * sizeof(wchar_t);
    const size_t uBytes = user.size() * sizeof(wchar_t);
    const size_t pBytes = pw.size() * sizeof(wchar_t);
    const size_t total = head + dBytes + uBytes + pBytes;
    BYTE* buf = static_cast<BYTE*>(CoTaskMemAlloc(total));
    if (!buf) return E_OUTOFMEMORY;
    ZeroMemory(buf, total);

    auto* k = reinterpret_cast<KERB_INTERACTIVE_UNLOCK_LOGON*>(buf);
    KERB_INTERACTIVE_LOGON& l = k->Logon;
    l.MessageType = (cpus == CPUS_UNLOCK_WORKSTATION) ? KerbWorkstationUnlockLogon : KerbInteractiveLogon;
    size_t off = head;
    auto place = [&](UNICODE_STRING& us, const wchar_t* s, size_t bytes) {
        memcpy(buf + off, s, bytes);
        us.Length = static_cast<USHORT>(bytes);
        us.MaximumLength = static_cast<USHORT>(bytes);
        // Packed form: the buffer pointer holds the offset from the start of the blob.
        us.Buffer = reinterpret_cast<PWSTR>(static_cast<ULONG_PTR>(off));
        off += bytes;
    };
    place(l.LogonDomainName, domain.data(), dBytes);
    place(l.UserName, user.data(), uBytes);
    place(l.Password, pw.c_str(), pBytes);
    // LogonId stays zero (filled by the system for unlock).
    *out = buf;
    *outSize = static_cast<ULONG>(total);
    return S_OK;
}

HRESULT NegotiatePackage(ULONG* package) {
    if (!package) return E_POINTER;
    HANDLE lsa = nullptr;
    NTSTATUS st = LsaConnectUntrusted(&lsa);
    if (st != 0) return HRESULT_FROM_NT(st);
    char name[] = "Negotiate";  // NEGOSSP_NAME_A
    LSA_STRING s;
    s.Buffer = name;
    s.Length = static_cast<USHORT>(strlen(name));
    s.MaximumLength = static_cast<USHORT>(s.Length + 1);
    ULONG pkg = 0;
    st = LsaLookupAuthenticationPackage(lsa, &s, &pkg);
    LsaDeregisterLogonProcess(lsa);
    if (st != 0) return HRESULT_FROM_NT(st);
    *package = pkg;
    return S_OK;
}

}  // namespace pulsar
