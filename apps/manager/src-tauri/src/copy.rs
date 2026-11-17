// SPDX-License-Identifier: GPL-3.0-only
//! Native UI copy of the manager shell (window titles, tray menu, file dialogs). English
//! default, Turkish locale (`seat_core::i18n`); the web UI has its own tables
//! (`src/lib/locales/`). The language follows the Windows user UI language until the UI
//! sends the user's choice (`set_ui_language`).

use seat_core::i18n::Text;

pub const WINDOW_TITLE: Text = Text::new("Pulsar Seat Manager", "Pulsar Seat Yönetici");
pub const WIZARD_TITLE: Text = Text::new("Pulsar Seat setup wizard", "Pulsar Seat kurulum sihirbazı");
pub const TRAY_SHOW: Text = Text::new("Show the window", "Pencereyi göster");
pub const TRAY_START: Text = Text::new("Start seats", "Koltukları başlat");
pub const TRAY_TOGGLE: Text = Text::new("Turn Pulsar Seat on / off", "Pulsar Seat'i aç / kapat");
pub const TRAY_QUIT: Text = Text::new("Quit", "Çıkış");
pub const PICK_FOLDER: Text = Text::new("Choose the folder whose programs to add", "Programları eklenecek klasörü seçin");
pub const PICK_PROGRAM: Text = Text::new("Choose a program", "Program seçin");
pub const FILTER_PROGRAMS: Text = Text::new("Programs and shortcuts", "Programlar ve kısayollar");
pub const FILTER_ALL: Text = Text::new("All files", "Bütün dosyalar");
pub const PICK_VM_IMAGE: Text = Text::new("Choose a Windows image or disk", "Windows görüntüsü ya da diski seçin");
pub const FILTER_VM_IMAGES: Text = Text::new("Windows images and disks", "Windows görüntüleri ve diskleri");
pub const PICK_LANGUAGE_CAB: Text = Text::new("Choose a language pack", "Dil paketi seçin");
pub const FILTER_CAB: Text = Text::new("Language packs", "Dil paketleri");
pub const PICK_HOST_FOLDER: Text = Text::new("Choose a folder", "Klasör seçin");

#[cfg(test)]
pub const ALL: &[(&str, &Text)] = seat_core::text_table![
    WINDOW_TITLE,
    WIZARD_TITLE,
    TRAY_SHOW,
    TRAY_START,
    TRAY_TOGGLE,
    TRAY_QUIT,
    PICK_FOLDER,
    PICK_PROGRAM,
    FILTER_PROGRAMS,
    FILTER_ALL,
    PICK_VM_IMAGE,
    FILTER_VM_IMAGES,
    PICK_LANGUAGE_CAB,
    FILTER_CAB,
    PICK_HOST_FOLDER,
];

#[cfg(test)]
mod tests {
    #[test]
    fn copy_table_is_complete() {
        let p = seat_core::i18n::check_table(super::ALL);
        assert!(p.is_empty(), "{p:?}");
    }
}
