// SPDX-License-Identifier: GPL-3.0-only
#include "provider.h"

#include "pack.h"

#include <new>

namespace pulsar {
namespace {

// Cache of the service's answer in this LogonUI process.
SRWLOCK g_cacheLock = SRWLOCK_INIT;
struct CacheEntry {
    bool valid = false;
    ULONGLONG at = 0;
    ULONGLONG gen = 0;
    SessionInfo info;
};
CacheEntry g_cache[2];
constexpr ULONGLONG kCacheMs = 30'000;
// The filter reuses an answer at most this old (else it asks the service again).
constexpr ULONGLONG kFilterMaxAgeMs = 2'000;

const wchar_t* LabelOf(DWORD field) {
    switch (field) {
        case kFieldTitle: return L"Pulsar Seat";
        case kFieldAccount: return L"Hesap";
        case kFieldPassword: return L"Parola";
        case kFieldSubmit: return L"Gönder";
        default: return L"Koltuk değiştir";
    }
}

CREDENTIAL_PROVIDER_FIELD_TYPE TypeOf(DWORD field) {
    switch (field) {
        case kFieldTitle: return CPFT_LARGE_TEXT;
        case kFieldAccount: return CPFT_SMALL_TEXT;
        case kFieldPassword: return CPFT_PASSWORD_TEXT;
        case kFieldSubmit: return CPFT_SUBMIT_BUTTON;
        default: return CPFT_COMMAND_LINK;
    }
}

bool IsWanted(CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus) {
    return cpus == CPUS_LOGON || cpus == CPUS_UNLOCK_WORKSTATION;
}

}  // namespace

namespace {

size_t SlotOf(Scenario scenario) { return scenario == Scenario::Unlock ? 1 : 0; }

// Under g_cacheLock (exclusive).
bool StoreLocked(size_t k, const SessionInfo& info, ULONGLONG now) {
    CacheEntry& e = g_cache[k];
    const bool changed = !e.valid || !SameSession(e.info, info);
    e.valid = true;
    e.at = now;
    e.info = info;
    if (changed) ++e.gen;
    return changed;
}

}  // namespace

SessionInfo CachedQuery(Scenario scenario, bool refresh, ULONGLONG* generation) {
    return CachedQueryMaxAge(scenario, refresh ? 0 : kCacheMs, generation);
}

SessionInfo CachedQueryMaxAge(Scenario scenario, ULONGLONG maxAgeMs, ULONGLONG* generation) {
    const size_t k = SlotOf(scenario);
    const ULONGLONG now = GetTickCount64();
    if (maxAgeMs > 0) {
        AcquireSRWLockShared(&g_cacheLock);
        CacheEntry e = g_cache[k];
        ReleaseSRWLockShared(&g_cacheLock);
        if (e.valid && now - e.at < maxAgeMs) {
            if (generation) *generation = e.gen;
            return e.info;
        }
    }
    SessionInfo info = ServiceClient::Query(scenario);
    AcquireSRWLockExclusive(&g_cacheLock);
    StoreLocked(k, info, now);
    if (generation) *generation = g_cache[k].gen;
    ReleaseSRWLockExclusive(&g_cacheLock);
    return info;
}

bool StoreAnswer(Scenario scenario, const SessionInfo& info) {
    AcquireSRWLockExclusive(&g_cacheLock);
    const bool changed = StoreLocked(SlotOf(scenario), info, GetTickCount64());
    ReleaseSRWLockExclusive(&g_cacheLock);
    return changed;
}

bool SeatTileWillExist(const SessionInfo& info, CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus) {
    return info.reachable && info.seat != 0 && cpus == CPUS_LOGON && !info.account.empty();
}

bool FilterHides(const SessionInfo& info) {
    return info.hideOthers && info.autoSubmit && SeatTileWillExist(info, CPUS_LOGON);
}

// ---- Notifier ------------------------------------------------------------------------

Notifier* Notifier::Create() {
    auto* n = new (std::nothrow) Notifier();
    if (!n) return nullptr;
    n->stop_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!n->stop_) {
        delete n;
        return nullptr;
    }
    return n;
}

