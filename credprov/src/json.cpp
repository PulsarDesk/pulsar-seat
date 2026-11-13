// SPDX-License-Identifier: GPL-3.0-only
#include "json.h"

#include <windows.h>

#include <cstring>

namespace pulsar::json {
namespace {

constexpr int kMaxDepth = 16;

void Scrub(std::string& s) {
    // Whole capacity (no reallocation: resize stays within it), so bytes past size() from
    // an earlier, longer content are cleared too.
    s.resize(s.capacity());
    if (!s.empty()) SecureZeroMemory(s.data(), s.size());
    s.clear();
}

struct Reader {
    const char* p;
    const char* end;
    bool ok = true;

    void Ws() {
        while (p < end && (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')) ++p;
    }
    bool Eat(char c) {
        Ws();
        if (p < end && *p == c) {
            ++p;
            return true;
        }
        return false;
    }
    bool Lit(const char* w) {
        size_t n = strlen(w);
        if (static_cast<size_t>(end - p) < n || memcmp(p, w, n) != 0) return false;
        p += n;
        return true;
    }
    static int Hex(char c) {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        return -1;
    }
    bool Hex4(uint32_t& v) {
        if (end - p < 4) return false;
        v = 0;
        for (int k = 0; k < 4; ++k) {
            int h = Hex(p[k]);
            if (h < 0) return false;
            v = (v << 4) | static_cast<uint32_t>(h);
        }
        p += 4;
        return true;
    }
    static void PutUtf8(std::string& o, uint32_t cp) {
        if (cp < 0x80) {
            o.push_back(static_cast<char>(cp));
        } else if (cp < 0x800) {
            o.push_back(static_cast<char>(0xC0 | (cp >> 6)));
            o.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        } else if (cp < 0x10000) {
            o.push_back(static_cast<char>(0xE0 | (cp >> 12)));
            o.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
            o.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        } else {
            o.push_back(static_cast<char>(0xF0 | (cp >> 18)));
            o.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
            o.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
            o.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
        }
    }
    // Raw length up to the closing quote (escapes skipped); the decoded text is never longer.
    size_t RawLen() const {
        const char* q = p;
        while (q < end && *q != '"') {
            if (*q == '\\' && q + 1 < end) ++q;
            ++q;
        }
        return static_cast<size_t>(q - p);
    }
    // Strings may carry the password. They are decoded into one buffer reserved up front
    // (never reallocated, so no unwiped copy is freed) that is always on the heap (at
    // least 32 bytes, past the small-string buffer), so moving the string moves the
    // pointer instead of copying the bytes into a stack object that is never wiped.
    bool Str(std::string& o) {
        if (p >= end || *p != '"') return false;
        ++p;
        const size_t raw = RawLen();
        o.reserve(raw < 32 ? 32 : raw + 1);
        while (p < end) {
            char c = *p++;
            if (c == '"') return true;
            if (static_cast<unsigned char>(c) < 0x20) return false;
            if (c != '\\') {
                o.push_back(c);
                continue;
            }
            if (p >= end) return false;
            char e = *p++;
            switch (e) {
                case '"': o.push_back('"'); break;
                case '\\': o.push_back('\\'); break;
                case '/': o.push_back('/'); break;
                case 'b': o.push_back('\b'); break;
                case 'f': o.push_back('\f'); break;
                case 'n': o.push_back('\n'); break;
                case 'r': o.push_back('\r'); break;
                case 't': o.push_back('\t'); break;
                case 'u': {
                    uint32_t cp;
                    if (!Hex4(cp)) return false;
                    if (cp >= 0xD800 && cp <= 0xDBFF) {
                        uint32_t lo;
                        if (end - p < 2 || p[0] != '\\' || p[1] != 'u') return false;
                        p += 2;
                        if (!Hex4(lo) || lo < 0xDC00 || lo > 0xDFFF) return false;
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                    } else if (cp >= 0xDC00 && cp <= 0xDFFF) {
                        return false;
                    }
                    PutUtf8(o, cp);
                    break;
                }
                default: return false;
            }
        }
        return false;
    }
    bool Num(Value& v) {
        const char* s = p;
        bool neg = false;
        if (p < end && *p == '-') {
            neg = true;
            ++p;
        }
        if (p >= end || *p < '0' || *p > '9') return false;
        uint64_t acc = 0;
        bool overflow = false;
        while (p < end && *p >= '0' && *p <= '9') {
            uint64_t d = static_cast<uint64_t>(*p - '0');
            if (acc > (UINT64_MAX - d) / 10) overflow = true;
            acc = acc * 10 + d;
            ++p;
        }
        bool frac = false;
        if (p < end && *p == '.') {
            frac = true;
            ++p;
            if (p >= end || *p < '0' || *p > '9') return false;
            while (p < end && *p >= '0' && *p <= '9') ++p;
        }
        if (p < end && (*p == 'e' || *p == 'E')) {
            frac = true;
            ++p;
            if (p < end && (*p == '+' || *p == '-')) ++p;
            if (p >= end || *p < '0' || *p > '9') return false;
            while (p < end && *p >= '0' && *p <= '9') ++p;
        }
        (void)s;
        v.kind = Value::Kind::Number;
        const uint64_t lim = neg ? (static_cast<uint64_t>(INT64_MAX) + 1) : static_cast<uint64_t>(INT64_MAX);
        v.integral = !frac && !overflow && acc <= lim;
        if (v.integral) {
            v.i = neg ? static_cast<int64_t>(0 - acc) : static_cast<int64_t>(acc);
        }
        return true;
    }
    bool Val(Value& v, int depth) {
        if (depth > kMaxDepth) return false;
        Ws();
        if (p >= end) return false;
        switch (*p) {
            case '{': {
                ++p;
                v.kind = Value::Kind::Object;
                if (Eat('}')) return true;
                v.fields.reserve(8);
                for (;;) {
                    Ws();
                    std::string k;
                    if (!Str(k)) {
                        Scrub(k);
                        return false;
                    }
                    if (!Eat(':')) return false;
                    Value child;
                    if (!Val(child, depth + 1)) {
                        // A half-parsed value may hold part of a secret.
                        child.Wipe();
                        return false;
                    }
                    // Element moves are noexcept (heap buffers change owner, no copies).
                    v.fields.emplace_back(std::move(k), std::move(child));
                    child.Wipe();  // moved-from: empty, but never leave bytes behind
                    if (Eat(',')) continue;
                    return Eat('}');
                }
            }
            case '[': {
                ++p;
                v.kind = Value::Kind::Array;
                if (Eat(']')) return true;
                v.items.reserve(8);
                for (;;) {
                    Value child;
                    if (!Val(child, depth + 1)) {
                        child.Wipe();
                        return false;
                    }
                    v.items.push_back(std::move(child));
                    child.Wipe();
                    if (Eat(',')) continue;
                    return Eat(']');
                }
            }
            case '"':
                v.kind = Value::Kind::String;
                return Str(v.s);
            case 't':
                v.kind = Value::Kind::Bool;
                v.b = true;
                return Lit("true");
            case 'f':
                v.kind = Value::Kind::Bool;
                return Lit("false");
            case 'n':
                v.kind = Value::Kind::Null;
                return Lit("null");
            default:
                return Num(v);
        }
    }
};

}  // namespace

const Value* Value::Get(const char* key) const {
    if (kind != Kind::Object) return nullptr;
    for (const auto& f : fields) {
        if (f.first == key) return &f.second;
    }
    return nullptr;
}

Value* Value::GetMut(const char* key) {
    return const_cast<Value*>(static_cast<const Value*>(this)->Get(key));
}

void Value::Wipe() {
    Scrub(s);
    for (auto& it : items) it.Wipe();
    for (auto& f : fields) {
        f.second.Wipe();
    }
}

bool Parse(const std::string& text, Value& out) {
    Reader r{text.data(), text.data() + text.size()};
    out = Value{};
    if (!r.Val(out, 0)) return false;
    r.Ws();
    return r.p == r.end;
}

void AppendString(std::string& out, const std::string& s) {
    static const char* hex = "0123456789abcdef";
    out.push_back('"');
    for (unsigned char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (c < 0x20) {
                    out += "\\u00";
                    out.push_back(hex[c >> 4]);
                    out.push_back(hex[c & 0xF]);
                } else {
                    out.push_back(static_cast<char>(c));
                }
        }
    }
    out.push_back('"');
}

bool Utf8ToWide(const std::string& in, std::wstring& out) {
    out.clear();
    if (in.empty()) return true;
    if (in.size() > 0x7FFFFFFF) return false;
    int n = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, in.data(), static_cast<int>(in.size()), nullptr, 0);
    if (n <= 0) return false;
    out.resize(static_cast<size_t>(n));
    return MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, in.data(), static_cast<int>(in.size()), out.data(), n) == n;
}

std::string WideToUtf8(const std::wstring& in) {
    if (in.empty() || in.size() > 0x7FFFFFFF) return {};
    int n = WideCharToMultiByte(CP_UTF8, 0, in.data(), static_cast<int>(in.size()), nullptr, 0, nullptr, nullptr);
    if (n <= 0) return {};
    std::string out(static_cast<size_t>(n), '\0');
    WideCharToMultiByte(CP_UTF8, 0, in.data(), static_cast<int>(in.size()), out.data(), n, nullptr, nullptr);
    return out;
}

}  // namespace pulsar::json
