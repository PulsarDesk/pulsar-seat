// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat's own stable Event Log ids (spec 20 §13.6: our own numbering, Windows error
//! code next to it in the message). Never reuse or renumber an id.

use seat_core::i18n::{Lang, Text};

pub const SERVICE_STARTED: u32 = 1000;
pub const SERVICE_STOPPED: u32 = 1001;
pub const SAFE_MODE: u32 = 1010;
pub const OS_BUILD_CHANGED: u32 = 1011;
pub const GENERIC_ERROR: u32 = 2000;
pub const PIPE_FAILED: u32 = 2001;
pub const CONFIG_STORE_FAILED: u32 = 2002;
pub const SECRET_STORE_FAILED: u32 = 2003;
pub const SEAT_START_FAILED: u32 = 2100;
pub const SEAT_HOST_EXITED: u32 = 2101;
pub const SEAT_LOGON_FAILED: u32 = 2102;
pub const BACKEND_FAILED: u32 = 2200;
pub const OWNERSHIP_FAILED: u32 = 2201;

/// Event Viewer text per id (UI copy, spec 20 §13.1; English default, Turkish locale via
/// `seat_core::i18n`). The English technical line follows it in the event as the detail.
pub fn text(id: u32) -> &'static Text {
    match id {
        SERVICE_STARTED => &T_SERVICE_STARTED,
        SERVICE_STOPPED => &T_SERVICE_STOPPED,
        SAFE_MODE => &T_SAFE_MODE,
        OS_BUILD_CHANGED => &T_OS_BUILD_CHANGED,
        PIPE_FAILED => &T_PIPE_FAILED,
        CONFIG_STORE_FAILED => &T_CONFIG_STORE_FAILED,
        SECRET_STORE_FAILED => &T_SECRET_STORE_FAILED,
        SEAT_START_FAILED => &T_SEAT_START_FAILED,
        SEAT_HOST_EXITED => &T_SEAT_HOST_EXITED,
        SEAT_LOGON_FAILED => &T_SEAT_LOGON_FAILED,
        BACKEND_FAILED => &T_BACKEND_FAILED,
        OWNERSHIP_FAILED => &T_OWNERSHIP_FAILED,
        _ => &T_GENERIC,
    }
}

const T_SERVICE_STARTED: Text = Text::new("The Pulsar Seat service started.", "Pulsar Seat hizmeti başladı.");
const T_SERVICE_STOPPED: Text = Text::new("The Pulsar Seat service stopped cleanly.", "Pulsar Seat hizmeti düzgün biçimde durdu.");
const T_SAFE_MODE: Text = Text::new(
    "Pulsar Seat is in safe mode: seats are not started automatically.",
    "Pulsar Seat güvenli kipte: koltuklar kendiliğinden başlatılmıyor.",
);
const T_OS_BUILD_CHANGED: Text = Text::new("The Windows version changed; check the seats.", "Windows sürümü değişti; koltukları denetleyin.");
const T_PIPE_FAILED: Text = Text::new("The Pulsar Seat control channel could not open.", "Pulsar Seat denetim kanalı açılamadı.");
const T_CONFIG_STORE_FAILED: Text = Text::new(
    "The Pulsar Seat configuration could not be read or saved.",
    "Pulsar Seat yapılandırması okunamadı veya kaydedilemedi.",
);
const T_SECRET_STORE_FAILED: Text = Text::new("A seat password could not be stored or read.", "Koltuk parolası saklanamadı veya okunamadı.");
const T_SEAT_START_FAILED: Text = Text::new("A seat could not start.", "Bir koltuk başlatılamadı.");
const T_SEAT_HOST_EXITED: Text = Text::new("A seat's host process closed unexpectedly.", "Bir koltuğun ana işlemi beklenmedik biçimde kapandı.");
const T_SEAT_LOGON_FAILED: Text = Text::new(
    "Sign-in with the seat account failed; the saved password is no longer used.",
    "Koltuk hesabıyla oturum açılamadı; saklanan parola artık kullanılmıyor.",
);
const T_BACKEND_FAILED: Text = Text::new("A Pulsar Seat component could not be prepared.", "Bir Pulsar Seat bileşeni hazırlanamadı.");
const T_OWNERSHIP_FAILED: Text = Text::new("Devices could not be assigned to seats.", "Aygıtlar koltuklara atanamadı.");
const T_GENERIC: Text = Text::new("Pulsar Seat error.", "Pulsar Seat hatası.");
/// Label of the technical detail line.
const T_DETAIL: Text = Text::new("Detail: {0}", "Ayrıntı: {0}");

/// Event Log message: localized text, then the English detail line.
pub fn event_message(id: u32, detail: &str) -> String {
    event_message_in(seat_core::i18n::current(), id, detail)
}

pub fn event_message_in(lang: Lang, id: u32, detail: &str) -> String {
    format!("{}\r\n\r\n{}", text(id).pick(lang), T_DETAIL.fmt_in(lang, &[&detail]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_has_its_own_text() {
        for id in [
            SERVICE_STARTED,
            SERVICE_STOPPED,
            SAFE_MODE,
            OS_BUILD_CHANGED,
            PIPE_FAILED,
            CONFIG_STORE_FAILED,
            SECRET_STORE_FAILED,
            SEAT_START_FAILED,
            SEAT_HOST_EXITED,
            SEAT_LOGON_FAILED,
            BACKEND_FAILED,
            OWNERSHIP_FAILED,
        ] {
            assert_ne!(text(id), text(0), "id {id}");
        }
        assert!(event_message_in(Lang::Tr, 1000, "service 1 ready").ends_with("Ayrıntı: service 1 ready"));
        assert!(event_message_in(Lang::En, 1000, "service 1 ready").ends_with("Detail: service 1 ready"));
        assert!(event_message_in(Lang::En, 1000, "x").starts_with("The Pulsar Seat service started."));
    }

    #[test]
    fn copy_table_is_complete() {
        let p = seat_core::i18n::check_table(seat_core::text_table![
            T_SERVICE_STARTED,
            T_SERVICE_STOPPED,
            T_SAFE_MODE,
            T_OS_BUILD_CHANGED,
            T_PIPE_FAILED,
            T_CONFIG_STORE_FAILED,
            T_SECRET_STORE_FAILED,
            T_SEAT_START_FAILED,
            T_SEAT_HOST_EXITED,
            T_SEAT_LOGON_FAILED,
            T_BACKEND_FAILED,
            T_OWNERSHIP_FAILED,
            T_GENERIC,
            T_DETAIL,
        ]);
        assert!(p.is_empty(), "{p:?}");
    }
}
