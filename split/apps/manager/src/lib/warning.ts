// SPDX-License-Identifier: GPL-3.0-only
// Experimental multi-session warning (contracts §2.4). Must stay identical to
// `split_termwrap::WARNING_TR` (checked by warning.test.ts); the service also sends its own
// copy in `termwrap_status.warning_text`, which the dialog prefers.

/** Bump together with `split_termwrap::WARNING_VERSION`; the ack is per version. */
export const WARNING_VERSION = 1;

export const WARNING_TR =
  "**Deneysel özellik — dikkatle okuyun.**\nBu modül, Windows'un istemci sürümlerinde (Pro, Enterprise) aynı anda birden fazla oturum açılabilmesi için Uzak Masaüstü hizmetini (TermService) **sizin kurduğunuz** açık kaynaklı bir sarmalayıcıya yönlendirir. Pulsar Split bu sarmalayıcıyı içermez ve Windows dosyalarını değiştirmez.\n\n- Microsoft'un Windows istemci lisans koşulları aynı anda birden fazla uzak kullanıcıya izin vermez. Bu kullanım lisans açısından gri bir alandadır; sorumluluk size aittir. Lisanslı yol, Windows Server + Uzak Masaüstü Oturumu Ana Bilgisayarı rolü ve RDS istemci erişim lisanslarıdır (CAL).\n- Windows güncellemeleri `termsrv.dll` dosyasını değiştirebilir. Pulsar Split tanımadığı bir sürümde modülü otomatik olarak kapatır; sarmalayıcı güncellenene kadar çoklu koltuk çalışmaz.\n- Virüs koruma yazılımları sarmalayıcıyı istenmeyen yazılım olarak işaretleyebilir.\n- Açma ve kapatma sırasında Uzak Masaüstü hizmeti yeniden başlatılır; açık olan bütün uzak oturumlar ve koltuklar kapanır.\n\nDevam etmek için bu riskleri anladığınızı onaylayın.";