Notifier::~Notifier() {
    if (events_) events_->Release();
    if (stop_) CloseHandle(stop_);
}

void Notifier::AddRef() { InterlockedIncrement(&refs_); }

void Notifier::Release() {
    if (InterlockedDecrement(&refs_) == 0) delete this;
}

void Notifier::Advise(ICredentialProviderEvents* events, UINT_PTR context) {
    AcquireSRWLockExclusive(&lock_);
    ICredentialProviderEvents* old = events_;
    events_ = events;
    if (events_) events_->AddRef();
    context_ = context;
    ResetEvent(stop_);
    ReleaseSRWLockExclusive(&lock_);
    if (old) old->Release();
}

void Notifier::UnAdvise() {
    AcquireSRWLockExclusive(&lock_);
    ICredentialProviderEvents* old = events_;
    events_ = nullptr;
    context_ = 0;
    SetEvent(stop_);
    ReleaseSRWLockExclusive(&lock_);
    if (old) old->Release();
}

void Notifier::Fire() {
    AcquireSRWLockShared(&lock_);
    ICredentialProviderEvents* e = events_;
    const UINT_PTR ctx = context_;
    if (e) e->AddRef();
    ReleaseSRWLockShared(&lock_);
    if (e) {
        e->CredentialsChanged(ctx);
        e->Release();
    }
}

void Notifier::Refresh(DWORD maxMs) {
    if (InterlockedCompareExchange(&busy_, 1, 0) != 0) return;
    maxMs_ = maxMs;
    // The worker keeps the DLL loaded (FreeLibraryAndExitThread at its end) and this object
    // alive.
    HMODULE mod = nullptr;
    if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, reinterpret_cast<LPCWSTR>(&Notifier::ThreadProc), &mod)) {
        InterlockedExchange(&busy_, 0);
        return;
    }
    module_ = mod;
    AddRef();
    HANDLE t = CreateThread(nullptr, 0, &Notifier::ThreadProc, this, 0, nullptr);
    if (!t) {
        module_ = nullptr;
        InterlockedExchange(&busy_, 0);
        Release();
        FreeLibrary(mod);
        return;
    }
    CloseHandle(t);
}

DWORD WINAPI Notifier::ThreadProc(void* self) {
    auto* n = static_cast<Notifier*>(self);
    n->Run();
    HMODULE mod = n->module_;
    n->module_ = nullptr;
    InterlockedExchange(&n->busy_, 0);
    n->Release();
    FreeLibraryAndExitThread(mod, 0);
}

void Notifier::Run() {
    const ULONGLONG until = GetTickCount64() + maxMs_;
    DWORD wait = maxMs_ == 0 ? 0 : kPendingPollEveryMs;
    for (;;) {
        if (WaitForSingleObject(stop_, wait) != WAIT_TIMEOUT) return;
        const SessionInfo info = ServiceClient::Query(Scenario::Logon);
        // An unreachable service never replaces what the tiles show.
        if (info.reachable && StoreAnswer(Scenario::Logon, info)) Fire();
        if (info.reachable && !info.pending) return;
        if (GetTickCount64() >= until) return;
        wait = kPendingPollEveryMs;
    }
}

std::wstring SwitchLinkText(int seat) {
    // Dative suffix after the spoken number's last word (bir'e, iki'ye, altı'ya, dokuz'a, on'a, ...).
    static const wchar_t* units[10] = {L"", L"'e", L"'ye", L"'e", L"'e", L"'e", L"'ya", L"'ye", L"'e", L"'a"};
    static const wchar_t* tens[10] = {L"'a", L"'a", L"'ye", L"'a", L"'a", L"'ye", L"'a", L"'e", L"'e", L"'a"};
    const wchar_t* suffix = (seat % 10 != 0) ? units[seat % 10] : tens[(seat / 10) % 10];
    return L"Koltuk " + std::to_wstring(seat) + suffix + L" geç";
}

