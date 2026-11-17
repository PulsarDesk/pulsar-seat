// SPDX-License-Identifier: GPL-3.0-only
// The Pulsar credential provider, its two tile kinds and the provider filter.
//
// Behaviour (spec 35 §5.4 / §5.5, docs/agents/architecture.md "Credential provider"):
// - Only inside LogonUI, only for CPUS_LOGON and CPUS_UNLOCK_WORKSTATION; E_NOTIMPL for
//   every other scenario (CredUI and change-password never see us).
// - Seat tile (logon screen of a Pulsar seat session with an account): when the service
//   armed a one-time credential, it is the default tile with auto-logon; the credential is
//   fetched at GetSerialization, once. If the service refuses, or the logon fails, the tile
//   stays as an ordinary password tile for the seat's account (no retry of the stored one).
// - Switch tile (logon and lock screen of a seat session): one command link per other seat.
// - Filter: hides every other provider only when the service says so for this session
//   (armed one-time logon, logon scenario), only when the answer gives a seat tile (account
//   known: SeatTileWillExist) and only if our provider is in the list; when the service
//   does not answer it hides nothing.
// - Late grant: LogonUI enumerates once, often before the service armed the grant. While
//   the service answers `pending` the provider polls (Notifier, bounded) and rebuilds its
//   tiles after CredentialsChanged. After a failed auto-logon it asks once more, so the
//   cached answer stops hiding; LogonUI re-runs the filter only on a new enumeration
//   (HYPOTHESIS, untested on hardware), so the other tiles may stay hidden until then.
#pragma once

#include "copy.h"
#include "common.h"
#include "pipe.h"

namespace pulsar {

// Tile image (seat number on the accent colour), drawn at run time; caller owns it.
HBITMAP TileImage(int seat);

// Field ids (one descriptor set for both tile kinds; each tile hides what it does not use).
enum Field : DWORD {
    kFieldTitle = 0,
    kFieldAccount,
    kFieldPassword,
    kFieldSubmit,
    kFieldImage,
    kFieldLink0,
    kMaxLinks = 15,
    kFieldCount = kFieldLink0 + kMaxLinks,
};

// Cached answer of the service for this LogonUI process (filter and provider share it, so
// the filter never hides tiles the provider then does not replace). `generation` (optional)
// = the cache entry's generation, read together with the answer; it grows whenever a
// stored answer differs from the one before (SameSession).
SessionInfo CachedQuery(Scenario scenario, bool refresh, ULONGLONG* generation = nullptr);
// Cached answer when younger than `maxAgeMs` (0 = always ask the service).
SessionInfo CachedQueryMaxAge(Scenario scenario, ULONGLONG maxAgeMs, ULONGLONG* generation = nullptr);
// Store an answer; true when it differs from the cached one (generation bumped).
bool StoreAnswer(Scenario scenario, const SessionInfo& info);

// One predicate for provider and filter: this answer gives a seat tile in `cpus`.
bool SeatTileWillExist(const SessionInfo& info, CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus);
// The filter hides the other providers for this answer (logon scenario only).
bool FilterHides(const SessionInfo& info);

// Pending answer polling: started when the service says `pending`, at most this long.
inline constexpr DWORD kPendingPollMaxMs = 30'000;
inline constexpr DWORD kPendingPollEveryMs = 250;

// Holds the provider's ICredentialProviderEvents and asks the service again on a worker
// thread; a changed answer goes into the cache and LogonUI is told with
// CredentialsChanged (it then calls GetCredentialCount, where the provider rebuilds its
// tiles on LogonUI's thread). Shared by the provider and its tiles (ref-counted).
// **Untested on hardware**: CredentialsChanged from a worker thread.
class Notifier {
public:
    static Notifier* Create();  // nullptr when out of resources (then no refresh)
    void AddRef();
    void Release();
    void Advise(ICredentialProviderEvents* events, UINT_PTR context);
    void UnAdvise();
    // Query again: every kPendingPollEveryMs while the answer is `pending`, at most
    // `maxMs` (0 = one query now). One worker at a time; a call while one runs is ignored.
    void Refresh(DWORD maxMs);

private:
    Notifier() = default;
    ~Notifier();
    static DWORD WINAPI ThreadProc(void* self);
    void Run();
    void Fire();

    LONG refs_ = 1;
    SRWLOCK lock_ = SRWLOCK_INIT;
    ICredentialProviderEvents* events_ = nullptr;
    UINT_PTR context_ = 0;
    HANDLE stop_ = nullptr;  // manual reset: set by UnAdvise / destruction
    LONG busy_ = 0;
    DWORD maxMs_ = 0;
    HMODULE module_ = nullptr;  // pinned while the worker runs
};

class Credential final : public ICredentialProviderCredential2 {
public:
    enum class Kind { Seat, Switch };
    Credential(Kind kind, CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, const SessionInfo& info, Notifier* notifier);
    bool IsSeat() const { return kind_ == Kind::Seat; }

    // IUnknown
    IFACEMETHODIMP QueryInterface(REFIID riid, void** ppv) override;
    IFACEMETHODIMP_(ULONG) AddRef() override;
    IFACEMETHODIMP_(ULONG) Release() override;

