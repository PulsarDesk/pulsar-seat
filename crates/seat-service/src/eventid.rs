// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat's own stable Event Log ids (spec 20 §13.6: our own numbering, Windows error
//! code next to it in the message). Never reuse or renumber an id.

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

/// Turkish Event Viewer text per id (UI copy, spec 20 §13.1). The English technical line
/// follows it in the event as the detail.
pub fn turkish(id: u32) -> &'static str {
    match id {
        SERVICE_STARTED => "Pulsar Seat hizmeti başladı.",
        SERVICE_STOPPED => "Pulsar Seat hizmeti düzgün biçimde durdu.",
        SAFE_MODE => "Pulsar Seat güvenli kipte: oturma yerleri kendiliğinden başlatılmıyor.",
        OS_BUILD_CHANGED => "Windows sürümü değişti; oturma yerlerini denetleyin.",
        PIPE_FAILED => "Pulsar Seat denetim kanalı açılamadı.",
        CONFIG_STORE_FAILED => "Pulsar Seat yapılandırması okunamadı veya kaydedilemedi.",
        SECRET_STORE_FAILED => "Oturma yeri parolası saklanamadı veya okunamadı.",
        SEAT_START_FAILED => "Bir oturma yeri başlatılamadı.",
        SEAT_HOST_EXITED => "Bir oturma yerinin ana işlemi beklenmedik biçimde kapandı.",
        SEAT_LOGON_FAILED => "Oturma yeri hesabıyla oturum açılamadı; saklanan parola artık kullanılmıyor.",
        BACKEND_FAILED => "Bir Pulsar Seat bileşeni hazırlanamadı.",
        OWNERSHIP_FAILED => "Aygıtlar oturma yerlerine atanamadı.",
        _ => "Pulsar Seat hatası.",
    }
}

/// Event Log message: Turkish text, then the English detail line.
pub fn event_message(id: u32, detail: &str) -> String {
    format!("{}\r\n\r\nAyrıntı: {detail}", turkish(id))
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_id_has_turkish_text() {
        for id in [
            super::SERVICE_STARTED,
            super::SERVICE_STOPPED,
            super::SAFE_MODE,
            super::OS_BUILD_CHANGED,
            super::PIPE_FAILED,
            super::CONFIG_STORE_FAILED,
            super::SECRET_STORE_FAILED,
            super::SEAT_START_FAILED,
            super::SEAT_HOST_EXITED,
            super::SEAT_LOGON_FAILED,
            super::BACKEND_FAILED,
            super::OWNERSHIP_FAILED,
        ] {
            assert_ne!(super::turkish(id), super::turkish(0), "id {id}");
        }
        assert!(super::event_message(1000, "service 1 ready").ends_with("Ayrıntı: service 1 ready"));
    }
}