void ApplyFilter(bool hide, const CLSID& ours, const GUID* providers, BOOL* allow, DWORD count) {
    if (!hide || !providers || !allow) return;
    bool present = false;
    for (DWORD i = 0; i < count; ++i) {
        if (IsEqualGUID(providers[i], ours)) present = true;
    }
    // Never leave the screen without a provider.
    if (!present) return;
    for (DWORD i = 0; i < count; ++i) {
        allow[i] = IsEqualGUID(providers[i], ours) ? TRUE : FALSE;
    }
}

// ---- Credential ----------------------------------------------------------------------

Credential::Credential(Kind kind, CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, const SessionInfo& info, Notifier* notifier)
    : kind_(kind), cpus_(cpus), info_(info), notifier_(notifier) {
    DllAddRef();
    if (notifier_) notifier_->AddRef();
    if (kind_ == Kind::Seat) {
        title_ = L"Koltuk " + std::to_wstring(info_.seat);
        autoPending_ = info_.autoSubmit && cpus_ == CPUS_LOGON;
    } else {
        title_ = L"Koltuk değiştir";
        autoPending_ = false;
    }
}

Credential::~Credential() {
    typed_.Clear();
    if (events_) events_->Release();
    if (notifier_) notifier_->Release();
    DllRelease();
}

IFACEMETHODIMP Credential::QueryInterface(REFIID riid, void** ppv) {
    if (!ppv) return E_POINTER;
    *ppv = nullptr;
    if (riid == IID_IUnknown || riid == IID_ICredentialProviderCredential || riid == IID_ICredentialProviderCredential2) {
        *ppv = static_cast<ICredentialProviderCredential2*>(this);
        AddRef();
        return S_OK;
    }
    return E_NOINTERFACE;
}

IFACEMETHODIMP_(ULONG) Credential::AddRef() { return static_cast<ULONG>(InterlockedIncrement(&refs_)); }

IFACEMETHODIMP_(ULONG) Credential::Release() {
    LONG r = InterlockedDecrement(&refs_);
    if (r == 0) delete this;
    return static_cast<ULONG>(r);
}

IFACEMETHODIMP Credential::Advise(ICredentialProviderCredentialEvents* events) {
    if (events_) events_->Release();
    events_ = events;
    if (events_) events_->AddRef();
    return S_OK;
}

IFACEMETHODIMP Credential::UnAdvise() {
    if (events_) events_->Release();
    events_ = nullptr;
    return S_OK;
}

IFACEMETHODIMP Credential::SetSelected(BOOL* autoLogon) {
    if (!autoLogon) return E_POINTER;
    *autoLogon = autoPending_ ? TRUE : FALSE;
    return S_OK;
}

IFACEMETHODIMP Credential::SetDeselected() {
    typed_.Clear();
    if (events_) events_->SetFieldString(this, kFieldPassword, L"");
    return S_OK;
}

IFACEMETHODIMP Credential::GetFieldState(DWORD field, CREDENTIAL_PROVIDER_FIELD_STATE* state,
                                         CREDENTIAL_PROVIDER_FIELD_INTERACTIVE_STATE* interactive) {
    if (!state || !interactive) return E_POINTER;
    if (field >= kFieldCount) return E_INVALIDARG;
    *interactive = CPFIS_NONE;
    *state = CPFS_HIDDEN;
    if (field == kFieldTitle) {
        *state = CPFS_DISPLAY_IN_BOTH;
    } else if (field == kFieldAccount) {
        *state = CPFS_DISPLAY_IN_SELECTED_TILE;
    } else if (kind_ == Kind::Seat && (field == kFieldPassword || field == kFieldSubmit)) {
        *state = CPFS_DISPLAY_IN_SELECTED_TILE;
        if (field == kFieldPassword) *interactive = CPFIS_FOCUSED;
    } else if (kind_ == Kind::Switch && field >= kFieldLink0) {
        if (field - kFieldLink0 < info_.switchTargets.size()) *state = CPFS_DISPLAY_IN_SELECTED_TILE;
    }
    return S_OK;
}

