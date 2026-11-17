// SPDX-License-Identifier: GPL-3.0-only
//! User-facing strings of the setup program (English default, Turkish locale).

use seat_core::i18n::Text;

pub const TITLE: Text = Text::new("Pulsar Seat Setup", "Pulsar Seat Kurulumu");

pub const USAGE: Text = Text::new(
    "Pulsar Seat Setup {0}\n\n\
     pulsar-seat-setup [/quiet | /passive] [/norestart] [/log <file>] [NAME=value ...]\n\
     pulsar-seat-setup /extract <folder>\n\
     pulsar-seat-setup /payload-info\n\n\
     Runs the Pulsar Seat MSI for this PC's architecture (ARM64 or x64) with Windows \
     Installer. NAME=value sets a public MSI property (for example REMOVEUSERDATA=1). \
     /extract writes the MSI into a folder for deployment without installing it.",
    "Pulsar Seat Kurulumu {0}\n\n\
     pulsar-seat-setup [/quiet | /passive] [/norestart] [/log <dosya>] [AD=değer ...]\n\
     pulsar-seat-setup /extract <klasör>\n\
     pulsar-seat-setup /payload-info\n\n\
     Bu bilgisayarın mimarisine (ARM64 veya x64) uygun Pulsar Seat MSI paketini Windows \
     Installer ile çalıştırır. AD=değer bir genel MSI özelliğini ayarlar (örneğin \
     REMOVEUSERDATA=1). /extract, MSI paketini kurmadan dağıtım için bir klasöre yazar.",
);

pub const BAD_ARG: Text = Text::new(
    "Unknown or invalid argument: {0}\n\nRun pulsar-seat-setup /? for help.",
    "Bilinmeyen veya geçersiz bağımsız değişken: {0}\n\nYardım için pulsar-seat-setup /? komutunu çalıştırın.",
);

pub const WRONG_ARCH: Text = Text::new(
    "This setup program contains the {0} package, but this PC is {1}. Download PulsarSeat_{2}_{1}_setup.exe from the release page.",
    "Bu kurulum programı {0} paketini içeriyor, ancak bu bilgisayar {1}. Sürüm sayfasından PulsarSeat_{2}_{1}_setup.exe dosyasını indirin.",
);

pub const NO_PACKAGE: Text = Text::new(
    "No installer package found. Put {0} next to this setup program, or download the setup program for this PC ({1}).",
    "Kurulum paketi bulunamadı. {0} dosyasını bu kurulum programının yanına koyun veya bu bilgisayar için olan kurulum programını ({1}) indirin.",
);

pub const UNKNOWN_ARCH: Text = Text::new(
    "This PC's processor architecture is not supported (machine 0x{0}). Pulsar Seat runs on x64 and ARM64.",
    "Bu bilgisayarın işlemci mimarisi desteklenmiyor (makine 0x{0}). Pulsar Seat x64 ve ARM64 üzerinde çalışır.",
);

pub const HASH_MISMATCH: Text = Text::new(
    "The installer package {0} is damaged or was changed (SHA-256 does not match). Download it again.",
    "{0} kurulum paketi bozuk veya değiştirilmiş (SHA-256 eşleşmiyor). Yeniden indirin.",
);

pub const WRONG_SIGNER: Text = Text::new(
    "The installer package {0} is signed by a certificate this setup does not trust ({1}). It was not started.",
    "{0} kurulum paketi bu kurulumun güvenmediği bir sertifikayla imzalanmış ({1}). Başlatılmadı.",
);
pub const NOT_SIGNED: Text = Text::new(
    "The installer package {0} has no valid digital signature ({1}). It was not started.",
    "{0} kurulum paketinin geçerli bir dijital imzası yok ({1}). Başlatılmadı.",
);

pub const WRITE_FAILED: Text = Text::new(
    "Could not unpack the installer package: {0}",
    "Kurulum paketi açılamadı: {0}",
);

pub const LAUNCH_FAILED: Text = Text::new(
    "Could not start Windows Installer (msiexec): {0}",
    "Windows Installer (msiexec) başlatılamadı: {0}",
);

pub const EXTRACTED: Text = Text::new("Wrote {0} to {1}.", "{0}, {1} klasörüne yazıldı.");

pub const NO_PAYLOAD: Text = Text::new(
    "This setup program has no embedded package; it looks for {0} next to itself.",
    "Bu kurulum programında gömülü paket yok; yanında {0} dosyasını arar.",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_text_has_both_languages() {
        let table = seat_core::text_table![
            TITLE,
            USAGE,
            BAD_ARG,
            WRONG_ARCH,
            NO_PACKAGE,
            UNKNOWN_ARCH,
            HASH_MISMATCH,
            NOT_SIGNED,
            WRONG_SIGNER,
            WRITE_FAILED,
            LAUNCH_FAILED,
            EXTRACTED,
            NO_PAYLOAD,
        ];
        assert_eq!(seat_core::i18n::check_table(table), Vec::<String>::new());
    }
}
