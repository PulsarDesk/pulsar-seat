// SPDX-License-Identifier: GPL-3.0-only
// UI copy of the credential provider tiles: English default, Turkish locale (same keys as
// every entry is a pair; `tests/selftest.cpp` checks that both sides are filled). The
// language is the UI language of LogonUI's process (`GetUserDefaultUILanguage`; LogonUI
// runs as SYSTEM, so this is the machine's default UI language: HYPOTHESIS, untested on
// hardware). Mirrors `seat_core::i18n` on the Rust side.
#pragma once

#include <windows.h>

namespace pulsar::copy {

enum class Lang { En, Tr };

struct Text {
    const wchar_t* en;
    const wchar_t* tr;
};

// Primary language id LANG_TURKISH = 0x1F; everything else is English.
inline Lang FromLangId(LANGID id) { return (id & 0x3ff) == 0x1f ? Lang::Tr : Lang::En; }

inline Lang Current() { return FromLangId(GetUserDefaultUILanguage()); }

inline const wchar_t* Pick(const Text& t, Lang l) { return l == Lang::Tr ? t.tr : t.en; }

inline const wchar_t* Get(const Text& t) { return Pick(t, Current()); }

inline constexpr Text kAccount{L"Account", L"Hesap"};
inline constexpr Text kPassword{L"Password", L"Parola"};
inline constexpr Text kSubmit{L"Sign in", L"Gönder"};
inline constexpr Text kSwitchSeat{L"Switch seat", L"Koltuk değiştir"};
// Followed by the seat number.
inline constexpr Text kSeat{L"Seat ", L"Koltuk "};
inline constexpr Text kSwitchHint{L"Give the screen to another seat", L"Ekranı başka bir koltuğa verin"};
// "Seat N starting…": prefix + number + suffix.
inline constexpr Text kStartingPrefix{L"Seat ", L"Koltuk "};
inline constexpr Text kStartingSuffix{L" is starting…", L" açılıyor…"};
inline constexpr Text kSwitchRefused{L"Could not switch: the Pulsar Seat service did not accept the request",
                                     L"Geçilemedi: Pulsar Seat hizmeti isteği kabul etmedi"};
inline constexpr Text kEnterPassword{L"Enter the password.", L"Parolayı girin."};
inline constexpr Text kSavedPasswordUnavailable{L"The saved password could not be read. Enter the password.",
                                                L"Kayıtlı parola alınamadı. Parolayı girin."};
inline constexpr Text kSavedPasswordRejected{L"The saved password was not accepted. Enter the password.",
                                             L"Kayıtlı parola kabul edilmedi. Parolayı girin."};
// English switch link: prefix + number (the Turkish one adds the dative suffix).
inline constexpr Text kSwitchToPrefix{L"Switch to seat ", L"Koltuk "};

inline constexpr const Text* kAll[] = {&kAccount,      &kPassword,       &kSubmit,        &kSwitchSeat,
                                       &kSeat,         &kSwitchHint,     &kStartingPrefix, &kStartingSuffix,
                                       &kSwitchRefused, &kEnterPassword, &kSavedPasswordUnavailable,
                                       &kSavedPasswordRejected, &kSwitchToPrefix};

}  // namespace pulsar::copy