IFACEMETHODIMP Credential::GetStringValue(DWORD field, LPWSTR* value) {
    if (!value) return E_POINTER;
    *value = nullptr;
    if (field >= kFieldCount) return E_INVALIDARG;
    if (field == kFieldTitle) return DupString(title_.c_str(), value);
    if (field == kFieldAccount) {
        if (kind_ == Kind::Seat) return DupString(info_.account.c_str(), value);
        return DupString(L"Ekranı başka bir koltuğa verin", value);
    }
    if (field >= kFieldLink0) {
        size_t i = field - kFieldLink0;
        if (i < info_.switchTargets.size()) return DupString(SwitchLinkText(info_.switchTargets[i]).c_str(), value);
        return DupString(L"", value);
    }
    // Password and submit: never echo what was typed.
    return DupString(L"", value);
}

IFACEMETHODIMP Credential::GetBitmapValue(DWORD, HBITMAP* bitmap) {
    if (bitmap) *bitmap = nullptr;
    return E_NOTIMPL;
}

IFACEMETHODIMP Credential::GetCheckboxValue(DWORD, BOOL*, LPWSTR*) { return E_NOTIMPL; }

IFACEMETHODIMP Credential::GetSubmitButtonValue(DWORD field, DWORD* adjacentTo) {
    if (!adjacentTo) return E_POINTER;
    if (field != kFieldSubmit) return E_INVALIDARG;
    *adjacentTo = kFieldPassword;
    return S_OK;
}

IFACEMETHODIMP Credential::GetComboBoxValueCount(DWORD, DWORD*, DWORD*) { return E_NOTIMPL; }
IFACEMETHODIMP Credential::GetComboBoxValueAt(DWORD, DWORD, LPWSTR*) { return E_NOTIMPL; }

IFACEMETHODIMP Credential::SetStringValue(DWORD field, LPCWSTR value) {
    if (field != kFieldPassword || kind_ != Kind::Seat) return E_INVALIDARG;
    if (!value) {
        typed_.Clear();
        return S_OK;
    }
    size_t n = wcsnlen(value, 1024);
    if (n >= 1024) return E_INVALIDARG;
    typed_.Assign(value, n);
    return S_OK;
}

IFACEMETHODIMP Credential::SetCheckboxValue(DWORD, BOOL) { return E_NOTIMPL; }
IFACEMETHODIMP Credential::SetComboBoxSelectedValue(DWORD, DWORD) { return E_NOTIMPL; }

void Credential::SetTitle(const std::wstring& text) {
    title_ = text;
    if (events_) events_->SetFieldString(this, kFieldTitle, title_.c_str());
}

IFACEMETHODIMP Credential::CommandLinkClicked(DWORD field) {
    if (kind_ != Kind::Switch || field < kFieldLink0 || field >= kFieldCount) return E_INVALIDARG;
    size_t i = field - kFieldLink0;
    if (i >= info_.switchTargets.size()) return E_INVALIDARG;
    const int seat = info_.switchTargets[i];
    if (ServiceClient::Switch(seat)) {
        SetTitle(L"Koltuk " + std::to_wstring(seat) + L" açılıyor…");
    } else {
        SetTitle(L"Geçilemedi: Pulsar Seat hizmeti isteği kabul etmedi");
    }
    return S_OK;
}

