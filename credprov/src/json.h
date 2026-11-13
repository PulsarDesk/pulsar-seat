// SPDX-License-Identifier: GPL-3.0-only
// Minimal JSON reader / writer for the service's pipe frames (seat-proto shapes, see
// crates/seat-proto/src/lib.rs test `credprov_shapes`). Strict RFC 8259 subset: objects,
// arrays, strings, numbers (integers only are kept exactly), true / false / null.
// Depth and size limited; never throws.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <utility>
#include <vector>

namespace pulsar::json {

struct Value {
    enum class Kind { Null, Bool, Number, String, Array, Object };
    Kind kind = Kind::Null;
    bool b = false;
    int64_t i = 0;          // integer value of a Number (fraction / exponent truncated)
    bool integral = false;  // Number was an integer literal in range
    std::string s;          // UTF-8 (String)
    std::vector<Value> items;                          // Array
    std::vector<std::pair<std::string, Value>> fields;  // Object

    const Value* Get(const char* key) const;  // Object field or nullptr
    Value* GetMut(const char* key);           // same, for moving a subtree out
    bool IsObject() const { return kind == Kind::Object; }
    bool IsString() const { return kind == Kind::String; }
    bool IsInt() const { return kind == Kind::Number && integral; }
    bool IsTrue() const { return kind == Kind::Bool && b; }
    // Wipe every string in the tree (the tree held a secret).
    void Wipe();
};

// Parse `text` (whole input must be one value). false on any error.
bool Parse(const std::string& text, Value& out);

// Append `s` (UTF-8) as a JSON string literal.
void AppendString(std::string& out, const std::string& s);

// UTF-8 <-> UTF-16. Invalid input gives false / replacement-free failure.
bool Utf8ToWide(const std::string& in, std::wstring& out);
std::string WideToUtf8(const std::wstring& in);

}  // namespace pulsar::json
