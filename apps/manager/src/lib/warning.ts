// SPDX-License-Identifier: GPL-3.0-only
// Warning shown before the EXPERIMENTAL client-SKU session backend (`termsrv_wrapper`,
// plan.md "Decisions for phases 1–5"). The admin's acknowledgement is stored as
// `settings.backends.experimental_ack = EXPERIMENTAL_WARNING_VERSION`; when this text
// changes in substance, bump `EXPERIMENTAL_WARNING_VERSION` in `types.ts` AND in
// `seat_core::config` so every admin has to read it again. `WARNING_VERSION` is the same
// constant. The text is a word-for-word copy of the service's canonical
// `crates/seat-session/src/termwrap/warning_tr.txt` (`WARNING_TR` in Rust);
// `warning.test.ts` fails when the two differ.

import { EXPERIMENTAL_WARNING_VERSION } from './types.ts';

export const WARNING_VERSION = EXPERIMENTAL_WARNING_VERSION;

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

/** `**bold**` segments of one line. */
export function boldParts(line: string): { text: string; bold: boolean }[] {
  return line.split('**').map((s, i) => ({ text: s, bold: i % 2 === 1 })).filter((p) => p.text !== '');
}