IFACEMETHODIMP Credential::GetSerialization(CREDENTIAL_PROVIDER_GET_SERIALIZATION_RESPONSE* response,
                                            CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION* serialization,
                                            LPWSTR* statusText, CREDENTIAL_PROVIDER_STATUS_ICON* statusIcon) {
    if (!response || !serialization) return E_POINTER;
    *response = CPGSR_NO_CREDENTIAL_NOT_FINISHED;
    if (statusText) *statusText = nullptr;
    if (statusIcon) *statusIcon = CPSI_NONE;
    ZeroMemory(serialization, sizeof(*serialization));
    if (kind_ != Kind::Seat) return S_OK;

    SeatCredential cred;
    bool useAuto = false;
    bool autoFailed = false;
    if (autoPending_) {
        // One attempt only: whatever happens, the stored credential is not asked for again
        // by this tile.
        autoPending_ = false;
        useAuto = ServiceClient::TakeCredentials(cred);
        autoFailed = !useAuto;
    }
    std::wstring domain;
    std::wstring user;
    const SecretW* pw = nullptr;
    if (useAuto) {
        domain = cred.domain;
        user = cred.user;
        pw = &cred.password;
    } else {
        if (typed_.Empty()) {
            if (statusText) {
                DupString(autoFailed ? L"Kayıtlı parola alınamadı. Parolayı girin." : L"Parolayı girin.", statusText);
            }
            if (statusIcon) *statusIcon = CPSI_WARNING;
            return S_OK;
        }
        SplitAccount(info_.account, domain, user);
        pw = &typed_;
    }
    if (user.empty()) return S_OK;

    ULONG package = 0;
    HRESULT hr = NegotiatePackage(&package);
    if (FAILED(hr)) return hr;
    BYTE* blob = nullptr;
    ULONG size = 0;
    hr = PackLogon(domain, user, *pw, cpus_, &blob, &size);
    typed_.Clear();
    cred.password.Clear();
    if (FAILED(hr)) return hr;
    serialization->ulAuthenticationPackage = package;
    serialization->clsidCredentialProvider = kProviderClsid;
    serialization->cbSerialization = size;
    serialization->rgbSerialization = blob;
    *response = CPGSR_RETURN_CREDENTIAL_FINISHED;
    submittedAuto_ = useAuto;
    return S_OK;
}

IFACEMETHODIMP Credential::ReportResult(NTSTATUS status, NTSTATUS subStatus, LPWSTR* statusText,
                                        CREDENTIAL_PROVIDER_STATUS_ICON* statusIcon) {
    if (statusText) *statusText = nullptr;
    if (statusIcon) *statusIcon = CPSI_NONE;
    if (submittedAuto_) {
        submittedAuto_ = false;
        ServiceClient::Report(status, subStatus);
        if (status != 0) {
            if (statusText) DupString(L"Kayıtlı parola kabul edilmedi. Parolayı girin.", statusText);
            if (statusIcon) *statusIcon = CPSI_ERROR;
            // The service dropped the grant: refresh the cached answer (no hiding any
            // more) and tell LogonUI. Whether LogonUI then re-runs the filter and shows the
            // other tiles again is unverified (see provider.h); the seat tile stays.
            if (notifier_) notifier_->Refresh(0);
        }
    }
    if (status != 0) {
        autoPending_ = false;
        typed_.Clear();
        if (events_) events_->SetFieldString(this, kFieldPassword, L"");
    }
    return S_OK;
}

IFACEMETHODIMP Credential::GetUserSid(LPWSTR* sid) {
    if (!sid) return E_POINTER;
    *sid = nullptr;
    if (kind_ == Kind::Seat && !info_.userSid.empty()) return DupString(info_.userSid.c_str(), sid);
    // Not tied to a user tile ("other user").
    return S_FALSE;
}

// ---- Provider ------------------------------------------------------------------------

Provider::Provider() : notifier_(Notifier::Create()) { DllAddRef(); }

Provider::~Provider() {
    ReleaseTiles();
    if (notifier_) {
        notifier_->UnAdvise();
        notifier_->Release();
    }
    DllRelease();
}

void Provider::ReleaseTiles() {
    for (Credential* c : tiles_) c->Release();
    tiles_.clear();
}

IFACEMETHODIMP Provider::QueryInterface(REFIID riid, void** ppv) {
    if (!ppv) return E_POINTER;
    *ppv = nullptr;
    if (riid == IID_IUnknown || riid == IID_ICredentialProvider) {
        *ppv = static_cast<ICredentialProvider*>(this);
    } else if (riid == IID_ICredentialProviderSetUserArray) {
        *ppv = static_cast<ICredentialProviderSetUserArray*>(this);
    } else {
        return E_NOINTERFACE;
    }
    AddRef();
    return S_OK;
}

IFACEMETHODIMP_(ULONG) Provider::AddRef() { return static_cast<ULONG>(InterlockedIncrement(&refs_)); }

IFACEMETHODIMP_(ULONG) Provider::Release() {
    LONG r = InterlockedDecrement(&refs_);
    if (r == 0) delete this;
    return static_cast<ULONG>(r);
}

