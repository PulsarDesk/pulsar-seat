// SPDX-License-Identifier: GPL-3.0-only
#include "pipe.h"

#include "json.h"

namespace pulsar {
namespace {

constexpr wchar_t kPipeName[] = L"\\\\.\\pipe\\PulsarSeat.Control";
constexpr wchar_t kServiceName[] = L"PulsarSeat";
// seat_proto::PIPE_CLIENT_ACCESS: FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES.
constexpr DWORD kPipeAccess = 0x0012018b;
constexpr DWORD kConnectWaitMs = 500;
constexpr DWORD kRequestMs = 2000;
// One overall budget per ServiceClient call (connect + hello + request): all of it runs on a
// LogonUI thread, so a hung service must cost at most this much per callback.
constexpr ULONGLONG kCallBudgetMs = 1000;
// Our answers are small; anything bigger is not for us.
constexpr DWORD kMaxFrame = 64 * 1024;
constexpr char kClientVersion[] = "0.1.0";

DWORD ServicePid() {
    DWORD pid = 0;
    SC_HANDLE scm = OpenSCManagerW(nullptr, nullptr, SC_MANAGER_CONNECT);
    if (!scm) return 0;
    SC_HANDLE svc = OpenServiceW(scm, kServiceName, SERVICE_QUERY_STATUS);
    if (svc) {
        SERVICE_STATUS_PROCESS ssp{};
        DWORD needed = 0;
        if (QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, reinterpret_cast<BYTE*>(&ssp), sizeof(ssp), &needed) &&
            ssp.dwCurrentState == SERVICE_RUNNING) {
            pid = ssp.dwProcessId;
        }
        CloseServiceHandle(svc);
    }
    CloseServiceHandle(scm);
    return pid;
}

class Conn {
public:
    Conn() = default;
    Conn(const Conn&) = delete;
    Conn& operator=(const Conn&) = delete;
    ~Conn() {
        if (h_ != INVALID_HANDLE_VALUE) CloseHandle(h_);
        if (ev_) CloseHandle(ev_);
    }

    bool Open() {
        deadline_ = GetTickCount64() + kCallBudgetMs;
        ev_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!ev_) return false;
        for (int attempt = 0; attempt < 2; ++attempt) {
            h_ = CreateFileW(kPipeName, kPipeAccess, 0, nullptr, OPEN_EXISTING,
                             FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, nullptr);
            if (h_ != INVALID_HANDLE_VALUE) break;
            const ULONGLONG left = Remaining();
            if (GetLastError() != ERROR_PIPE_BUSY || left == 0 ||
                !WaitNamedPipeW(kPipeName, static_cast<DWORD>(left < kConnectWaitMs ? left : kConnectWaitMs))) {
                return false;
            }
        }
        if (h_ == INVALID_HANDLE_VALUE) return false;
        ULONG server = 0;
        if (!GetNamedPipeServerProcessId(h_, &server) || server == 0) return false;
        DWORD svc = ServicePid();
        return svc != 0 && svc == server;
    }

    // One request, one response. `body` = `{"type":...}` JSON. Returns the `Ok` value.
    bool Call(const std::string& body, json::Value& ok, std::string* errReason = nullptr) {
        const uint64_t id = ++next_;
        std::string req = "{\"version\":1,\"id\":" + std::to_string(id) + ",\"body\":" + body + "}";
        bool sent = SendFrame(req);
        Wipe(req);
        if (!sent) return false;
        for (int frames = 0; frames < 8; ++frames) {
            // The frame may be the credentials answer: frame text and parse tree are wiped
            // on every path out of this iteration (short read, bad JSON, wrong id, ...).
            std::string text;
            WipeOnExit wipeText(text);
            json::Value v;
            struct TreeWipe {
                json::Value& v;
                ~TreeWipe() { v.Wipe(); }
            } wipeTree{v};
            if (!RecvFrame(text)) return false;
            if (!json::Parse(text, v)) return false;
            Wipe(text);
            json::Value* resp = v.GetMut("response");
            if (!resp) continue;  // an event frame (we never subscribe; ignore defensively)
            const json::Value* rid = resp->Get("id");
            json::Value* result = resp->GetMut("result");
            if (!rid || !rid->IsInt() || static_cast<uint64_t>(rid->i) != id || !result) return false;
            if (json::Value* o = result->GetMut("Ok")) {
                // Move, not copy: the secret keeps its one buffer (the caller wipes `ok`).
                ok.Wipe();
                ok = std::move(*o);
                return true;
            }
            if (errReason) {
                if (const json::Value* e = result->Get("Err")) {
                    const json::Value* r = e->Get("reason");
                    const json::Value* c = e->Get("code");
                    if (r && r->IsString()) *errReason = r->s;
                    else if (c && c->IsString()) *errReason = c->s;
                }
            }
            return false;
        }
        return false;
    }