    // ICredentialProviderCredential
    IFACEMETHODIMP Advise(ICredentialProviderCredentialEvents* events) override;
    IFACEMETHODIMP UnAdvise() override;
    IFACEMETHODIMP SetSelected(BOOL* autoLogon) override;
    IFACEMETHODIMP SetDeselected() override;
    IFACEMETHODIMP GetFieldState(DWORD field, CREDENTIAL_PROVIDER_FIELD_STATE* state,
                                 CREDENTIAL_PROVIDER_FIELD_INTERACTIVE_STATE* interactive) override;
    IFACEMETHODIMP GetStringValue(DWORD field, LPWSTR* value) override;
    IFACEMETHODIMP GetBitmapValue(DWORD field, HBITMAP* bitmap) override;
    IFACEMETHODIMP GetCheckboxValue(DWORD field, BOOL* checked, LPWSTR* label) override;
    IFACEMETHODIMP GetSubmitButtonValue(DWORD field, DWORD* adjacentTo) override;
    IFACEMETHODIMP GetComboBoxValueCount(DWORD field, DWORD* count, DWORD* selected) override;
    IFACEMETHODIMP GetComboBoxValueAt(DWORD field, DWORD item, LPWSTR* value) override;
    IFACEMETHODIMP SetStringValue(DWORD field, LPCWSTR value) override;
    IFACEMETHODIMP SetCheckboxValue(DWORD field, BOOL checked) override;
    IFACEMETHODIMP SetComboBoxSelectedValue(DWORD field, DWORD selected) override;
    IFACEMETHODIMP CommandLinkClicked(DWORD field) override;
    IFACEMETHODIMP GetSerialization(CREDENTIAL_PROVIDER_GET_SERIALIZATION_RESPONSE* response,
                                    CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION* serialization,
                                    LPWSTR* statusText, CREDENTIAL_PROVIDER_STATUS_ICON* statusIcon) override;
    IFACEMETHODIMP ReportResult(NTSTATUS status, NTSTATUS subStatus, LPWSTR* statusText,
                                CREDENTIAL_PROVIDER_STATUS_ICON* statusIcon) override;

    // ICredentialProviderCredential2
    IFACEMETHODIMP GetUserSid(LPWSTR* sid) override;

private:
    ~Credential();
    void SetTitle(const std::wstring& text);

    LONG refs_ = 1;
    Kind kind_;
    CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus_;
    SessionInfo info_;
    std::wstring title_;
    SecretW typed_;
    bool autoPending_;      // the armed credential has not been fetched yet
    bool submittedAuto_ = false;  // the last serialization used the service's credential
    ICredentialProviderCredentialEvents* events_ = nullptr;
    Notifier* notifier_ = nullptr;
};

class Provider final : public ICredentialProvider, public ICredentialProviderSetUserArray {
public:
    Provider();

    IFACEMETHODIMP QueryInterface(REFIID riid, void** ppv) override;
    IFACEMETHODIMP_(ULONG) AddRef() override;
    IFACEMETHODIMP_(ULONG) Release() override;

    // ICredentialProvider
    IFACEMETHODIMP SetUsageScenario(CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, DWORD flags) override;
    IFACEMETHODIMP SetSerialization(const CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION* serialization) override;
    IFACEMETHODIMP Advise(ICredentialProviderEvents* events, UINT_PTR context) override;
    IFACEMETHODIMP UnAdvise() override;
    IFACEMETHODIMP GetFieldDescriptorCount(DWORD* count) override;
    IFACEMETHODIMP GetFieldDescriptorAt(DWORD index, CREDENTIAL_PROVIDER_FIELD_DESCRIPTOR** descriptor) override;
    IFACEMETHODIMP GetCredentialCount(DWORD* count, DWORD* defaultIndex, BOOL* autoLogonWithDefault) override;
    IFACEMETHODIMP GetCredentialAt(DWORD index, ICredentialProviderCredential** credential) override;

    // ICredentialProviderSetUserArray
    IFACEMETHODIMP SetUserArray(ICredentialProviderUserArray* users) override;

private:
    ~Provider();
    void ReleaseTiles();
    // Bring the tiles in line with the cached answer (LogonUI's thread only).
    void Rebuild();
    Scenario ScenarioOf() const { return cpus_ == CPUS_UNLOCK_WORKSTATION ? Scenario::Unlock : Scenario::Logon; }

    LONG refs_ = 1;
    CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus_ = CPUS_INVALID;
    SessionInfo info_;
    std::vector<Credential*> tiles_;
    bool autoDefault_ = false;
    bool built_ = false;
    ULONGLONG builtGen_ = 0;
    // SIDs of LogonUI's user array (SetUserArray); the seat tile names its user only when
    // the SID is listed, so a tile is never tied to a user LogonUI does not show.
    bool usersKnown_ = false;
    std::vector<std::wstring> userSids_;
    Notifier* notifier_ = nullptr;
};

class ProviderFilter final : public ICredentialProviderFilter {
public:
    ProviderFilter();

    IFACEMETHODIMP QueryInterface(REFIID riid, void** ppv) override;
    IFACEMETHODIMP_(ULONG) AddRef() override;
    IFACEMETHODIMP_(ULONG) Release() override;

    IFACEMETHODIMP Filter(CREDENTIAL_PROVIDER_USAGE_SCENARIO cpus, DWORD flags, GUID* providers, BOOL* allow,
                          DWORD count) override;
    IFACEMETHODIMP UpdateRemoteCredential(const CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION* in,
                                          CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION* out) override;

private:
    ~ProviderFilter();
    LONG refs_ = 1;
};

// Pure decision of the filter (unit-testable): which providers stay visible.
// Hides everything but `ours` only when `hide` is set and `ours` is in the list.
void ApplyFilter(bool hide, const CLSID& ours, const GUID* providers, BOOL* allow, DWORD count);

// Switch link text in the UI language: "Switch to seat N" / "Koltuk N'ye geç" (Turkish
// dative suffix of the number).
std::wstring SwitchLinkText(int seat);
std::wstring SwitchLinkTextIn(int seat, copy::Lang lang);

}  // namespace pulsar