IFACEMETHODIMP Provider::SetUsageScenario(CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, DWORD) {
    ReleaseTiles();
    autoDefault_ = false;
    built_ = false;
    if (!InLogonUI() || !IsWanted(cpus)) return E_NOTIMPL;
    cpus_ = cpus;
    info_ = CachedQuery(ScenarioOf(), false);
    if (!info_.reachable) return E_NOTIMPL;
    // A hand-over is under way: take part with no tiles yet, so LogonUI advises us and the
    // poll can bring the seat tile in with CredentialsChanged.
    if (cpus == CPUS_LOGON && info_.pending) return S_OK;
    if (info_.seat == 0) return E_NOTIMPL;
    if (!SeatTileWillExist(info_, cpus) && info_.switchTargets.empty()) return E_NOTIMPL;
    // Tiles are built at GetCredentialCount (after SetUserArray, which may come later).
    return S_OK;
}

void Provider::Rebuild() {
    ULONGLONG gen = 0;
    SessionInfo now = CachedQuery(ScenarioOf(), false, &gen);
    if (built_ && gen == builtGen_) return;
    builtGen_ = gen;
    // The service went away meanwhile: keep what is shown.
    if (built_ && !now.reachable) return;
    if (now.switchTargets.size() > kMaxLinks) now.switchTargets.resize(kMaxLinks);
    if (!now.userSid.empty()) {
        bool listed = false;
        for (const auto& s : userSids_) {
            if (CompareStringOrdinal(s.c_str(), -1, now.userSid.c_str(), -1, TRUE) == CSTR_EQUAL) listed = true;
        }
        if (!usersKnown_ || !listed) now.userSid.clear();
    }
    // Same tiles (only the filter flags changed, or the grant went away): keep them, so a
    // status text LogonUI shows on the seat tile stays. A grant that appears late needs
    // new tiles (default + auto-logon).
    const bool sameTiles = built_ && now.seat == info_.seat && now.account == info_.account &&
                           now.userSid == info_.userSid && now.switchTargets == info_.switchTargets &&
                           !(now.autoSubmit && !info_.autoSubmit);
    info_ = now;
    if (sameTiles) {
        autoDefault_ = autoDefault_ && info_.autoSubmit;
        return;
    }
    ReleaseTiles();
    autoDefault_ = false;
    built_ = true;
    try {
        if (SeatTileWillExist(info_, cpus_)) {
            tiles_.push_back(new Credential(Credential::Kind::Seat, cpus_, info_, notifier_));
            autoDefault_ = info_.autoSubmit;
        }
        if (info_.reachable && info_.seat != 0 && !info_.switchTargets.empty()) {
            tiles_.push_back(new Credential(Credential::Kind::Switch, cpus_, info_, notifier_));
        }
    } catch (...) {
        ReleaseTiles();
        autoDefault_ = false;
    }
}

IFACEMETHODIMP Provider::SetSerialization(const CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION*) { return E_NOTIMPL; }

IFACEMETHODIMP Provider::Advise(ICredentialProviderEvents* events, UINT_PTR context) {
    if (!notifier_) return S_OK;
    notifier_->Advise(events, context);
    // LogonUI enumerated before the service armed the grant: poll (bounded).
    if (cpus_ == CPUS_LOGON && info_.pending) notifier_->Refresh(kPendingPollMaxMs);
    return S_OK;
}

IFACEMETHODIMP Provider::UnAdvise() {
    if (notifier_) notifier_->UnAdvise();
    return S_OK;
}

IFACEMETHODIMP Provider::GetFieldDescriptorCount(DWORD* count) {
    if (!count) return E_POINTER;
    *count = kFieldCount;
    return S_OK;
}

IFACEMETHODIMP Provider::GetFieldDescriptorAt(DWORD index, CREDENTIAL_PROVIDER_FIELD_DESCRIPTOR** descriptor) {
    if (!descriptor) return E_POINTER;
    *descriptor = nullptr;
    if (index >= kFieldCount) return E_INVALIDARG;
    auto* d = static_cast<CREDENTIAL_PROVIDER_FIELD_DESCRIPTOR*>(CoTaskMemAlloc(sizeof(CREDENTIAL_PROVIDER_FIELD_DESCRIPTOR)));
    if (!d) return E_OUTOFMEMORY;
    ZeroMemory(d, sizeof(*d));
    d->dwFieldID = index;
    d->cpft = TypeOf(index);
    HRESULT hr = DupString(LabelOf(index), &d->pszLabel);
    if (FAILED(hr)) {
        CoTaskMemFree(d);
        return hr;
    }
    *descriptor = d;
    return S_OK;
}

