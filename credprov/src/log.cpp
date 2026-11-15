// SPDX-License-Identifier: GPL-3.0-only
#include "log.h"

#include "common.h"

#include <cstdarg>
#include <cwchar>

namespace pulsar {
namespace {

constexpr ULONGLONG kMaxBytes = 1024 * 1024;
SRWLOCK g_logLock = SRWLOCK_INIT;

bool LogPath(wchar_t* out, DWORD cch) {
    DWORD n = ExpandEnvironmentStringsW(L"%ProgramData%\\Pulsar Seat\\logs\\credprov.log", out, cch);
    return n > 0 && n <= cch && out[0] != L'%';
}

}  // namespace

void Log(const wchar_t* fmt, ...) {
    if (!InLogonUI()) return;
    wchar_t msg[768];
    va_list ap;
    va_start(ap, fmt);
    HRESULT hr = StringCchVPrintfW(msg, ARRAYSIZE(msg), fmt, ap);
    va_end(ap);
    if (FAILED(hr) && hr != STRSAFE_E_INSUFFICIENT_BUFFER) return;
    SYSTEMTIME t;
    GetSystemTime(&t);
    DWORD session = 0;
    ProcessIdToSessionId(GetCurrentProcessId(), &session);
    wchar_t line[900];
    StringCchPrintfW(line, ARRAYSIZE(line), L"%04u-%02u-%02uT%02u:%02u:%02u.%03uZ pid %lu session %lu %s\r\n", t.wYear, t.wMonth,
                     t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds, GetCurrentProcessId(), session, msg);
    char utf8[2048];
    int len = WideCharToMultiByte(CP_UTF8, 0, line, -1, utf8, sizeof(utf8), nullptr, nullptr);
    if (len <= 1) return;
    wchar_t path[MAX_PATH];
    if (!LogPath(path, MAX_PATH)) return;
    AcquireSRWLockExclusive(&g_logLock);
    WIN32_FILE_ATTRIBUTE_DATA a{};
    if (GetFileAttributesExW(path, GetFileExInfoStandard, &a) &&
        ((static_cast<ULONGLONG>(a.nFileSizeHigh) << 32) | a.nFileSizeLow) > kMaxBytes) {
        wchar_t old[MAX_PATH];
        if (SUCCEEDED(StringCchPrintfW(old, MAX_PATH, L"%s.1", path))) MoveFileExW(path, old, MOVEFILE_REPLACE_EXISTING);
    }
    // Plain append; the folder is the service's (SYSTEM + Administrators), never a user's.
    HANDLE h = CreateFileW(path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_ALWAYS,
                           FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
    if (h != INVALID_HANDLE_VALUE) {
        DWORD written = 0;
        WriteFile(h, utf8, static_cast<DWORD>(len - 1), &written, nullptr);
        CloseHandle(h);
    }
    ReleaseSRWLockExclusive(&g_logLock);
}

}  // namespace pulsar
