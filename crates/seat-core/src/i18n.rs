// SPDX-License-Identifier: GPL-3.0-only
//! UI language of the text that Rust code shows to people (tray menus, kiosk shell, seat
//! host notices, the Windows event log, the service CLI, account comments ...).
//!
//! English is the default; Turkish is the one extra locale (umbrella rule "English
//! everywhere, Turkish only as an app i18n locale"). Every user-facing string is a
//! [`Text`] pair, so a missing translation is a compile error; [`check_table`] (called from
//! each crate's tests) also checks that both sides are filled and carry the same `{0}`,
//! `{1}` ... placeholders. Logs, errors meant for logs and wire codes stay English only and
//! do not go through this module.
//!
//! The language is chosen once per process ([`current`]): `PULSAR_SEAT_LANG=en|tr` if set
//! (tests, support), else the Windows user UI language of the process
//! (`GetUserDefaultUILanguage`; for the service running as LocalSystem that is the
//! machine's default UI language — HYPOTHESIS, not checked on hardware). A process may
//! override it with [`set`] (the manager does, after the user picked a language).
//!
//! This is the only platform call in seat-core (one `kernel32` import, no dependency).

use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

/// A supported UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    #[default]
    En,
    Tr,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Tr];

    /// BCP 47 primary subtag.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Tr => "tr",
        }
    }

    /// `en`, `tr`, `tr-TR`, `en_GB` ... (case-insensitive); `None` for anything else.
    pub fn from_tag(tag: &str) -> Option<Lang> {
        let primary = tag.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        match primary.as_str() {
            "en" => Some(Lang::En),
            "tr" => Some(Lang::Tr),
            _ => None,
        }
    }

    /// Windows LANGID → language (primary language id `LANG_TURKISH` = 0x1F).
    pub fn from_langid(langid: u16) -> Lang {
        if langid & 0x3ff == 0x1f { Lang::Tr } else { Lang::En }
    }

    fn to_u8(self) -> u8 {
        match self {
            Lang::En => 1,
            Lang::Tr => 2,
        }
    }

    fn from_u8(v: u8) -> Option<Lang> {
        match v {
            1 => Some(Lang::En),
            2 => Some(Lang::Tr),
            _ => None,
        }
    }
}

/// Environment override (tests, support sessions).
pub const LANG_ENV: &str = "PULSAR_SEAT_LANG";

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The language of this process (detected on first use).
pub fn current() -> Lang {
    if let Some(l) = Lang::from_u8(CURRENT.load(Ordering::Relaxed)) {
        return l;
    }
    let l = detect();
    CURRENT.store(l.to_u8(), Ordering::Relaxed);
    l
}

/// Override the language of this process.
pub fn set(lang: Lang) {
    CURRENT.store(lang.to_u8(), Ordering::Relaxed);
}

/// `PULSAR_SEAT_LANG`, else the OS UI language, else English.
pub fn detect() -> Lang {
    if let Some(l) = std::env::var(LANG_ENV).ok().as_deref().and_then(Lang::from_tag) {
        return l;
    }
    os_ui_lang()
}

#[cfg(windows)]
fn os_ui_lang() -> Lang {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: no arguments, no pointers; documented to always succeed.
    Lang::from_langid(unsafe { GetUserDefaultUILanguage() })
}

#[cfg(not(windows))]
fn os_ui_lang() -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find_map(|v| Lang::from_tag(&v))
        .unwrap_or_default()
}

/// One user-facing string in every supported language. Placeholders are `{0}`, `{1}` ...
/// (filled by [`Text::fmt`]); a literal brace is written `{{` / `}}`. Serializes as
/// `{"en": ..., "tr": ...}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Text {
    pub en: &'static str,
    pub tr: &'static str,
}

impl Text {
    pub const fn new(en: &'static str, tr: &'static str) -> Self {
        Text { en, tr }
    }

    /// The string in `lang`.
    pub fn pick(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.en,
            Lang::Tr => self.tr,
        }
    }

    /// The string in the process language.
    pub fn get(&self) -> &'static str {
        self.pick(current())
    }

    /// The string in the process language with `{n}` replaced by `args[n]`.
    pub fn fmt(&self, args: &[&dyn Display]) -> String {
        fill(self.get(), args)
    }

    /// The string in `lang` with `{n}` replaced by `args[n]`.
    pub fn fmt_in(&self, lang: Lang, args: &[&dyn Display]) -> String {
        fill(self.pick(lang), args)
    }

    /// `s` equals this text in any supported language (markers written in whatever
    /// language the writing process had, such as account comments).
    pub fn matches(&self, s: &str) -> bool {
        Lang::ALL.iter().any(|l| self.pick(*l) == s)
    }
}

