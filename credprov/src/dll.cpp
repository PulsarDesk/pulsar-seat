// SPDX-License-Identifier: GPL-3.0-only
// COM in-process server entry points. No DllRegisterServer: the Pulsar Seat service
// registers the CLSIDs itself, only while the product is enabled under fast user switching
// (seat_session::credprov), and removes them on disable / uninstall.
#include "provider.h"

namespace pulsar {
namespace {

LONG g_refs = 0;

class ClassFactory final : public IClassFactory {
public:
    explicit ClassFactory(bool filter) : filter_(filter) { DllAddRef(); }

    IFACEMETHODIMP QueryInterface(REFIID riid, void** ppv) override {
        if (!ppv) return E_POINTER;
        *ppv = nullptr;
        if (riid == IID_IUnknown || riid == IID_IClassFactory) {
            *ppv = static_cast<IClassFactory*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    IFACEMETHODIMP_(ULONG) AddRef() override { return static_cast<ULONG>(InterlockedIncrement(&refs_)); }
    IFACEMETHODIMP_(ULONG) Release() override {
        LONG r = InterlockedDecrement(&refs_);
        if (r == 0) delete this;
        return static_cast<ULONG>(r);
    }

    IFACEMETHODIMP CreateInstance(IUnknown* outer, REFIID riid, void** ppv) override {
        if (!ppv) return E_POINTER;
        *ppv = nullptr;
        if (outer) return CLASS_E_NOAGGREGATION;
        HRESULT hr = E_OUTOFMEMORY;
        try {
            if (filter_) {
                auto* f = new ProviderFilter();
                hr = f->QueryInterface(riid, ppv);
                f->Release();
            } else {
                auto* p = new Provider();
                hr = p->QueryInterface(riid, ppv);
                p->Release();
            }
        } catch (...) {
            hr = E_OUTOFMEMORY;
        }
        return hr;
    }
    IFACEMETHODIMP LockServer(BOOL lock) override {
        if (lock) DllAddRef();
        else DllRelease();
        return S_OK;
    }

private:
    ~ClassFactory() { DllRelease(); }
    LONG refs_ = 1;
    bool filter_;
};

}  // namespace

void DllAddRef() { InterlockedIncrement(&g_refs); }
void DllRelease() { InterlockedDecrement(&g_refs); }

bool InLogonUI() {
    wchar_t path[MAX_PATH * 2] = {};
    DWORD n = GetModuleFileNameW(nullptr, path, ARRAYSIZE(path));
    if (n == 0 || n >= ARRAYSIZE(path)) return false;
    wchar_t sysdir[MAX_PATH] = {};
    UINT s = GetSystemDirectoryW(sysdir, ARRAYSIZE(sysdir));
    if (s == 0 || s >= ARRAYSIZE(sysdir)) return false;
    std::wstring want = std::wstring(sysdir) + L"\\LogonUI.exe";
    return CompareStringOrdinal(path, static_cast<int>(n), want.c_str(), static_cast<int>(want.size()), TRUE) == CSTR_EQUAL;
}

HRESULT DupString(const wchar_t* s, wchar_t** out) {
    if (!out) return E_POINTER;
    *out = nullptr;
    if (!s) s = L"";
    size_t n = wcslen(s) + 1;
    auto* p = static_cast<wchar_t*>(CoTaskMemAlloc(n * sizeof(wchar_t)));
    if (!p) return E_OUTOFMEMORY;
    memcpy(p, s, n * sizeof(wchar_t));
    *out = p;
    return S_OK;
}

}  // namespace pulsar

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) DisableThreadLibraryCalls(instance);
    return TRUE;
}

STDAPI DllCanUnloadNow() {
    return pulsar::g_refs == 0 ? S_OK : S_FALSE;
}

STDAPI DllGetClassObject(REFCLSID clsid, REFIID riid, void** ppv) {
    if (!ppv) return E_POINTER;
    *ppv = nullptr;
    bool filter;
    if (IsEqualCLSID(clsid, pulsar::kProviderClsid)) filter = false;
    else if (IsEqualCLSID(clsid, pulsar::kFilterClsid)) filter = true;
    else return CLASS_E_CLASSNOTAVAILABLE;
    pulsar::ClassFactory* f = nullptr;
    try {
        f = new pulsar::ClassFactory(filter);
    } catch (...) {
        return E_OUTOFMEMORY;
    }
    HRESULT hr = f->QueryInterface(riid, ppv);
    f->Release();
    return hr;
}