    bool Hello() {
        std::string body =
            "{\"type\":\"hello\",\"data\":{\"client\":\"credential_provider\",\"client_version\":\"" +
            std::string(kClientVersion) + "\",\"protocol_version\":1}}";
        json::Value ok;
        if (!Call(body, ok)) return false;
        const json::Value* t = ok.Get("type");
        return t && t->IsString() && t->s == "hello";
    }

private:
    ULONGLONG Remaining() const {
        const ULONGLONG now = GetTickCount64();
        return now >= deadline_ ? 0 : deadline_ - now;
    }

    // Per-request deadline, capped by the call's overall budget.
    ULONGLONG RequestDeadline() const {
        const ULONGLONG d = GetTickCount64() + kRequestMs;
        return d < deadline_ ? d : deadline_;
    }

    bool Io(bool write, void* buf, DWORD n, ULONGLONG deadline) {
        BYTE* p = static_cast<BYTE*>(buf);
        while (n > 0) {
            ULONGLONG now = GetTickCount64();
            if (now >= deadline) return false;
            OVERLAPPED ov{};
            ov.hEvent = ev_;
            ResetEvent(ev_);
            DWORD done = 0;
            BOOL r = write ? WriteFile(h_, p, n, nullptr, &ov) : ReadFile(h_, p, n, nullptr, &ov);
            if (!r && GetLastError() != ERROR_IO_PENDING) return false;
            if (WaitForSingleObject(ev_, static_cast<DWORD>(deadline - now)) != WAIT_OBJECT_0) {
                CancelIoEx(h_, &ov);
                GetOverlappedResult(h_, &ov, &done, TRUE);
                return false;
            }
            if (!GetOverlappedResult(h_, &ov, &done, FALSE) || done == 0) return false;
            p += done;
            n -= done;
        }
        return true;
    }

    bool SendFrame(const std::string& s) {
        if (s.size() > kMaxFrame) return false;
        std::string f(4 + s.size(), '\0');
        const uint32_t len = static_cast<uint32_t>(s.size());
        for (int k = 0; k < 4; ++k) f[static_cast<size_t>(k)] = static_cast<char>((len >> (8 * k)) & 0xFF);
        memcpy(f.data() + 4, s.data(), s.size());
        bool ok = Io(true, f.data(), static_cast<DWORD>(f.size()), RequestDeadline());
        Wipe(f);
        return ok;
    }

    bool RecvFrame(std::string& out) {
        const ULONGLONG deadline = RequestDeadline();
        unsigned char hdr[4];
        if (!Io(false, hdr, 4, deadline)) return false;
        const uint32_t len = static_cast<uint32_t>(hdr[0]) | (static_cast<uint32_t>(hdr[1]) << 8) |
                             (static_cast<uint32_t>(hdr[2]) << 16) | (static_cast<uint32_t>(hdr[3]) << 24);
        if (len == 0 || len > kMaxFrame) return false;
        Wipe(out);
        out.assign(len, '\0');
        if (Io(false, out.data(), len, deadline)) return true;
        // Short read / timeout: the part already read may be a credentials frame.
        Wipe(out);
        return false;
    }

    HANDLE h_ = INVALID_HANDLE_VALUE;
    HANDLE ev_ = nullptr;
    uint64_t next_ = 0;
    ULONGLONG deadline_ = 0;
};

// Open + hello, or nullptr-equivalent false.
bool Connect(Conn& c) {
    return c.Open() && c.Hello();
}

const json::Value* DataOf(const json::Value& ok, const char* type) {
    const json::Value* t = ok.Get("type");
    if (!t || !t->IsString() || t->s != type) return nullptr;
    return ok.Get("data");
}

std::wstring WideField(const json::Value* d, const char* key) {
    std::wstring w;
    if (!d) return w;
    const json::Value* v = d->Get(key);
    if (v && v->IsString() && !json::Utf8ToWide(v->s, w)) w.clear();
    return w;
}

}  // namespace