/// Replace `{0}`, `{1}` ... by the arguments; `{{` / `}}` are literal braces. An index
/// without an argument stays as written.
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut digits = String::new();
                while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                    digits.push(d);
                    chars.next();
                }
                if !digits.is_empty() && chars.peek() == Some(&'}') {
                    chars.next();
                    match digits.parse::<usize>().ok().and_then(|i| args.get(i)) {
                        Some(a) => out.push_str(&a.to_string()),
                        None => {
                            out.push('{');
                            out.push_str(&digits);
                            out.push('}');
                        }
                    }
                } else {
                    out.push('{');
                    out.push_str(&digits);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Sorted placeholder indices of a template (`{{` escapes skipped).
pub fn placeholders(template: &str) -> Vec<usize> {
    let mut v = Vec::new();
    let b = template.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            if b.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && b.get(j) == Some(&b'}') {
                if let Ok(n) = template[i + 1..j].parse() {
                    v.push(n);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Check a locale table: both languages filled, same placeholders, unique names. Returns
/// every problem found (empty = fine). Each crate calls it from a test on its own table.
pub fn check_table(table: &[(&str, &Text)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (name, text) in table {
        if !seen.insert(*name) {
            problems.push(format!("{name}: duplicate key"));
        }
        for lang in Lang::ALL {
            if text.pick(lang).trim().is_empty() {
                problems.push(format!("{name}: empty {}", lang.code()));
            }
        }
        if placeholders(text.en) != placeholders(text.tr) {
            problems.push(format!("{name}: placeholders differ ({:?} vs {:?})", placeholders(text.en), placeholders(text.tr)));
        }
        for lang in Lang::ALL {
            if let Some(m) = mojibake(text.pick(lang)) {
                problems.push(format!("{name}: {} looks double-encoded ({m:?})", lang.code()));
            }
        }
    }
    problems
}

/// The marker of UTF-8 text that was read as Windows-1252 and written back as UTF-8
/// (U+0131 becomes U+00C4 U+00B1, U+00A7 becomes U+00C2 U+00A7, U+2013 becomes U+00E2 U+20AC
/// U+201C), or `None` when the text looks clean. Turkish and English copy never contains
/// these pairs, so a hit is always an encoding accident. Written with escapes so this file
/// passes its own repository scan.
pub fn mojibake(text: &str) -> Option<&'static str> {
    const MARKERS: [&str; 9] = [
        "\u{c3}\u{a7}",   // ç
        "\u{c3}\u{b6}",   // ö
        "\u{c3}\u{bc}",   // ü
        "\u{c4}\u{b1}",   // ı
        "\u{c4}\u{b0}",   // İ
        "\u{c4}\u{178}",  // ğ (byte 0x9F reads as U+0178 in cp1252)
        "\u{c5}\u{178}",  // ş
        "\u{c2}\u{a7}",   // §
        "\u{e2}\u{20ac}", // – — “ ” ’ …
    ];
    MARKERS.iter().copied().find(|m| text.contains(m))
}

/// Build a `&[(&str, &Text)]` table from named [`Text`] constants, for [`check_table`].
#[macro_export]
macro_rules! text_table {
    ($($name:path),* $(,)?) => {
        &[$((stringify!($name), &$name)),*]
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_langids() {
        assert_eq!(Lang::from_tag("tr-TR"), Some(Lang::Tr));
        assert_eq!(Lang::from_tag("EN_gb"), Some(Lang::En));
        assert_eq!(Lang::from_tag("de-DE"), None);
        assert_eq!(Lang::from_tag(""), None);
        assert_eq!(Lang::from_langid(0x041f), Lang::Tr);
        assert_eq!(Lang::from_langid(0x0409), Lang::En);
        assert_eq!(Lang::from_langid(0x0407), Lang::En, "unsupported languages fall back to English");
        assert_eq!(Lang::Tr.code(), "tr");
    }

    #[test]
    fn fill_replaces_indices_and_keeps_braces() {
        assert_eq!(fill("Seat {0} of {1}", &[&2, &"x"]), "Seat 2 of x");
        assert_eq!(fill("{1} {0} {0}", &[&"a", &"b"]), "b a a");
        assert_eq!(fill("{{0}} {0}", &[&7]), "{0} 7");
        assert_eq!(fill("{2} {x} {", &[&1]), "{2} {x} {");
        assert_eq!(fill("ünicode {0}", &[&"ç"]), "ünicode ç");
    }

    #[test]
    fn placeholders_ignore_escapes() {
        assert_eq!(placeholders("{1} {0} {{2}} {0}"), vec![0, 1]);
        assert!(placeholders("none").is_empty());
    }

    #[test]
    fn check_table_finds_problems() {
        const A: Text = Text::new("a {0}", "b {0}");
        const B: Text = Text::new("a {0}", "b");
        const C: Text = Text::new("a", " ");
        assert!(check_table(text_table![A]).is_empty());
        let p = check_table(text_table![A, B, C, A]);
        assert_eq!(p.len(), 3, "{p:?}");
        const M: Text = Text::new("Seat in use", "ek koltuklar kullan\u{c4}\u{b1}mda");
        let p = check_table(text_table![M]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("double-encoded"), "{p:?}");
    }

    #[test]
    fn mojibake_markers() {
        assert_eq!(mojibake("kullan\u{c4}\u{b1}mda"), Some("\u{c4}\u{b1}"));
        assert_eq!(mojibake("spec 80 \u{c2}\u{a7}11"), Some("\u{c2}\u{a7}"));
        assert_eq!(mojibake("phases 1\u{e2}\u{20ac}\u{201c}5"), Some("\u{e2}\u{20ac}"));
        assert_eq!(mojibake("ba\u{c5}\u{178}lat"), Some("\u{c5}\u{178}"));
        assert_eq!(mojibake("kullanımda, yapılandırma için başlatılıyor, İstanbul, Çağrı, ödül, §5 – ok"), None);
    }

    #[test]
    fn pick_and_set() {
        const T: Text = Text::new("Hello {0}", "Merhaba {0}");
        assert_eq!(T.fmt_in(Lang::Tr, &[&"Ayşe"]), "Merhaba Ayşe");
        assert_eq!(T.pick(Lang::En), "Hello {0}");
        set(Lang::Tr);
        assert_eq!(current(), Lang::Tr);
        set(Lang::En);
        assert_eq!(T.fmt(&[&1]), "Hello 1");
        assert!(T.matches("Merhaba {0}") && T.matches("Hello {0}") && !T.matches("Hallo {0}"));
    }
}
