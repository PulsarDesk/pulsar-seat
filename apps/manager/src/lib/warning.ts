// SPDX-License-Identifier: GPL-3.0-only
// Warning shown before the EXPERIMENTAL client-SKU session backend (`termsrv_wrapper`,
// plan.md "Decisions for phases 1–5"). The admin's acknowledgement is stored as
// `settings.backends.experimental_ack = EXPERIMENTAL_WARNING_VERSION`; when this text
// changes in substance, bump `EXPERIMENTAL_WARNING_VERSION` in `types.ts` AND in
// `seat_core::config` so every admin has to read it again. `WARNING_VERSION` is the same
// constant. The texts are word-for-word copies of the service's canonical
// `crates/seat-session/src/termwrap/warning_en.txt` / `warning_tr.txt` (`WARNING_EN` /
// `WARNING_TR` in Rust); `warning.test.ts` fails when they differ. `warningText()` picks
// the one of the active UI language.

import { activeLang } from './i18n.ts';
import { EXPERIMENTAL_WARNING_VERSION } from './types.ts';

export const WARNING_VERSION = EXPERIMENTAL_WARNING_VERSION;

export const WARNING_EN = [
  "**Experimental feature — read carefully.**",
  "If you turn this option on, Pulsar Seat itself redirects the Remote Desktop service (TermService) to an open-source wrapper **that you installed**, so that every seat can have its own session at the same time on client editions of Windows (Pro, Enterprise): it points the TermService\\Parameters\\ServiceDll value at the wrapper's DLL file and restarts the service. Pulsar Seat does not contain or install the wrapper; it does not change Windows files on disk and does not touch the kernel or the session manager (LSM).",
  "- **The wrapper changes the code of the Remote Desktop service (termsrv) in memory while it runs.** Pulsar Seat's own rule is not to touch termsrv; this option is an experimental exception used only when you explicitly acknowledge it.",
  "- Pulsar Seat also writes two registry values that Microsoft does not document (EnableConcurrentSessions, AllowMultipleTSSessions); when the option is turned off, these values and ServiceDll are restored to their old state.",
  "- The Remote Desktop service restarts when the option is turned on and off; all open extra seat sessions close.",
  "- Microsoft's licence terms for client Windows do not allow several users at the same time. This use is a grey area for licensing; the responsibility is yours. The licensed path is Windows Server + the Remote Desktop Session Host role and RDS client access licences (CAL).",
  "- Windows updates can change the termsrv.dll file. On a version it does not recognise, Pulsar Seat turns the option off by itself; until the wrapper is updated, extra seats do not run and seats fall back to fast user switching.",
  "- Antivirus software may flag the wrapper as unwanted software.",
  "- On ARM64 PCs the wrapper must be native ARM64; otherwise it is not used.",
  "- This option never requires you to change Memory Integrity, Secure Boot or any other Windows security setting.",
  "Confirm that you understand these risks to continue."
].join('\n');

export const WARNING_TR = [
  "**Deneysel özellik — dikkatle okuyun.**",
  "Bu seçeneği açarsanız Pulsar Seat, Windows'un istemci sürümlerinde (Pro, Enterprise) her koltuğa aynı anda kendi oturumunu verebilmek için Uzak Masaüstü hizmetini (TermService) **sizin kurduğunuz** açık kaynaklı bir sarmalayıcıya kendisi yönlendirir: TermService\\Parameters\\ServiceDll değerini sarmalayıcının DLL dosyasına çevirir ve hizmeti yeniden başlatır. Pulsar Seat sarmalayıcıyı içermez ve kurmaz; diskteki Windows dosyalarını değiştirmez, çekirdeğe ve oturum yöneticisine (LSM) dokunmaz.",
  "- **Sarmalayıcı, Uzak Masaüstü hizmetinin (termsrv) kodunu çalışma sırasında bellekte değiştirir.** Pulsar Seat'in kendi kuralı termsrv'ye dokunmamaktır; bu seçenek, yalnızca açıkça onayladığınızda kullanılan deneysel bir istisnadır.",
  "- Pulsar Seat, Microsoft'un belgelemediği iki kayıt defteri değerini de (EnableConcurrentSessions, AllowMultipleTSSessions) yazar; seçeneği kapatınca bu değerler ve ServiceDll eski hâllerine döndürülür.",
  "- Açma ve kapatma sırasında Uzak Masaüstü hizmeti yeniden başlatılır; açık olan bütün ek koltuk oturumları kapanır.",
  "- Microsoft'un Windows istemci lisans koşulları aynı anda birden fazla kullanıcıya izin vermez. Bu kullanım lisans açısından gri bir alandadır; sorumluluk size aittir. Lisanslı yol, Windows Server + Uzak Masaüstü Oturumu Ana Bilgisayarı rolü ve RDS istemci erişim lisanslarıdır (CAL).",
  "- Windows güncellemeleri termsrv.dll dosyasını değiştirebilir. Pulsar Seat tanımadığı bir sürümde seçeneği otomatik olarak kapatır; sarmalayıcı güncellenene kadar ek koltuklar çalışmaz ve hızlı kullanıcı değiştirmeye geri dönülür.",
  "- Virüs koruma yazılımları sarmalayıcıyı istenmeyen yazılım olarak işaretleyebilir.",
  "- ARM64 bilgisayarlarda sarmalayıcının yerel ARM64 olması gerekir; değilse kullanılmaz.",
  "- Bu seçenek Bellek Bütünlüğü, Güvenli Önyükleme ya da başka bir Windows güvenlik ayarını değiştirmenizi asla gerektirmez.",
  "Devam etmek için bu riskleri anladığınızı onaylayın."
].join('\n');

/** The warning in the active UI language. */
export function warningText(): string {
  return activeLang() === 'tr' ? WARNING_TR : WARNING_EN;
}

/** `**bold**` segments of one line. */
export function boldParts(line: string): { text: string; bold: boolean }[] {
  return line.split('**').map((s, i) => ({ text: s, bold: i % 2 === 1 })).filter((p) => p.text !== '');
}
