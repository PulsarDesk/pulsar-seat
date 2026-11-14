// SPDX-License-Identifier: GPL-3.0-only
// Self-test of the pure parts of the credential provider (no LogonUI, no service, no
// registry): JSON reader on the seat-proto shapes, the logon packing layout, the filter
// decision, the Turkish link text, and that provider + filter stay inert outside LogonUI.
// Run: build\<Platform>\<Configuration>\credprov-selftest.exe (exit code 0 = all passed).
#include "../src/json.h"
#include "../src/pack.h"
#include "../src/provider.h"

#include <cstdio>

namespace {

int g_failed = 0;

void Check(bool ok, const char* what) {
    std::printf("%s %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) ++g_failed;
}

void JsonShapes() {
    using pulsar::json::Value;
    Value v;
    // Response frame of credprov_query (same text as seat-proto test credprov_shapes).
    const std::string q =
        R"({"version":1,"id":2,"result":{"Ok":{"type":"credprov_session","data":{"seat":2,"auto_submit":true,"hide_others":true,"account":"PC\\ali","switch_targets":[1]}}}})";
    Check(pulsar::json::Parse(q, v), "parse credprov_session");
    const Value* d = v.Get("result") ? v.Get("result")->Get("Ok") : nullptr;
    d = d ? d->Get("data") : nullptr;
    Check(d && d->Get("seat") && d->Get("seat")->IsInt() && d->Get("seat")->i == 2, "seat 2");
    Check(d && d->Get("account") && d->Get("account")->s == "PC\\ali", "account unescaped");
    Check(d && d->Get("switch_targets") && d->Get("switch_targets")->items.size() == 1, "switch targets");
    Value e;
    Check(pulsar::json::Parse(R"({"s":"ç😀","n":-1073741715})", e), "unicode escapes");
    Check(e.Get("s") && e.Get("s")->s == "\xc3\xa7\xf0\x9f\x98\x80", "utf-8 out");
    Check(e.Get("n") && e.Get("n")->IsInt() && e.Get("n")->i == -1073741715, "negative int");
    Value bad;
    Check(!pulsar::json::Parse(R"({"a":1,})", bad), "trailing comma refused");
    Check(!pulsar::json::Parse(R"({"a":"\ud800"})", bad), "lone surrogate refused");
    Check(!pulsar::json::Parse(R"({"a":1} x)", bad), "trailing data refused");
    std::string deep(40, '[');
    deep += std::string(40, ']');
    Check(!pulsar::json::Parse(deep, bad), "depth limit");
    std::string out;
    pulsar::json::AppendString(out, "a\"b\\c\n");
    Check(out == R"("a\"b\\c\n")", "string escaping");
    e.Wipe();
    Check(e.Get("s") && e.Get("s")->s.empty(), "wipe clears strings");
    // Short strings (a typical password) are decoded on the heap with room reserved up
    // front, so moving them never copies the bytes into an unwiped small-string buffer.
    Value pw;
    Check(pulsar::json::Parse(R"({"type":"credentials","data":{"password":"kisa"}})", pw), "parse credentials");
    const Value* p = pw.Get("data") ? pw.Get("data")->Get("password") : nullptr;
    Check(p && p->s == "kisa" && p->s.capacity() >= 31, "short string kept on the heap");
    Value moved = std::move(*pw.GetMut("data"));
    Check(moved.Get("password") && moved.Get("password")->s == "kisa", "subtree moved out");
    moved.Wipe();
    pw.Wipe();
    Check(moved.Get("password")->s.empty(), "moved subtree wiped");
    // The hand-over flag of credprov_session (seat-proto test credprov_shapes).
    Value pend;
    Check(pulsar::json::Parse(R"({"type":"credprov_session","data":{"seat":null,"auto_submit":false,"hide_others":false,"pending":true}})", pend) &&
              pend.Get("data") && pend.Get("data")->Get("pending") && pend.Get("data")->Get("pending")->IsTrue(),
          "pending flag");
    // A failed parse leaves nothing readable behind in the tree.
    Value half;
    Check(!pulsar::json::Parse(R"({"data":{"password":"geheim)", half), "unterminated string refused");
}

void SessionDecisions() {
    pulsar::SessionInfo armed;
    armed.reachable = true;
    armed.seat = 2;
    armed.autoSubmit = true;
    armed.hideOthers = true;
    armed.account = L".\\ali";
    Check(pulsar::SeatTileWillExist(armed, CPUS_LOGON), "seat tile with an account");
    Check(!pulsar::SeatTileWillExist(armed, CPUS_UNLOCK_WORKSTATION), "no seat tile on the lock screen");
    Check(pulsar::FilterHides(armed), "armed + account: filter hides");
    pulsar::SessionInfo noAccount = armed;
    noAccount.account.clear();
    Check(!pulsar::SeatTileWillExist(noAccount, CPUS_LOGON), "no account: no seat tile");
    Check(!pulsar::FilterHides(noAccount), "no account: filter hides nothing");
    pulsar::SessionInfo gone = armed;
    gone.reachable = false;
    Check(!pulsar::FilterHides(gone), "service gone: filter hides nothing");
    pulsar::SessionInfo late = armed;
    late.pending = true;
    Check(pulsar::SameSession(armed, late), "pending alone is not a new answer");
    late.autoSubmit = false;
    Check(!pulsar::SameSession(armed, late), "grant change is a new answer");
}

void Packing() {
    pulsar::SecretW pw;
    pw.Assign(L"pw", 2);
    BYTE* blob = nullptr;
    ULONG size = 0;
    // Unlock scenario: no CredProtect, exact layout check.
    HRESULT hr = pulsar::PackLogon(L"DOM", L"ali", pw, CPUS_UNLOCK_WORKSTATION, &blob, &size);
    Check(SUCCEEDED(hr) && blob, "pack unlock");
    if (SUCCEEDED(hr) && blob) {
        auto* k = reinterpret_cast<KERB_INTERACTIVE_UNLOCK_LOGON*>(blob);
        const size_t head = sizeof(KERB_INTERACTIVE_UNLOCK_LOGON);
        Check(size == head + (3 + 3 + 2) * sizeof(wchar_t), "blob size");
        Check(k->Logon.MessageType == KerbWorkstationUnlockLogon, "message type unlock");
        const ULONG_PTR dOff = reinterpret_cast<ULONG_PTR>(k->Logon.LogonDomainName.Buffer);
        const ULONG_PTR uOff = reinterpret_cast<ULONG_PTR>(k->Logon.UserName.Buffer);
        const ULONG_PTR pOff = reinterpret_cast<ULONG_PTR>(k->Logon.Password.Buffer);
        Check(dOff == head && uOff == head + 6 && pOff == head + 12, "offsets relative to the blob");
        Check(k->Logon.UserName.Length == 6 && memcmp(blob + uOff, L"ali", 6) == 0, "user name bytes");
        Check(k->Logon.Password.Length == 4 && memcmp(blob + pOff, L"pw", 4) == 0, "password bytes");
        SecureZeroMemory(blob, size);
        CoTaskMemFree(blob);
    }
    // Logon scenario: local account (".") becomes the computer name, password protected.
    blob = nullptr;
    hr = pulsar::PackLogon(L".", L"ali", pw, CPUS_LOGON, &blob, &size);
    Check(SUCCEEDED(hr) && blob, "pack logon");
    if (SUCCEEDED(hr) && blob) {
        auto* k = reinterpret_cast<KERB_INTERACTIVE_UNLOCK_LOGON*>(blob);
        Check(k->Logon.MessageType == KerbInteractiveLogon, "message type logon");
        Check(k->Logon.LogonDomainName.Length > 0 &&
                  memcmp(blob + reinterpret_cast<ULONG_PTR>(k->Logon.LogonDomainName.Buffer), L".", 2) != 0,
              "computer name for a local account");
        Check(k->Logon.Password.Length > 4, "password protected (longer than plain)");
        SecureZeroMemory(blob, size);
        CoTaskMemFree(blob);
    }
    Check(FAILED(pulsar::PackLogon(L"D", L"", pw, CPUS_LOGON, &blob, &size)), "empty user refused");
    Check(FAILED(pulsar::PackLogon(L"D", L"u", pw, CPUS_CREDUI, &blob, &size)), "CredUI refused");
    std::wstring d, u;
    pulsar::SplitAccount(L".\\ali", d, u);
    Check(d == L"." && u == L"ali", "split account");
    pulsar::SplitAccount(L"ali", d, u);
    Check(d.empty() && u == L"ali", "split bare user");
}

void FilterDecision() {
    const GUID other = {0x60b78e88, 0xead8, 0x445c, {0x9c, 0xfd, 0x0b, 0x87, 0xf7, 0x4e, 0xa6, 0xcd}};
    GUID list[3] = {other, pulsar::kProviderClsid, other};
    BOOL allow[3] = {TRUE, TRUE, TRUE};
    pulsar::ApplyFilter(false, pulsar::kProviderClsid, list, allow, 3);
    Check(allow[0] && allow[1] && allow[2], "no hide: everything stays");
    pulsar::ApplyFilter(true, pulsar::kProviderClsid, list, allow, 3);
    Check(!allow[0] && allow[1] && !allow[2], "hide: only ours stays");
    GUID foreign[2] = {other, other};
    BOOL allow2[2] = {TRUE, TRUE};
    pulsar::ApplyFilter(true, pulsar::kProviderClsid, foreign, allow2, 2);
    Check(allow2[0] && allow2[1], "ours missing: nothing hidden");
}

void LinkText() {
    Check(pulsar::SwitchLinkText(2) == L"Koltuk 2'ye geç", "2'ye");
    Check(pulsar::SwitchLinkText(3) == L"Koltuk 3'e geç", "3'e");
    Check(pulsar::SwitchLinkText(6) == L"Koltuk 6'ya geç", "6'ya");
    Check(pulsar::SwitchLinkText(9) == L"Koltuk 9'a geç", "9'a");
    Check(pulsar::SwitchLinkText(10) == L"Koltuk 10'a geç", "10'a");
    Check(pulsar::SwitchLinkText(16) == L"Koltuk 16'ya geç", "16'ya");
    Check(pulsar::SwitchLinkText(1) == L"Koltuk 1'e geç", "1'e");
    Check(pulsar::SwitchLinkText(11) == L"Koltuk 11'e geç", "11'e");
    Check(pulsar::SwitchLinkText(21) == L"Koltuk 21'e geç", "21'e");
    Check(pulsar::SwitchLinkText(20) == L"Koltuk 20'ye geç", "20'ye");
    Check(pulsar::SwitchLinkText(40) == L"Koltuk 40'a geç", "40'a");
    Check(pulsar::SwitchLinkText(50) == L"Koltuk 50'ye geç", "50'ye");
    Check(pulsar::SwitchLinkText(61) == L"Koltuk 61'e geç", "61'e");
    Check(pulsar::SwitchLinkText(64) == L"Koltuk 64'e geç", "64'e");
}

// VM test 2026-11-15: the switch links must be reachable from the seat tile (Windows 11
// lists only tiles of the locked / selected user), and the switch tile belongs to the seat
// user's tile when the SID is known.
void TileLinks() {
    pulsar::SessionInfo info;
    info.reachable = true;
    info.seat = 2;
    info.account = L"PC\\seat1";
    info.userSid = L"S-1-5-21-1-2-3-1001";
    info.switchTargets = {1, 3};
    using K = pulsar::Credential::Kind;
    for (K kind : {K::Seat, K::Switch}) {
        auto* c = new pulsar::Credential(kind, CPUS_LOGON, info, nullptr);
        CREDENTIAL_PROVIDER_FIELD_STATE st = CPFS_HIDDEN;
        CREDENTIAL_PROVIDER_FIELD_INTERACTIVE_STATE is = CPFIS_NONE;
        c->GetFieldState(pulsar::kFieldLink0, &st, &is);
        Check(st == CPFS_DISPLAY_IN_SELECTED_TILE, kind == K::Seat ? "seat tile shows link 1" : "switch tile shows link 1");
        c->GetFieldState(pulsar::kFieldLink0 + 2, &st, &is);
        Check(st == CPFS_HIDDEN, "unused link hidden");
        LPWSTR sid = nullptr;
        Check(c->GetUserSid(&sid) == S_OK && sid && std::wstring(sid) == info.userSid, "tile tied to the seat user");
        CoTaskMemFree(sid);
        c->Release();
    }
    info.userSid.clear();
    auto* c = new pulsar::Credential(K::Switch, CPUS_UNLOCK_WORKSTATION, info, nullptr);
    LPWSTR sid = nullptr;
    Check(c->GetUserSid(&sid) == S_FALSE && !sid, "no SID: other-user tile");
    c->Release();
}

void InertOutsideLogonUI() {
    Check(!pulsar::InLogonUI(), "this process is not LogonUI");
    ICredentialProvider* p = nullptr;
    IClassFactory* f = nullptr;
    HRESULT hr = DllGetClassObject(pulsar::kProviderClsid, IID_IClassFactory, reinterpret_cast<void**>(&f));
    Check(SUCCEEDED(hr) && f, "class factory");
    if (f) {
        hr = f->CreateInstance(nullptr, IID_ICredentialProvider, reinterpret_cast<void**>(&p));
        Check(SUCCEEDED(hr) && p, "provider instance");
        if (p) {
            Check(p->SetUsageScenario(CPUS_LOGON, 0) == E_NOTIMPL, "provider inert outside LogonUI");
            p->Release();
        }
        f->Release();
    }
    ICredentialProviderFilter* fl = nullptr;
    f = nullptr;
    hr = DllGetClassObject(pulsar::kFilterClsid, IID_IClassFactory, reinterpret_cast<void**>(&f));
    if (f) {
        f->CreateInstance(nullptr, IID_ICredentialProviderFilter, reinterpret_cast<void**>(&fl));
        f->Release();
    }
    Check(fl != nullptr, "filter instance");
    if (fl) {
        GUID list[1] = {{0x60b78e88, 0xead8, 0x445c, {0x9c, 0xfd, 0x0b, 0x87, 0xf7, 0x4e, 0xa6, 0xcd}}};
        BOOL allow[1] = {TRUE};
        fl->Filter(CPUS_LOGON, 0, list, allow, 1);
        Check(allow[0] == TRUE, "filter hides nothing outside LogonUI");
        fl->Release();
    }
    Check(DllCanUnloadNow() == S_OK, "all objects released");
    IClassFactory* none = nullptr;
    Check(DllGetClassObject(IID_IUnknown, IID_IClassFactory, reinterpret_cast<void**>(&none)) == CLASS_E_CLASSNOTAVAILABLE,
          "unknown CLSID refused");
}

}  // namespace

int wmain() {
    JsonShapes();
    Packing();
    FilterDecision();
    SessionDecisions();
    LinkText();
    TileLinks();
    InertOutsideLogonUI();
    std::printf("%d failed\n", g_failed);
    return g_failed == 0 ? 0 : 1;
}
