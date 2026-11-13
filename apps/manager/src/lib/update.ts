// SPDX-License-Identifier: GPL-3.0-only
// Update check UI helpers (spec 90 §11 item 10). The service does the network work, the
// hash and the signature check; the manager only shows the state and asks the
// administrator before anything is installed. Turkish copy for the update dialog lives here.

import type { UpdateState, UpdateStatus } from './types.ts';

export const updateText = {
  bar: 'Güncellemeler',
  title: 'Güncellemeler',
  intro:
    'Pulsar Seat, isterseniz yeni sürümleri GitHub üzerindeki sürüm sayfasından denetler. Denetim kapalıyken hiçbir bağlantı kurulmaz; bilgisayarınız hakkında hiçbir bilgi gönderilmez.',
  optIn: 'Yeni sürümleri denetle (GitHub)',
  weekly: 'Haftada bir kendiliğinden denetle (yalnızca bildirir, indirmez)',
  current: (v: string, arch: string) => `Kurulu sürüm: ${v}${arch ? ` (${arch.toUpperCase()})` : ''}`,
  lastChecked: (when: string) => `Son denetim: ${when}`,
  never: 'Henüz denetlenmedi.',
  check: 'Şimdi denetle',
  download: 'İndir ve doğrula',
  install: 'Kur…',
  page: 'Sürüm sayfası',
  pageHint: (url: string) => `Sürüm notları: ${url}`,
  state: {
    idle: 'Denetim yapılmadı.',
    checking: 'Denetleniyor…',
    up_to_date: 'Pulsar Seat güncel.',
    available: (v: string) => `Yeni sürüm var: ${v}`,
    downloading: (pct: number | null) => (pct === null ? 'İndiriliyor…' : `İndiriliyor… %${pct}`),
    ready: (v: string, signer: string) => `${v} indirildi ve doğrulandı (imzalayan: ${signer}).`,
    installing:
      'Kurulum paketi yeniden doğrulanıyor ve kuruluyor. Pulsar Seat hizmeti kurulum sırasında yeniden başlar; yönetici bağlantısı kısa süre kopabilir.',
    failed: 'Son adım başarısız oldu.'
  },
  confirmTitle: 'Güncelleme kurulsun mu?',
  confirmBody: (v: string) =>
    `Pulsar Seat ${v} kurulacak. Çalışan tüm koltuklar durdurulacak ve koltuklardaki oturumlar kapanacak. Ayarlarınız ve koltuk parolaları korunur. Gerekirse kurulumdan sonra yeniden başlatmanız istenir.`,
  confirmYes: 'Kur',
  confirmNo: 'Vazgeç',
  needsOptIn: 'Denetim için önce yukarıdaki seçeneği açın.',
  reasons: {
    disabled: 'Güncelleme denetimi kapalı.',
    network: 'Sürüm sayfasına bağlanılamadı. İnternet bağlantısını denetleyin.',
    feed: 'Sürüm bilgisi okunamadı.',
    no_asset: 'Yeni sürümde bu bilgisayarın mimarisi için kurulum paketi yok.',
    no_checksum: 'Yeni sürümün sağlama toplamı (SHA-256) yayımlanmamış; güvenle indirilemez.',
    hash_mismatch: 'İndirilen dosyanın sağlama toplamı tutmuyor; dosya silindi.',
    unsigned: 'Kurulum paketi imzalı değil ya da imzası doğrulanamadı; kurulmayacak.',
    untrusted_signer: 'Kurulum paketi tanınmayan bir yayıncı tarafından imzalanmış; kurulmayacak.',
    // Release signature of SHA256SUMS.txt (minisign), not the installer's Authenticode.
    sums_unsigned:
      'Sürüm imzası (SHA256SUMS.txt.minisig) eksik veya doğrulanamadı; güncelleme sunulmuyor.',
    sums_untrusted:
      'Sürüm imzası bu derlemenin tanıdığı bir sürüm anahtarına ait değil ya da geçersiz; güncelleme sunulmuyor.',
    sums_mismatch: 'Sürüm imzası bu sürüme ait değil; güncelleme sunulmuyor.',
    sums_rollback:
      'Sunulan sürümün imzası, bu bilgisayarın daha önce kabul ettiği bir imzadan eski; güncelleme sunulmuyor.',
    no_release_key: 'Bu derlemede sürüm imza anahtarı yok; güncelleme denetimi devre dışı.',
    too_large: 'Kurulum paketi beklenenden büyük; indirilmedi.',
    launch: 'Kurulum başlatılamadı.',
    install_failed:
      'Kurulum tamamlanamadı (başka bir kurulum sürüyor olabilir). Ayrıntılar: %ProgramData%\\Pulsar Seat\\logs\\update-msi.log. İndirilen paket saklandı; yeniden deneyebilirsiniz.',
    install_timeout:
      'Kurulum bir saat içinde bitmedi. Ayrıntılar: %ProgramData%\\Pulsar Seat\\logs\\update-msi.log. Yeniden deneyebilirsiniz.',
    busy: 'Başka bir güncelleme adımı sürüyor.',
    not_ready: 'Önce yeni sürümü denetleyin ve indirin.',
    not_confirmed: 'Kurulum onaylanmadı.',
    unsupported: 'Bu sürümde güncelleme denetimi kullanılamıyor.'
  } as Record<string, string>
};

/** Turkish line for the current state. */
export function describeUpdate(s: UpdateStatus): string {
  const st: UpdateState = s.state;
  switch (st) {
    case 'available':
      return updateText.state.available(s.latest?.version ?? '?');
    case 'downloading': {
      const size = s.latest?.size ?? 0;
      return updateText.state.downloading(size > 0 ? Math.min(100, Math.floor((s.downloaded * 100) / size)) : null);
    }
    case 'ready':
      return updateText.state.ready(s.latest?.version ?? '?', s.signer ?? '?');
    case 'failed':
      return (s.reason && updateText.reasons[s.reason]) || updateText.state.failed;
    default:
      return updateText.state[st];
  }
}

/** Steps that run on the service and are polled until they finish. `installing` is
 *  polled too: the service reports a failed msiexec run (a real upgrade restarts it). */
export function isBusy(s: UpdateStatus | null): boolean {
  return s !== null && (s.state === 'checking' || s.state === 'downloading' || s.state === 'installing');
}

/** msiexec itself failed or timed out: the verified file is kept and may be installed again. */
export function installRetry(s: UpdateStatus | null): boolean {
  return !!s && s.state === 'failed' && !!s.reason?.startsWith('install_');
}

export function canDownload(s: UpdateStatus | null): boolean {
  return !!s && s.enabled && !!s.latest && (s.state === 'available' || (s.state === 'failed' && s.reason !== 'no_asset'));
}

export function canInstall(s: UpdateStatus | null): boolean {
  return !!s && s.enabled && (s.state === 'ready' || installRetry(s)) && !!s.latest;
}

export function formatChecked(unixS: number | null | undefined): string {
  if (!unixS) return updateText.never;
  return updateText.lastChecked(new Date(unixS * 1000).toLocaleString('tr-TR'));
}