IFACEMETHODIMP Provider::GetCredentialCount(DWORD* count, DWORD* defaultIndex, BOOL* autoLogonWithDefault) {
    if (!count || !defaultIndex || !autoLogonWithDefault) return E_POINTER;
    if (cpus_ != CPUS_INVALID) Rebuild();
    *count = static_cast<DWORD>(tiles_.size());
    *defaultIndex = autoDefault_ ? 0 : CREDENTIAL_PROVIDER_NO_DEFAULT;
    *autoLogonWithDefault = autoDefault_ ? TRUE : FALSE;
    return S_OK;
}

IFACEMETHODIMP Provider::GetCredentialAt(DWORD index, ICredentialProviderCredential** credential) {
    if (!credential) return E_POINTER;
    *credential = nullptr;
    if (index >= tiles_.size()) return E_INVALIDARG;
    return tiles_[index]->QueryInterface(IID_ICredentialProviderCredential, reinterpret_cast<void**>(credential));
}

IFACEMETHODIMP Provider::SetUserArray(ICredentialProviderUserArray* users) {
    userSids_.clear();
    usersKnown_ = false;
    built_ = false;  // the seat tile's user SID depends on this list
    if (!users) return S_OK;
    DWORD n = 0;
    if (FAILED(users->GetCount(&n))) return S_OK;
    try {
        for (DWORD i = 0; i < n && i < 1024; ++i) {
            ICredentialProviderUser* u = nullptr;
            if (FAILED(users->GetAt(i, &u)) || !u) continue;
            LPWSTR sid = nullptr;
            if (SUCCEEDED(u->GetSid(&sid)) && sid) {
                userSids_.emplace_back(sid);
                CoTaskMemFree(sid);
            }
            u->Release();
        }
        usersKnown_ = true;
    } catch (...) {
        userSids_.clear();
    }
    return S_OK;
}

// ---- Filter --------------------------------------------------------------------------

ProviderFilter::ProviderFilter() { DllAddRef(); }
ProviderFilter::~ProviderFilter() { DllRelease(); }

IFACEMETHODIMP ProviderFilter::QueryInterface(REFIID riid, void** ppv) {
    if (!ppv) return E_POINTER;
    *ppv = nullptr;
    if (riid == IID_IUnknown || riid == IID_ICredentialProviderFilter) {
        *ppv = static_cast<ICredentialProviderFilter*>(this);
        AddRef();
        return S_OK;
    }
    return E_NOINTERFACE;
}

IFACEMETHODIMP_(ULONG) ProviderFilter::AddRef() { return static_cast<ULONG>(InterlockedIncrement(&refs_)); }

IFACEMETHODIMP_(ULONG) ProviderFilter::Release() {
    LONG r = InterlockedDecrement(&refs_);
    if (r == 0) delete this;
    return static_cast<ULONG>(r);
}

IFACEMETHODIMP ProviderFilter::Filter(CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, DWORD, GUID* providers, BOOL* allow,
                                      DWORD count) {
    // Logon scenario only; anything else (unlock, CredUI, change password) is left alone.
    if (cpus != CPUS_LOGON || !InLogonUI()) return S_OK;
    // A recent answer (the provider usually asked just before) is reused: a hung service
    // must not stall every LogonUI callback with a fresh round trip.
    const SessionInfo info = CachedQueryMaxAge(Scenario::Logon, kFilterMaxAgeMs);
    // Same predicate as the provider's seat tile: never hide when it will not show one.
    ApplyFilter(FilterHides(info), kProviderClsid, providers, allow, count);
    return S_OK;
}

IFACEMETHODIMP ProviderFilter::UpdateRemoteCredential(const CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION*,
                                                      CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION*) {
    return E_NOTIMPL;
}

}  // namespace pulsar