bool SameSession(const SessionInfo& a, const SessionInfo& b) {
    return a.reachable == b.reachable && a.seat == b.seat && a.autoSubmit == b.autoSubmit &&
           a.hideOthers == b.hideOthers && a.account == b.account && a.userSid == b.userSid &&
           a.switchTargets == b.switchTargets;
}

SessionInfo ServiceClient::Query(Scenario scenario) {
    SessionInfo info;
    Conn c;
    if (!Connect(c)) return info;
    std::string body = std::string("{\"type\":\"credprov_query\",\"data\":{\"scenario\":\"") +
                       (scenario == Scenario::Unlock ? "unlock" : "logon") + "\"}}";
    json::Value ok;
    if (!c.Call(body, ok)) return info;
    const json::Value* d = DataOf(ok, "credprov_session");
    if (!d || !d->IsObject()) return info;
    info.reachable = true;
    const json::Value* seat = d->Get("seat");
    if (seat && seat->IsInt() && seat->i >= 1 && seat->i <= 64) info.seat = static_cast<int>(seat->i);
    const json::Value* a = d->Get("auto_submit");
    const json::Value* h = d->Get("hide_others");
    const json::Value* p = d->Get("pending");
    info.account = WideField(d, "account");
    info.userSid = WideField(d, "user_sid");
    // The seat tile exists only with an account (Provider::Rebuild): without one, nothing
    // is auto-submitted and the filter must not hide the other providers.
    info.autoSubmit = info.seat != 0 && !info.account.empty() && a && a->IsTrue();
    info.hideOthers = info.autoSubmit && h && h->IsTrue();
    info.pending = scenario == Scenario::Logon && p && p->IsTrue();
    if (const json::Value* t = d->Get("switch_targets"); t && t->kind == json::Value::Kind::Array) {
        for (const auto& it : t->items) {
            if (it.IsInt() && it.i >= 1 && it.i <= 64 && info.switchTargets.size() < 16) {
                info.switchTargets.push_back(static_cast<int>(it.i));
            }
        }
    }
    return info;
}

bool ServiceClient::TakeCredentials(SeatCredential& out) {
    Conn c;
    if (!Connect(c)) return false;
    json::Value ok;
    struct TreeWipe {
        json::Value& v;
        ~TreeWipe() { v.Wipe(); }
    } wipeTree{ok};
    if (!c.Call("{\"type\":\"credprov_take_credentials\"}", ok)) return false;
    bool good = false;
    if (const json::Value* d = DataOf(ok, "credentials")) {
        out.domain = WideField(d, "domain");
        out.user = WideField(d, "user");
        const json::Value* pw = d->Get("password");
        if (pw && pw->IsString() && !out.user.empty() && !pw->s.empty() && pw->s.size() < 0x10000) {
            // Decode straight into a reserved, wiped buffer (no growing std::wstring).
            const int n = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, pw->s.data(), static_cast<int>(pw->s.size()), nullptr, 0);
            if (n > 0) {
                std::vector<wchar_t> w(static_cast<size_t>(n) + 1, L'\0');
                if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, pw->s.data(), static_cast<int>(pw->s.size()), w.data(), n) == n) {
                    out.password.Assign(w.data(), static_cast<size_t>(n));
                    good = !out.password.Empty();
                }
                SecureZeroMemory(w.data(), w.size() * sizeof(wchar_t));
            }
        }
    }
    return good;
}

void ServiceClient::Report(LONG status, LONG subStatus) {
    Conn c;
    if (!Connect(c)) return;
    std::string body = "{\"type\":\"credprov_report\",\"data\":{\"status\":" + std::to_string(status) +
                       ",\"sub_status\":" + std::to_string(subStatus) + "}}";
    json::Value ok;
    c.Call(body, ok);
}

bool ServiceClient::Switch(int seat) {
    if (seat < 1 || seat > 64) return false;
    Conn c;
    if (!Connect(c)) return false;
    std::string body = "{\"type\":\"credprov_switch\",\"data\":{\"seat\":" + std::to_string(seat) + "}}";
    json::Value ok;
    if (!c.Call(body, ok)) return false;
    const json::Value* t = ok.Get("type");
    return t && t->IsString() && t->s == "ack";
}

}  // namespace pulsar
