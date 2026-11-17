// SPDX-License-Identifier: GPL-3.0-only
// Turkish locale of the manager UI copy. Same keys as `en.ts` (the default); the type
// `Locale` makes a missing key a compile error and `i18n.test.ts` checks the key sets.
// Every string here is ours (CLEANROOM.md); no wording is taken from any other product.

import { FKEY_SEATS } from '../wizard.ts';
import type { CheckOutcome, DeviceKind, StartMode } from '../types.ts';
import type {
  ErrorCode as RemoteErrorCode,
  SeatState as RemoteSeatState,
  ServiceRunState,
  Verbosity
} from '../remote/types.ts';
import type { FieldError as RemoteFieldError } from '../remote/validation.ts';
import type { Locale } from './en.ts';

const t = {
  appTitle: 'Pulsar Seat Yönetici',
  loading: 'Yükleniyor…',
  refresh: 'Yenile',
  product: 'Ürün durumu',
  seats: 'Koltuklar',
  seat: 'Koltuk',
  examplePath: 'C:\\Oyunlar\\oyun.exe',
  seatN: (n: number) => `${n}. koltuk`,
  startAll: 'Koltukları başlat',
  stop: 'Durdur',
  relogon: 'Yeniden oturum aç',
  serviceDown: 'Pulsar Seat hizmetine ulaşılamıyor. Hizmetin kurulu ve çalışır durumda olduğundan emin olun; bir güvenlik yazılımı hizmeti durdurmuş olabilir.',
  adminNeeded: 'Bu işlem için yönetici olarak çalıştırmanız gerekir.',
  adminToView: 'Koltuk ayarlarını görmek için yönetici olarak yeniden başlatın (üstteki düğme).',
  restartElevated: 'Yönetici olarak yeniden başlat',
  // Service start / stop / restart (SCM, elevated manager only; restored from Split).
  svc: {
    label: 'Hizmet',
    start: 'Hizmeti başlat',
    stop: 'Hizmeti durdur',
    restart: 'Hizmeti yeniden başlat',
    starting: 'Hizmet başlatılıyor…',
    stopping: 'Hizmet durduruluyor…',
    restarting: 'Hizmet yeniden başlatılıyor…',
    started: 'Hizmet başlatıldı.',
    stopped: 'Hizmet durduruldu.',
    restarted: 'Hizmet yeniden başlatıldı.',
    needElevated: 'Hizmeti başlatmak ya da durdurmak için yöneticiyi yönetici olarak çalıştırın.',
    pending: 'Hizmet şu an durum değiştiriyor, biraz bekleyin.',
    notInstalled: 'Hizmet kurulu değil; Pulsar Seat’i yeniden kurun.',
    confirmStopTitle: 'Hizmet durdurulsun mu?',
    confirmStop: 'Çalışan bütün koltuklar (yerel ve uzak) kapanır. Koltuklardaki kaydedilmemiş işler kaybolabilir.',
    confirmRestartTitle: 'Hizmet yeniden başlatılsın mı?',
    confirmRestart: 'Çalışan bütün koltuklar kapanır. Hizmet açıldıktan sonra koltukları yeniden başlatmanız gerekebilir.'
  },
  ok: 'Tamam',
  cancel: 'Vazgeç',
  close: 'Kapat',
  save: 'Kaydet',
  discard: 'Kaydetmeden devam et',
  yes: 'Evet',
  no: 'Hayır',
  apply: 'Uygula',
  later: 'Sonra',
  none: '—',
  linkDown: 'Hizmetle canlı bağlantı yok; durum birkaç saniyede bir yenileniyor.',
  restricted: 'Yönetici, bu bilgisayarda ayarların yalnızca yöneticilerce görülüp değiştirilmesini istedi.',
  errorJson: 'Metin geçerli bir JSON değil.',
  osBuildChanged: 'Windows güncellendi. Koltuk ayarlarını gözden geçirin; sorun varsa Pulsar Seat’i kapatıp açın.',
  changedElsewhere: 'Ayarlar bu arada başka bir pencereden değiştirildi. Kaydederseniz o değişikliklerin üzerine yazılmaz; önce yeniden yükleyin.',
  reload: 'Yeniden yükle',
  seatOneAdvice: 'Kararlı çalışma için ilk ekran kartının ilk ekranını (1.1) 1. koltuğa vermeniz önerilir.',
  notAdmin: 'Salt okunur görünüm: değişiklik yapmak için yöneticisiniz ama yükseltilmiş değilsiniz ya da yönetici değilsiniz.',

  tabs: {
    seats: 'Koltuklar',
    basic: 'Temel ayarlar',
    advanced: 'Gelişmiş',
    network: 'Ağ adresleri',
    remote: 'Uzak koltuklar',
    status: 'Durum'
  },

  /** Seat kinds (config schema 2): shown next to every seat. */
  seatKind: {
    local: 'Yerel',
    remote: 'Uzak',
    localHint: 'Bu bilgisayara takılı ekran, klavye ve fareyle kullanılan koltuk',
    remoteHint: 'Pulsar ya da Moonlight ile ağ üzerinden kullanılan koltuk',
    remoteList: 'Uzak koltuklar',
    remoteListHint: 'Uzak koltuklara cihaz atanmaz; ayarları Uzak koltuklar sekmesindedir.',
    open: 'Düzenle'
  },

  network: {
    intro: 'Bazı ağ programları ve yerel ağ oyunları her koltuğun kendi IP adresini kullanmasını ister. Her koltuğa bu bilgisayarın sabit IPv4 adreslerinden birini verebilirsiniz.',
    bestEffort: 'Kaynak IP sürücüsü yüklü değilken Pulsar Seat bu ayarı saklar ve yalnızca destekleyen programlarda uygular. Programın kendi “şu adrese bağlan” ayarı varsa onu da aynı adrese getirin.',
    enforcedByDriver: (seats: string) => `Kaynak IP sürücüsü yüklü: ${seats} koltuğunun programları bağlantılarında koltuğun adresini kullanır.`,
    driverReady: 'Kaynak IP sürücüsü yüklü; bir koltuk oturum açınca adresi zorlanır.',
    driverMissing: (why: string) => `Kaynak IP sürücüsü kullanılamıyor (${why}). Adres yalnızca PULSAR_SEAT_SOURCE_IP değişkenini okuyan ya da kendi adres ayarı olan programlarda geçerli.`,
    driverReasons: {
      driver_not_loaded: 'sürücü yüklü değil',
      driver_busy: 'sürücü başka bir işlem tarafından kullanılıyor',
      driver_version_mismatch: 'sürücü sürümü uyuşmuyor',
      driver_pass_through: 'sürücü kurtarma kipinde',
      map_invalid: 'koltuk listesi gönderilemedi',
      map_refused: 'sürücü koltuk listesini kabul etmedi'
    } as Record<string, string>,
    sharedSession: (seats: string) => `${seats} koltuğu oturumunu başka bir koltukla paylaşıyor; adresi sürücüyle zorlanmaz.`,
    droppedPaths: (n: number) => `Program listesindeki ${n} yol sürücüye aktarılamadı (bilinmeyen sürücü harfi ya da göreli yol).`,
    driverScope: 'Sürücü yalnızca IPv4 bağlantılarına dokunur; programın kendisinin seçtiği adresler ve bu bilgisayarın içindeki (127.x) bağlantılar olduğu gibi kalır.',
    seat: 'Koltuk',
    address: 'IP adresi',
    rule: 'Kural',
    notAssigned: 'Atanmadı',
    edit: 'Düzenle…',
    programs: 'Program listesi (bütün koltuklar için ortak)',
    programsCount: (n: number) => `${n} program`,
    addressHelp: 'Ek adres almak için: Ağ bağlantılarında bağdaştırıcıya ikinci bir adres ekleyin ya da Aygıt Yöneticisi’nden sanal bir ağ bağdaştırıcısı ekleyin.'
  },

  ipDialog: {
    title: (n: number) => `${n}. koltuk: IP adresi`,
    address: 'IP adresi',
    notAssigned: 'Atanmadı',
    noStatic: 'Bu bilgisayarda sabit IPv4 adresi bulunamadı. DHCP ile alınan adresler değişebildiği için listelenmez.',
    loadFailed: 'Adres listesi alınamadı.',
    rule: 'Hangi programlar bu adresi kullansın?',
    rules: {
      all_programs: 'Bu koltukta başlatılan bütün programlar',
      only_listed: 'Yalnızca listedeki programlar',
      all_except_listed: 'Listedekiler dışındaki bütün programlar'
    } as Record<string, string>,
    list: 'Program listesi (bütün koltuklar için ortak)',
    title2: 'Başlık',
    path: 'Program ya da klasör yolu',
    pathHint: 'Bir klasör, içindeki bütün programlar demektir.',
    browse: 'Gözat…',
    browseHint: 'Bir program ya da kısayol seçin (Başlat menüsü, masaüstü ya da herhangi bir sürücü); kısayolun gösterdiği program eklenir.',
    addFolder: 'Klasördeki programları ekle',
    folderEmpty: 'Seçilen klasörde program bulunamadı.',
    pickFailed: 'Program seçilemedi.',
    add: 'Ekle',
    remove: 'Sil',
    enabled: 'Etkin',
    tooLong: (max: number) => `Liste çok uzun; kaydederken ilk ${max} satır tutulur.`,
    invalidPath: 'Yol boş olamaz.',
    openNetwork: 'Ağ bağlantılarını aç',
    openDevices: 'Aygıt Yöneticisi’ni aç'
  },

  picture: {
    title: (name: string) => `${name}: resim`,
    default: 'Varsayılan resim',
    builtin: 'Hazır resimler',
    user: 'Kendi resimleriniz',
    userHint: 'Yönetici olarak kendi resimlerinizi %ProgramData%\\Pulsar Seat\\pictures klasöründe aygıt türünün alt klasörüne (ör. keyboard, mouse) koyabilirsiniz. Kullanılmadığı zaman gösterilecek resim için dosya adının sonuna _off ekleyin.',
    noUser: 'Bu tür için kendi resminiz yok.',
    choose: 'Seç'
  },

  logLevel: {
    title: 'Günlük ayrıntısı',
    intro: 'Hatalar ve uyarılar her zaman günlüğe yazılır. Ek ayrıntı sorun ararken işe yarar; günlük dosyası daha hızlı büyür.',
    info: 'Bilgi',
    debug: 'Hata ayıklama',
    trace: 'İz (çok ayrıntılı)'
  },

  outputs: {
    title: 'Görüntü çıkışları',
    intro: 'Her ekran için onu besleyecek ekran kartı çıkışını seçin. Birden fazla çıkış seçmek görüntüyü kopyalar (klonlama). Değişiklikler bir sonraki başlatmada ya da yeniden başlatmadan sonra uygulanır; Windows isteği her zaman yerine getirmeyebilir.',
    adapter: 'Ekran kartı',
    current: 'şu anki',
    automatic: 'Otomatik',
    allowClone: 'Bir ekranı birden fazla çıkışa kopyalamaya izin ver (klonlama)',
    forceAtStart: 'Koltuklar başlarken her ekranı kayıtlı çıkışına yeniden bağla',
    softwareCursor: 'Yazılım fare imleci',
    hdr: 'HDR',
    vrr: 'Değişken yenileme (VRR)',
    hdrVrrHint:
      'HDR ve değişken yenileme 2. ve sonraki koltukların ekranları içindir (1. koltuk Windows ayarlarını kullanır). HDR, ekranın HDR desteği ve Windows 11 gerektirir; yoksa görüntü SDR kalır. Kopya korumalı (HDCP) içerik bu koltuklarda gösterilmez.',
    followMode: 'Ekran çözünürlüğü koltuğun oturumunu izlesin',
    followModeChoice: {
      auto: 'Otomatik (yalnızca masaüstü dışındaki ekranlar)',
      on: 'Açık (masaüstündeki ekranlar dahil; koltuk durunca geri alınır)',
      off: 'Kapalı (görüntü ölçeklenir)'
    } as Record<string, string>,
    cloneNeeded: 'Birden fazla çıkış için önce klonlamaya izin verin.',
    forceNote: 'Seçtiğiniz çıkışlar her koltuk başlatılışında zaten uygulanır. Bu seçenek açıkken hizmet, her koltuk başlarken ekranların koltuklara dağılımını Windows’a yeniden uygular (Windows çıkışları kendiliğinden değiştirdiyse).',
    none: 'Ekran bulunamadı.',
    status: {
      assigned: 'Koltuk başlarken bu çıkışa bağlanır',
      forced: 'Çıkışa zorla atanacak',
      unassigned: 'Atanmadı'
    } as Record<string, string>
  },

  camera: {
    title: (name: string) => `${name}: kamera önizlemesi`,
    starting: 'Kamera açılıyor…',
    denied: 'Kameraya erişim verilmedi ya da kamera başka bir program tarafından kullanılıyor.',
    notFound: 'Bu kamera görüntü aygıtları arasında bulunamadı; ilk kamera gösteriliyor.',
    unsupported: 'Bu pencerede kamera önizlemesi kullanılamıyor.'
  },

  bar: {
    save: 'Kaydet',
    saveHint: 'Kaydedilmemiş değişiklikler yönetici kapatılınca kaybolur. Bazı ayarlar yönetici hakkı ister.',
    enable: 'Pulsar Seat’i aç',
    disable: 'Pulsar Seat’i kapat',
    start: 'Koltukları başlat',
    reboot: 'Yeniden başlat',
    support: 'Tanı raporu',
    discard: 'Değişiklikleri geri al',
    unsaved: 'Kaydedilmemiş değişiklik var'
  },

  title: {
    onSeat: (n: number) => `${n}. koltukta çalışıyor`,
    noSeat: 'Koltuk bilinmiyor'
  },

  canvas: {
    system: 'Ortak ve boştaki aygıtlar',
    systemHint: 'Buradaki aygıtlar ya hiçbir koltuğa verilmemiştir (boşta) ya da bütün koltuklarca paylaşılır. Bir aygıtı koltuğa vermek için sürükleyip koltuğun üzerine bırakın.',
    emptySeat: 'Bu koltuğa henüz aygıt verilmedi.',
    addSeat: 'Koltuk ekle',
    seatLimit: 'Koltuk sınırına ulaşıldı (16).',
    pickByTouch: 'Dokunarak bul',
    pickByTouchHint: 'Bulmak istediğiniz klavyede bir tuşa basın ya da fareyi oynatın.',
    pickCancel: 'Aramayı bitir',
    pickFound: (name: string) => `Bulundu: ${name}`,
    identifyMonitors: 'Ekranları numarala',
    identifyMonitorsStop: 'Numaraları gizle',
    noDevices: 'Aygıt listesi alınamadı.',
    sharedSign: 'Bütün koltuklarca paylaşılıyor',
    spareSign: 'Boşta',
    unplugged: 'Takılı değil',
    justConnected: 'Yeni takıldı',
    attention: 'Dikkat gerektiriyor',
    inherited: (hub: string) => `${hub} göbeğinden geliyor`,
    inheritedPort: (port: string) => `Takılı olduğu bağlantı noktasından geliyor (${port})`,
    inheritedContainer: 'Aygıtın tamamına yapılan atamadan geliyor',
    assignPort: 'Bu bağlantı noktası',
    assignContainer: 'Bu aygıtın tamamı',
    defaultMask: 'Varsayılan atama',
    hubDepth: (n: number) => `Göbek derinliği ${n}`,
    dropHere: 'Buraya bırakın',
    menu: 'Menü',
    deviceMenu: (name: string) => `${name} için menü`,
    seatMenu: (n: number) => `${n}. koltuk menüsü`
  },

  health: {
    not_starting: 'Başlamayacak',
    no_display: 'Ekran yok',
    no_keyboard: 'Klavye yok',
    no_mouse: 'Fare yok',
    ready: 'Her şey hazır'
  } as Record<string, string>,

  deviceMenu: {
    assign: 'Koltuklara ata…',
    spare: 'Boşa çıkar',
    reset: 'Varsayılana döndür',
    indicate: 'Göster (tanıt)',
    indicateStop: 'Göstermeyi durdur',
    showIds: 'Donanım kimliklerini göster',
    hideIds: 'Donanım kimliklerini gizle',
    path: 'Bağlantı yolu',
    sharing: 'Paylaşım durumu',
    softwareCursor: 'Yazılım fare imleci kullan',
    hardwareCursor: 'Donanım fare imleci kullan',
    picture: 'Resmini değiştir…',
    outputs: 'Görüntü çıkışları…',
    preview: 'Kamera önizlemesi',
    moveHere: 'Atamayı bu porta taşı'
  },

  seatMenu: {
    rename: 'Adını değiştir…',
    account: 'Oturum hesabı…',
    relogon: 'Yeniden oturum aç',
    start: 'Bu koltuğu başlat',
    stop: 'Bu koltuğu durdur',
    remove: 'Koltuğu sil',
    cpu: 'İşlemci çekirdekleri…',
    ip: 'IP adresi ata…',
    resetGraphics: 'Ekran kartı sürücüsünü yeniden başlat',
    resetGraphicsSent: 'Ekran kartı sürücüsünü yeniden başlatma komutu koltuğa gönderildi (ekran bir an kararabilir).',
    removeKioskUser: 'Koltuğun kiosk kullanıcısını sil…',
    removeKioskUserTitle: (n: number) => `${n}. koltuğun kiosk kullanıcısı silinsin mi?`,
    removeKioskUserBody:
      'Pulsar Seat bu koltuğun kiosk kabuğu için bir Windows kullanıcısı oluşturdu. Kiosk kabuğu kapatılınca kullanıcı yalnızca devre dışı bırakılır ve dosyaları (kayıtlar, tarayıcı profilleri) korunur. Silmek kullanıcıyı ve bütün dosyalarını kalıcı olarak kaldırır.',
    removeKioskUserDone: 'Koltuğun kiosk kullanıcısı silindi.'
  },

  systemMenu: {
    compare: 'Kayıtlı yapılandırmayla karşılaştır…',
    view: 'Görünüm ayarları…',
    cpu: 'Koltuk başına işlemci çekirdekleri…',
    hotkey: 'Klavye/fare geçiş kısayolu…',
    wizard: 'Kurulum sihirbazı…',
    reset: 'Bütün ayarları sıfırla…',
    import: 'Başka bir yapılandırmayı içe aktar…',
    deviceManager: 'Aygıt Yöneticisi',
    displaySettings: 'Ekran ayarları',
    networkConnections: 'Ağ bağlantıları',
    userAccounts: 'Kullanıcı hesapları',
    soundSettings: 'Ses ayarları',
    panels: 'Windows panelleri',
    clearPictures: 'Bütün özel resimleri temizle',
    logLevel: 'Günlük ayrıntısı…',
    outputs: 'Görüntü çıkışları…'
  },

  kinds: {
    monitor: 'Ekran',
    keyboard: 'Klavye',
    mouse: 'Fare',
    hid: 'Diğer giriş aygıtı',
    audio_render: 'Hoparlör',
    audio_capture: 'Mikrofon',
    webcam: 'Kamera',
    smart_card_reader: 'Akıllı kart okuyucu',
    usb_hub: 'USB göbeği',
    storage: 'Depolama',
    printer: 'Yazıcı',
    serial_port: 'Seri port',
    network: 'Ağ bağdaştırıcısı',
    other: 'Diğer aygıt'
  } as Record<DeviceKind, string>,

  assign: {
    title: (name: string) => `${name}: koltuk ataması`,
    exclusiveHint: 'Bu aygıt aynı anda yalnızca bir koltukta olabilir.',
    shareableHint: 'Bu aygıt birden fazla koltukta kullanılabilir.',
    noSeat: 'Hiçbir koltuk (boşta)',
    allSeats: 'Bütün koltuklar',
    clearAll: 'Hiçbiri',
    seatOption: (n: number, name: string) => (name === String(n) ? `${n}. koltuk` : `${n}. koltuk — ${name}`),
    networkHint: 'Ağ bağdaştırıcıları her zaman ortaktır; yalnızca bilgi için gösterilir.',
    target: 'Neyi atıyorsunuz?',
    targetDevice: 'Yalnızca bu aygıt',
    targetHint: 'Bu atama kapsadığı bütün aygıtlara geçer, sonradan takılanlar da dahil. Klavye, fare gibi tek koltuklu aygıtlar yalnızca tek bir koltuk seçildiğinde bu atamayı izler.'
  },

  seatName: {
    title: (n: number) => `${n}. koltuğun adı`,
    label: 'Ad',
    errors: {
      empty: 'Ad boş olamaz.',
      too_long: 'Ad en fazla 64 karakter olabilir.',
      control: 'Ad görünmeyen denetim karakterleri içeremez.'
    } as Record<string, string>
  },

  account: {
    title: (n: number) => `${n}. koltuğun oturum hesabı`,
    logonScreen: 'Normal oturum açma ekranını göster (önerilen)',
    autoLogon: 'Bu hesapla kendiliğinden oturum aç',
    local: 'Yerel hesap',
    domain: 'Etki alanı hesabı',
    user: 'Kullanıcı',
    domainName: 'Etki alanı',
    userName: 'Hesap adı',
    pickUser: 'Bir kullanıcı seçin',
    openAccounts: 'Kullanıcı hesaplarını aç',
    password: 'Parola',
    confirm: 'Parola (tekrar)',
    mismatch: 'Parolalar aynı değil.',
    emptyOk: 'Parola boş bırakılabilir.',
    keepPassword: 'Kayıtlı parolayı değiştirme',
    passwordSet: 'Parola kayıtlı',
    passwordFailing: 'Kayıtlı parola son oturum açmada reddedildi',
    seatOneWarning: '1. koltukta kendiliğinden oturum açma önerilmez; burada normal oturum açma ekranını kullanın.',
    disabledUser: 'devre dışı',
    adminUser: 'yönetici',
    invalidUser: 'Hesap adı boş olamaz ve \\ / " @ karakterlerini içeremez.',
    invalidDomain: 'Etki alanı adı geçersiz.',
    usersFailed: 'Yerel kullanıcılar alınamadı; adı elle yazabilirsiniz.',
    savedHint: 'Parola hemen ve yalnızca hizmete gönderilir; dosyalara yazılmaz, sonradan görüntülenemez.'
  },

  basic: {
    seatTable: 'Koltuklar',
    colSeat: 'Koltuk',
    colAccount: 'Oturum hesabı',
    colStart: 'Kendiliğinden başlat',
    colWallpaper: 'Koltuk duvar kağıdı',
    colVirtualPads: 'Sanal oyun kolu',
    colGpu: 'Ekran kartı tercihi',
    virtualPadsHint:
      'Sanal oyun kolu: bu koltuğa atanan oyun kolu (PlayStation, Switch Pro, Xbox ya da başka bir HID kolu) için yalnızca bu koltuğun kullanıcısının açabildiği bir sanal kol oluşturulur. ViGEmBus ya da usbip-win2 kurulu olmalıdır (Pulsar Seat bunları kurmaz). “Fiziksel kolları koltuğa kilitle” açıkken gerçek kol yalnızca bu koltukta açılır.',
    colPadIgnore: 'SDL’nin yok sayacağı kollar',
    padIgnoreHint:
      'Bu koltukta SDL kullanan oyunların görmezden geleceği kollar, 0xVVVV/0xPPPP biçiminde, virgülle ayrılmış. Başka koltuklara verilmiş kollar kendiliğinden eklenir.',
    padIgnorePlaceholder: '0x045E/0x0B09',
    colState: 'Şu anki durum',
    logonScreen: 'Oturum açma ekranı',
    seatOneAlways: '1. koltuk her zaman çalışır.',
    startMode: 'Başlatma biçimi',
    behaviour: 'Davranış',
    adminOnly: 'Ayarları yalnızca yöneticiler görebilsin ve değiştirebilsin',
    ownSeat: 'Kullanıcılar kendi koltuklarını yeniden başlatabilsin',
    hideTray: 'Bildirim alanı simgesini gizle',
    askOnStart: 'Koltukları elle başlatmadan önce sor',
    noShowGoodApply: 'Kaydettikten sonra başarı iletisini gösterme',
    lockScreen: 'Pulsar Seat açıkken kilit ekranında koltuk resmini göster',
    sameAccount: 'Aynı hesap birden fazla koltukta',
    sameAllow: 'İzin ver',
    sameWarn: 'İzin ver ama uyar',
    sameDeny: 'İzin verme',
    screensaver: 'Ekran koruyucu ayarları',
    lockScreenSettings: 'Kilit ekranı ayarları',
    theme: 'Görünüm',
    themeSystem: 'Sistem',
    themeLight: 'Açık',
    themeDark: 'Koyu',
    language: 'Dil',
    languageSystem: 'Windows görüntüleme dili',
    languageNames: { en: 'English', tr: 'Türkçe' } as Record<string, string>,
    languageRestart: 'Yönetici seçilen dilde yeniden yüklenir.',
    colApps: 'Başlangıç programları',
    appsNone: 'Yok',
    appsCount: (n: number) => `${n} program`
  },

  startupApps: {
    title: (seat: number) => `${seat}. koltuğun başlangıç programları`,
    hint: 'Koltukta oturum açılınca bu programlar koltuğun işlemci sınırlarıyla başlar. Tam yol yazın (ör. C:\\Oyunlar\\oyun.exe).',
    program: 'Program yolu',
    args: 'Bağımsız değişkenler',
    add: 'Program ekle',
    remove: 'Kaldır',
    notAbsolute: 'Tam yol gerekiyor (sürücü harfiyle ya da \\\\sunucu\\paylaşım ile başlamalı).',
    empty: 'Henüz başlangıç programı yok.'
  },

  gpuPref: {
    title: (seat: number) => `${seat}. koltuk: ekran kartı tercihi`,
    hint: 'Seçilen programlar koltuk kullanıcısı için tercih edilen ekran kartında açılır (Windows grafik ayarlarıyla aynı kayıt). Bu bir tercihtir; programlar öteki ekran kartlarını da görmeye devam eder. Değişiklik programın bir sonraki açılışında geçerli olur.',
    mode: 'Tercih',
    modes: {
      off: 'Kapalı (Windows karar verir)',
      seat_gpu: 'Koltuğun ekranlarını süren ekran kartı',
      high_performance: 'Yüksek performanslı ekran kartı',
      power_saving: 'Güç tasarruflu ekran kartı'
    } as Record<string, string>,
    program: 'Program (tam yol, .exe)',
    add: 'Program ekle',
    remove: 'Kaldır',
    empty: 'Program yok.',
    invalid: 'Tam bir .exe yolu girin (ör. C:\\Oyunlar\\oyun.exe).',
    off: 'Kapalı',
    count: (n: number) => `${n} program`
  },

  kiosk: {
    column: 'Kiosk',
    off: 'Kapalı',
    count: (n: number) => `${n} uygulama`,
    title: (seat: number) => `${seat}. koltuk: kiosk koltuğu`,
    enable: 'Bu koltuk kiosk koltuğu olsun',
    intro:
      'Kiosk koltuğu 1. koltukla aynı Windows oturumunda çalışır; “her kiosk koltuğu kendi Windows kullanıcısıyla çalışsın” açıksa (varsayılan) kendi kullanıcı profili, ayarları ve uygulama verileri olur. Koltuğun ekranlarında yalnızca aşağıdaki uygulamalar açılır. Klavyesi ve faresi yalnızca bu uygulamalara gider; ancak bu, 1. koltuk da kiosk koltuğuysa ya da giriş filtresi etkinse tam olarak geçerlidir.',
    limits:
      'Sınırlar: bütün koltuklar aynı Windows oturumunu ve panoyu paylaşır. Girişi ham giriş, DirectInput veya XInput ile okuyan oyunlar ve uygulamalar koltuğun kendi aygıtlarını yalnızca uygulama başına yalıtım (Gelişmiş sekmesi) açıksa alır. Tam ekran oyunlar için uygulamada “Tam ekran” seçin: oyun öne geçer, ama aynı anda bilgisayarda yalnızca bir oyun gerçek tam ekranda olabilir. Mağaza (UWP / MSIX) uygulamaları uygulama kimliğiyle (AUMID) eklenir; her Windows sürümünde açılmayabilirler. Zaten açık bir kopyaya devreden programlar koltuğun kendi kullanıcısıyla çalışıyorsa koltukta kalır. Ses: kiosk koltuğuna yalnızca ona atanmış bir ses aygıtı verirseniz, koltuğun uygulamaları Windows 10 1803 ve sonrasında uygulama başına varsayılan aygıt olarak bunu kullanır (belgelenmemiş bir Windows arabirimi; Windows güncellemeleri bunu bozabilir, o zaman ses 1. koltuğun varsayılan aygıtından çıkar). Kiosk koltuğu tam bir kilit değildir: Ctrl+Alt+Del ve Win+L Windows tarafından kullanıcı kipindeki her programdan önce işlenir; hangi koltukta basılırsa basılsın bütün koltukları güvenlik ekranına geçirir ve buradan oturumu kapatmak bütün koltukları kapatır. Diğer sistem tuşları (Windows tuşu, Win+R, Win+E, Alt+Tab, Ctrl+Esc, Ctrl+Shift+Esc) kiosk klavyelerinden 1. koltuğun masaüstüne ulaşmaz; giriş filtresi yoksa bu, klavyeleri ayırt eden en iyi çaba yöntemiyle yapılır. Uygulama listesine dönmek için çubuktaki “Uygulamalar” düğmesini veya Ctrl+Alt+Home tuşlarını kullanın.',
    appId: 'Mağaza uygulaması kimliği (AUMID, isteğe bağlı)',
    appIdHint: 'Örnek: Microsoft.WindowsCalculator_8wekyb3d8bbwe!App. Doluysa program yolu gerekmez.',
    notAppId: 'Geçersiz uygulama kimliği (Paket!Uygulama biçiminde olmalı).',
    fullscreen: 'Tam ekran',
    perSeatAccounts: 'Her kiosk koltuğu kendi Windows kullanıcısıyla çalışsın',
    perSeatAccountsHint:
      'Açıkken 2. ve sonraki kiosk koltukları kendi kullanıcısıyla çalışır: koltuğa hesap ve parola kayıtlıysa o hesapla, değilse Pulsar’ın oluşturduğu gizli bir yerel hesapla (PulsarKiosk02 …). Her koltuğun profili, ayarları ve uygulama verileri ayrı olur, ama bu bir güvenlik sınırı değildir: bütün kiosk koltukları tek bir masaüstünü paylaşır, bir koltuktaki program diğerlerinin klavyesini, ekranını ve panosunu okuyabilir. İlk açılışta yeni profil oluşturulur; eskiden 1. koltuğun kullanıcısında kalan uygulama verileri taşınmaz. Kapalıyken bütün kiosk koltukları 1. koltuğun kullanıcısıyla çalışır. Bir koltuk kiosk koltuğu olmaktan çıkınca Pulsar hesabı yalnızca devre dışı bırakılır ve dosyaları kalır (koltuk menüsü: “Koltuğun kiosk kullanıcısını sil…”).',
    account: {
      console: 'Kullanıcı: 1. koltuğun kullanıcısı',
      seat: (user: string) => `Kullanıcı: ${user}`,
      auto: (user: string) => `Kullanıcı: ${user} (Pulsar hesabı)`,
      personalWarning:
        'Bu koltuğun kayıtlı hesabı (kişisel ya da etki alanı hesabı) diğer kiosk koltuklarının paylaştığı bir masaüstünde oturum açar: onların programları bu hesabın klavyesini, ekranını ve panosunu okuyabilir. Pulsar hesabını tercih edin (kayıtlı parolayı kaldırın).'
    },
    appTitle: 'Döşeme adı',
    program: 'Program yolu',
    args: 'Bağımsız değişkenler',
    autostart: 'Koltuk açılınca başlat',
    monitor: 'Ekran (1 = ilk)',
    add: 'Uygulama ekle',
    remove: 'Kaldır',
    empty: 'Henüz uygulama yok.',
    tooMany: 'En çok 16 uygulama eklenebilir.',
    idle: 'İşlem yapılmazsa oturumu kapat (dakika, 0 = kapalı)',
    idleBad: '0 ile 1440 arasında bir sayı girin.',
    notAbsolute: 'Tam yol gerekiyor (sürücü harfiyle ya da \\\\sunucu\\paylaşım ile başlamalı).',
    seatOneDesktop: '1. koltuk normal masaüstü olarak kalsın',
    seatOneDesktopHint:
      'Açıkken kiosk koltukları öne geçmez. Giriş filtresi yoksa Pulsar, kiosk klavyelerinden gelen Windows tuşunu, Alt+Tab, Alt+Esc, Ctrl+Esc ve Ctrl+Shift+Esc tuşlarını aygıta göre ayırıp 1. koltuktan uzak tutar (1. koltuğun kendi klavyesi bunları kullanmaya devam eder); Windows izin verirse kiosk klavyelerinin diğer tuşları ve kiosk farelerinin tıklamaları da 1. koltuğa gitmez. Kiosk farelerinin hareketi 1. koltuğun imlecini yine oynatabilir. 1. koltuğun uygulamaları, uygulama başına yalıtım açıksa kiosk aygıtlarını ham girişte görmez. Giriş filtresi etkinse kiosk koltuklarının girişi hizmet üzerinden yalnızca kendi kabuğuna gider. Kapalıyken 1. koltuk da kiosk koltuğu olur ve kendi uygulama listesini kullanır.',
    systemKeys:
      'Ctrl+Alt+Del (güvenli dikkat dizisi) ve Win+L kullanıcı kipinde engellenemez: hangi klavyeden basılırsa basılsın bütün koltukları etkiler. Diğer Windows sistem tuşları kiosk klavyelerinden 1. koltuğa ulaşmaz (giriş filtresi yoksa aygıta göre ayırma en iyi çaba ile yapılır).',
    backendOnly: 'Kiosk koltukları yalnızca “Eşzamanlı kiosk koltukları” oturum yöntemiyle çalışır.',
    audio: {
      routed: (render: string | null, capture: string | null) =>
        'Ses: ' + [render ? `çıkış ${render}` : null, capture ? `mikrofon ${capture}` : null].filter(Boolean).join(', '),
      noEndpoint: 'Ses: koltuğa ayrı ses aygıtı atanmamış (1. koltuğun varsayılan aygıtı)',
      unsupported: 'Ses: bu Windows sürümünde uygulama başına ses aygıtı yok (1. koltuğun varsayılan aygıtı)',
      sharedApp: (seats: number[]) => `Aynı program ${seats.join(', ')}. koltukta da var: ses aygıtı ayarını paylaşırlar`
    }
  },

  startModes: {
    manual: 'Elle (yalnızca “Koltukları başlat” ile) — en güvenli, başlangıçta önerilir',
    at_boot: 'Windows açılınca',
    after_seat_one_logon: '1. koltukta biri oturum açınca'
  } as Record<StartMode, string>,

  startModeHints: {
    manual: 'Koltuklar yalnızca bu penceredeki ya da bildirim alanındaki düğmeyle başlar.',
    at_boot: 'Bilgisayar açılırken bütün koltuk ekranlarının açık olması gerekir. Ekranı açık olmayan koltuk ekranı gelene kadar bekler.',
    after_seat_one_logon: 'Ekranları açılışta hazır olmayan bilgisayarlar için: koltuklar 1. koltukta ilk oturum açıldıktan sonra başlar.'
  } as Record<StartMode, string>,

  advanced: {
    banner: 'Bu sekme deneyimli kullanıcılar içindir. Emin olmadığınız ayarı değiştirmeyin.',
    backends: 'Çalışma yöntemleri',
    session: 'Oturum yöntemi',
    topology: 'Koltuk sunucularının yeri',
    display: 'Ekran yöntemi',
    input: 'Giriş yöntemi',
    audio: 'Ses yöntemi',
    gates: 'Aygıt erişim denetimi',
    active: 'şu an kullanılıyor',
    selected: 'seçili',
    experimental: 'deneysel',
    unavailableHere: 'bu bilgisayarda kullanılamaz',
    experimentalAcked: 'Deneysel uyarı onaylandı.',
    experimentalOpen: 'Uyarıyı yeniden oku',
    timings: 'Zamanlamalar',
    startDelay: 'İlk koltuk başlamadan önce bekle (ms)',
    betweenSeats: 'Koltuklar arası bekleme (ms)',
    seatTimeout: 'Koltuk başlatma zaman aşımı (sn, 30–120)',
    relogonDelay: 'Oturum kapandıktan sonra yeniden açma gecikmesi (ms)',
    audioWait: 'Ses hizmetini bekleme süresi (sn)',
    monitorIdle: 'Boşta kalan ek koltuğun ekranını kapatma süresi (sn, 0 = kapalı)',
    safety: 'Güvenlik',
    uncleanGuard: 'Windows düzgün kapanmadıysa ek koltukları kendiliğinden başlatma',
    crashLimit: 'Hizmet art arda bu kadar kez çökerse güvenli kipte kal',
    hotkey: 'Klavye/fare geçiş kısayolu',
    hotkeyOff: 'Kapalı',
    hotkeyChange: 'Değiştir…',
    systemButtons: 'Ek koltuklardaki güç ve uyku tuşları bütün bilgisayarı etkileyebilsin',
    padDeviceScope: 'Fiziksel oyun kollarını koltuğa kilitle',
    padDeviceScopeHint:
      'Bir koltuğa verilen kol yalnızca o koltuğun kullanıcısı tarafından açılabilir; uzak koltuklar çalışırken diğer kollar yalnızca bu bilgisayarın başındaki kullanıcıya kalır. Varsayılan olarak kapalıdır (sistem değişikliği): kolların aygıt düğümlerinin güvenlik ayarını değiştirir ve onları bağlı oldukları aygıtla birlikte yeniden başlatır (kolu içinde olan bir kulaklık ya da klavye de yeniden başlar). Değişiklikler hizmet durunca ya da bu kapatılınca geri alınır.',
    devices: 'Aygıtlar',
    adminsBypass: 'Yöneticiler bütün koltukların aygıtlarına erişebilsin',
    classDefaults: 'Sınıf düzeyinde varsayılan erişim tanımları (sistem değişikliği)',
    classDefaultsSoon: 'Bu ayar Windows’un aygıt sınıfı varsayılan erişim tanımlarını değiştirir. Gerçek donanımda denenene kadar kapalı tutuluyor.',
    classDefaultClasses: 'Bu ayar açıldığında kapsanacak aygıt sınıfları',
    classTargets: { disk_drive: 'Diskler', cdrom: 'CD / DVD sürücüleri', volume: 'Birimler', ports: 'Seri ve paralel bağlantı noktaları' } as Record<string, string>,
    tweaks: 'İsteğe bağlı sistem ayarları',
    tweaksHint: 'Her biri kayda geçirilir ve Pulsar Seat kapatılınca ya da kaldırılınca geri alınır.',
    hideFus: 'Hızlı kullanıcı değiştirmeyi gizle',
    hideFusConflict:
      'Seçili oturum yöntemi (“Gerçek oturumlar, sırayla”) hızlı kullanıcı değiştirmeye dayanır; bu ayar o yöntemi kapatacağı için şu an kullanılamaz.',
    excludeDrivers: 'Kalite güncellemelerinde sürücü güncellemelerini dışarıda bırak',
    palmCheck: 'Dokunmatik yüzey avuç algılama eşiği',
    palmCheckOff: 'Windows varsayılanı',
    cpu: 'İşlemci çekirdekleri',
    cpuOpen: 'Koltuk başına çekirdekleri düzenle…',
    inputNotIsolated: 'Bu düzende diğer koltukların klavye girişleri 1. koltuğun öndeki penceresine de ulaşabilir. Tam yalıtım için “Ayrılmış sunucu oturumu” düzenini seçin ya da imzalı giriş sürücüsünü bekleyin.',
    gpuHint: 'Bir koltukta birden fazla ekran olabilir ve koltuklar aynı ekran kartını paylaşabilir.',
    loopbackTitle: 'Parolasız koltuklar için ayrı bağlantı noktası',
    loopbackEnabled: 'Parolası kayıtlı olmayan koltuklar için ayrı bir Uzak Masaüstü bağlantı noktası kullan',
    loopbackHint:
      'Bu bağlantı noktasında ağ düzeyinde kimlik doğrulaması kapalıdır; yalnızca bu bilgisayarın içinden bağlanılabilir (güvenlik duvarı kuralıyla). Güvenlik duvarı kapalıysa ya da bir ilke yerel kuralları yok sayıyorsa kullanılmaz. Ayar değişince Uzak Masaüstü Hizmetleri, açık Uzak Masaüstü koltuğu kalmadığında yeniden başlatılır.',
    loopbackPort: 'Bağlantı noktası (1024–65535, 3389 olamaz)',
    loopbackAccounts: 'Bu bağlantı noktasından oturum açabilecek ek hesaplar (her satıra bir: kullanici ya da ETKİ_ALANI\\kullanici)',
    loopbackAccountsHint: 'Koltuklara atanmış hesaplar her zaman izinlidir. Yöneticiler bu bağlantı noktasından oturum açamaz.',
    loopbackAllowed: (list: string) => `Şu hesaplar bu bağlantı noktasından oturum açabilir: ${list}.`,
    loopbackNoAccounts: (seats: string) =>
      `Oturum açma ekranındaki koltuklar (${seats}) için izinli hesap yok; bu koltuklar başlayamaz. Yukarıya bu koltuklarda oturum açacak hesapları yazın.`
  },

  backendNames: {
    rds_loopback: 'Eşzamanlı tam oturumlar (yalnızca Windows Server x64, Uzak Masaüstü Oturumu Ana Bilgisayarı)',
    termsrv_wrapper: 'İstemci Windows’ta çoklu oturum (deneysel)',
    fast_user_switch: 'Gerçek oturumlar, sırayla (hızlı kullanıcı değiştirme, aynı anda bir koltuk)',
    desktops: 'Eşzamanlı kiosk koltukları (tek oturumda ayrı masaüstleri)',
    seat_one_console: '1. koltuğun kendi oturumunda (varsayılan)',
    dedicated_host: 'Ayrılmış sunucu oturumunda (tam giriş yalıtımı)',
    window_presenter: 'Tam ekran pencere (her yerde çalışır)',
    display_core: 'Ayrılmış ekran çıkışı',
    pulsar_idd: 'Pulsar sanal ekran sürücüsü',
    console_only: 'Yalnızca konsol',
    raw_input_host: 'Ham giriş, kullanıcı kipi',
    kmdf_filter: 'Pulsar giriş süzgeç sürücüsü',
    none: 'Kapalı',
    rdp_bridge: 'Koltuğun kendi ses aygıtına yönlendir',
    console_shared: 'Bütün sesler 1. koltuğun aygıtlarından',
    device_security: 'Aygıt güvenlik tanımları',
    session_drive_letters: 'Oturuma özel sürücü harfleri',
    camera_consent: 'Kamera izinleri',
    storage_minifilter: 'Depolama süzgeç sürücüsü',
    printer_acl: 'Yazıcı kuyruğu izinleri'
  } as Record<string, string>,

  availability: {
    available: 'Kullanılabilir',
    needs_setup: 'Kurulum gerekiyor',
    driver_not_signed: 'Sürücü henüz imzalı değil, yüklenmez',
    driver_version_mismatch: 'Sürücü sürümü uyuşmuyor; Pulsar Seat’i güncelleyin',
    unavailable: 'Kullanılamaz'
  } as Record<string, string>,

  unavailableReasons: {
    edition_unsupported: 'Bu Windows sürümü desteklemiyor',
    arch_unsupported: 'Bu işlemci mimarisinde yok',
    rds_not_ready: 'Uzak Masaüstü Oturumu Ana Bilgisayarı rolü hazır değil',
    no_wrapper: 'Kurulu bir sarmalayıcı bulunamadı',
    not_native: 'Sarmalayıcı bu işlemci için derlenmemiş',
    not_implemented: 'Bu sürümde henüz yok',
    not_built: 'Bu derlemede yok',
    feature_not_built: 'Bu derlemede yok',
    os_unsupported: 'Bu Windows sürümünde çalışmaz',
    not_client_sku: 'Yalnızca istemci Windows sürümlerinde',
    rdsh_role_missing: 'Uzak Masaüstü Oturumu Ana Bilgisayarı rolü kurulu değil',
    rdp_denied_by_policy: 'Uzak Masaüstü bağlantıları ilkeyle engellenmiş',
    foreign_service_dll: 'Uzak Masaüstü hizmeti Windows varsayılanını kullanmıyor',
    unknown_termsrv_build: 'Uzak Masaüstü hizmetinin bu derlemesi tanınmıyor',
    wrapper_missing: 'Kurulu bir sarmalayıcı bulunamadı',
    reference_product_present: 'Başka bir çoklu koltuk yazılımı kurulu',
    audio_service_not_running: 'Windows ses hizmeti çalışmıyor',
    display_core_unavailable: 'Ayrılmış ekran çıkışı bu bilgisayarda yok',
    experimental_not_acknowledged: 'Deneysel yöntem uyarısı onaylanmadı',
    fsu_disabled: 'Hızlı kullanıcı geçişi bir ilkeyle kapatılmış',
    console_not_ready: 'Konsol oturumu henüz hazır değil'
  } as Record<string, string>,

  /** `Availability::NeedsSetup.what` codes (the service sends English-free ids). */
  needsSetup: {
    specialized_display: 'ekranın ayrılmış çıkış olarak ayarlanması gerekiyor',
    enable_remote_desktop: 'Uzak Masaüstü’nün açılması gerekiyor',
    termservice_start: 'Uzak Masaüstü hizmetinin başlatılması gerekiyor',
    enable_termsrv_wrapper: 'kurduğunuz sarmalayıcının etkinleştirilmesi gerekiyor',
    console_sign_in: '1. koltukta bir kullanıcının oturum açması gerekiyor'
  } as Record<string, string>,

  fallback: {
    instead: (wanted: string, used: string) => `${wanted} yerine ${used} kullanılıyor`,
    notUsable: (wanted: string) => `${wanted} kullanılamıyor`,
    forcedByFus: (used: string) => `${used} kullanılıyor (hızlı kullanıcı değiştirme etkin)`,
    unknownReason: (code: string) => `bilinmeyen neden (${code})`,
    gate: 'Aygıt erişim denetimi'
  },

  /** Text per pre-flight check id and outcome; the service's `detail` is for logs. */
  checkText: {
    admin: { pass: 'Yönetici olarak çalışıyor.', fail: 'Yönetici hakları gerekiyor.' },
    os_build: {
      pass: 'Windows sürümü destekleniyor.',
      warn: 'Windows sürümü, denenmiş en yeni sürümden daha yeni.',
      fail: 'Windows sürümü çok eski.'
    },
    arch_native: {
      pass: 'Bu paket bilgisayarın işlemcisi için derlenmiş.',
      warn: 'Bilgisayarın işlemci mimarisi belirlenemedi.',
      fail: 'Bu paket başka bir işlemci mimarisi için; doğru paketi kurun.'
    },
    edition: { info: 'Windows baskısı bildirildi.' },
    rdsh_role: {
      pass: 'Rol kurulu.',
      warn: 'Rol bulunamadı; koltuklar hızlı kullanıcı değiştirmeye geri döner.',
      info: 'Rol durumu bilinmiyor.'
    },
    secure_boot: { pass: 'Açık.', info: 'Kapalı ya da bilinmiyor (yalnızca bildirilir; Pulsar Seat bunu asla değiştirmez).' },
    memory_integrity: { pass: 'Açık.', info: 'Kapalı ya da bilinmiyor (yalnızca bildirilir; Pulsar Seat bunu asla değiştirmez).' },
    smart_app_control: { info: 'Akıllı Uygulama Denetimi durumu bildirildi; açıksa imzasız derlemeler engellenebilir.' },
    windows_safe_mode: { pass: 'Normal açılış.', warn: 'Windows Güvenli Mod’da: ek koltuklar başlatılmaz.' },
    driver_signing: { info: 'Pulsar Seat sürücüleri henüz imzalı değil; kullanıcı kipi yöntemleri kullanılır.' },
    reference_product: { pass: 'Kurulu değil.', fail: 'Başka bir çoklu koltuk yazılımı kurulu.' },
    termservice_stock: { pass: 'Uzak Masaüstü hizmeti Windows’un kendi dosyasını kullanıyor.', warn: 'Uzak Masaüstü hizmeti Windows varsayılanını kullanmıyor.' },
    foreign_input_filters: { pass: 'Yok.', warn: 'Başka giriş süzgeçleri var (dokunulmadı).' }
  } as Record<string, Partial<Record<CheckOutcome, string>>>,
  technicalDetail: 'teknik ayrıntı',

  notes: {
    input_not_isolated: 'Giriş tam yalıtılmıyor',
    seat_one_sees_other_endpoints: '1. koltuk diğer koltukların ses aygıtlarını da görür',
    display_fallback: 'Ekran tam ekran pencereyle gösteriliyor (1. koltuktan gizli değil)',
    audio_console_shared: 'Ses 1. koltuğun aygıtlarından çıkıyor',
    same_account_warning: 'Bu hesap başka bir koltukta da açık',
    one_seat_at_a_time: 'Aynı anda yalnızca bir koltuk etkin',
    fus_inactive_frozen:
      'Sırası olmayan koltuklar donar: ekranları, girişleri ve sesleri yoktur, programları arka planda çalışır. Ekran kartı kullanan programlar (oyunlar, 3B) koltuk geri geldiğinde aygıtlarını kaybedip kapanabilir',
    fus_audio_per_user: 'Her koltuğun kullanıcısı varsayılan ses aygıtını kendi oturumunda bir kez seçer (Windows bunu kullanıcı başına saklar)',
    kiosk_audio_shared: 'Kiosk koltuklarındaki uygulamaların sesi 1. koltuğun varsayılan ses aygıtından çıkar',
    kiosk_audio_per_app: 'Kiosk koltuklarındaki uygulamalar, koltuğa atanmış ses aygıtını uygulama başına varsayılan aygıt olarak kullanır (belgelenmemiş Windows arabirimi)',
    no_user_isolation: 'Koltuklar arasında kullanıcı yalıtımı yok',
    activity_needs_console_listener: 'Konsol dinleyicisi şu an çalışmıyor: etkinlik algılanmıyor',
    hotkey_needs_filter: 'Kısayol yalnızca konsol dinleyicisi ya da giriş süzgeciyle çalışır',
    gamepads_shared: 'Oyun kumandaları tüm koltuklarca paylaşılır',
    xinput_pads_shared: 'XInput oyun kumandaları tüm koltuklarca paylaşılır',
    touch_emulated_as_mouse: 'Dokunma fare olarak aktarılır',
    driver_not_signed: 'Sürücü imzalı değil',
    input_not_routed: 'Giriş aygıtları koltuklara yönlendirilmiyor',
    rdp_listener_on_all_interfaces: 'Uzak Masaüstü bütün ağ bağdaştırıcılarında dinliyor (ağ düzeyinde kimlik doğrulaması açık)',
    // Redirection notes (seat_rdp::policy, HostRedirection.notes).
    clipboard_shared_with_host_session:
      'Pano, ana oturumun panosu üzerinden aktarılır: 1. koltuğun kullanıcısı ve panosu açık diğer koltuklar da bu panoyu kullanır',
    no_seat_drives: 'Bu koltuğa ait yönlendirilebilecek sürücü yok',
    drive_without_global_letter: 'Koltuğa ait bir sürücünün harfi yalnızca koltuğun oturumunda görünüyor; bu sürücü yönlendirilmez',
    drive_acl_volume_refused:
      'NTFS gibi izinli dosya sistemleri yönlendirilmez (yalnızca FAT32 / exFAT); koltuk oturumu bu sürücüyü doğrudan görür',
    drive_letter_moved: 'Bir sürücü harfi artık başka bir birimi gösteriyor; bu sürücü yönlendirilmez',
    drive_volume_unknown: 'Bir sürücünün birimi belirlenemedi; bu sürücü yönlendirilmez',
    no_seat_readers: 'Bu koltuğa atanmış akıllı kart okuyucu yok',
    no_seat_printers: 'Bu koltuğa atanmış ve kuyruğu bulunan yazıcı yok',
    printers_all_queues: 'Bilgisayardaki bütün yazıcılar bu koltuğa yönlendirilir (başka koltuklarınkiler de)',
    printers_print_as_host: 'Yönlendirilen yazıcılara işler koltuğun arka plan hesabıyla gönderilir, koltuk kullanıcısı adına değil',
    // Backend probe / apply notes (seat-session, seat-media, seat-service).
    no_per_user_isolation: 'Bütün koltuklar aynı Windows kullanıcısıyla çalışır (kullanıcı yalıtımı yok)',
    one_kiosk_visible_at_a_time: 'Aynı anda yalnızca bir kiosk masaüstü görünür',
    kiosk_shell_seat_aware_apps_only:
      'Kiosk koltuklarında giriş pencere iletileriyle gelir; ham giriş, DirectInput / XInput kullanan oyunlar koltuğun aygıtlarını yalnızca uygulama başına yalıtım açıksa alır; tam ekran ve mağaza (UWP) uygulamaları en iyi çaba ile desteklenir',
    kiosk_per_seat_accounts: 'Kiosk koltukları kendi Windows kullanıcılarıyla çalışabilir (2. koltuktan başlayarak): bu dosyalarını ve ayarlarını ayırır, güvenliklerini değil',
    kiosk_users_share_desktop:
      'Kiosk koltukları tek bir Windows masaüstünü paylaşır: bir koltuktaki program diğerlerinin ekranını görebilir, klavyesini okuyabilir, panosunu ve pencerelerini kullanabilir. Bir kiosk koltuğunda kişisel ya da iş hesabıyla oturum açmayın.',
    kiosk_account_fallback: 'Bir kiosk koltuğunun kendi kullanıcısı açılamadı; o koltuk 1. koltuğun kullanıcısıyla çalışıyor (hizmet günlüğüne bakın)',
    no_privacy_from_console: 'Koltuğun görüntüsü 1. koltuktan gizlenemez',
    audio_off: 'Ses kapalı',
    shared_fallback_unconfigured: 'Ortak ses aygıtı seçilmemiş; koltuk sessiz kalabilir',
    using_shared_fallback: 'Koltuk ortak ses aygıtını kullanıyor',
    session_reset_on_timeout: 'Bağlantısı kopan oturumlar bir süre sonra kapatılıyor (ilke ayarı)',
    hardware_gpu_not_enabled: 'Uzak Masaüstü oturumları ekran kartını kullanmıyor (ilke ayarı kapalı)',
    rdp_disabled_until_prepare: 'Uzak Masaüstü kapalı; hazırlık adımı açacak',
    rds_licensing_not_configured: 'Uzak Masaüstü lisanslaması ayarlanmamış; deneme süresi bitince oturumlar açılmaz',
    two_admin_sessions_only: 'Bu Windows Server yalnızca iki yönetici oturumuna izin verir (Uzak Masaüstü Oturum Ana Bilgisayarı rolü yok)',
    termservice_restarted: 'Uzak Masaüstü Hizmetleri yeniden başlatıldı',
    loopback_listener_created: 'Parolasız koltuk bağlantı noktası oluşturuldu',
    loopback_listener_updated: 'Parolasız koltuk bağlantı noktası güncellendi',
    loopback_listener_firewall_rule: 'Parolasız koltuk bağlantı noktası için güvenlik duvarı kuralı eklendi',
    loopback_listener_old_rule_removed: 'Eski güvenlik duvarı kuralı kaldırıldı',
    termsrv_wrapper_enabled: 'Uzak Masaüstü sarmalayıcısı etkinleştirildi',
    wrapper_managed_externally: 'Uzak Masaüstü sarmalayıcısını başka bir program yönetiyor; Pulsar Seat dokunmaz'
  } as Record<string, string>,

  redirect: {
    column: 'Yönlendirme',
    off: 'Kapalı',
    title: (seat: number) => `${seat}. koltuğun yönlendirmesi`,
    hint: 'Bu ayarlar yalnızca IronRDP koltuk istemcisinde tam uygulanır. ActiveX istemcisi süzemez: panoyu yalnızca her şey açıkken aktarır, sürücü ve akıllı kartı hiç yönlendirmez.',
    seatOne: '1. koltuk ana oturumda çalışır; yönlendirme gerekmez.',
    clipboard: 'Pano',
    directions: {
      off: 'Kapalı',
      to_seat: 'Koltuğa (ana oturumdan koltuğa yapıştırma)',
      from_seat: 'Koltuktan (koltuktan ana oturuma kopyalama)',
      both: 'İki yönlü'
    } as Record<string, string>,
    short: { off: 'kapalı', to_seat: 'koltuğa', from_seat: 'koltuktan', both: 'iki yönlü' } as Record<string, string>,
    formats: 'Aktarılan biçimler',
    text: 'Metin',
    html: 'HTML ve zengin metin',
    images: 'Resim',
    files: 'Dosya',
    crossSeat: 'Başka koltuklarda kopyalanan veriler de bu koltuğa gelebilsin',
    sharedWarning:
      'Uyarı: pano ana oturumun (1. koltuğun) panosu üzerinden aktarılır. 1. koltuğun kullanıcısı ve panosu açık olan diğer koltuklar bu verileri görebilir.',
    drives: 'Sürücüler',
    driveModes: { off: 'Kapalı', read_only: 'Salt okunur', read_write: 'Okuma-yazma' } as Record<string, string>,
    drivesHint:
      'Yalnızca bu koltuğa ait, FAT32 / exFAT biçimli sürücüler (ör. USB bellek) yönlendirilir. NTFS sürücüler izinleri koltuk kullanıcısına göre denetlenemediği için yönlendirilmez.',
    smartcards: 'Akıllı kart',
    smartcardsLabel: 'Bu koltuğa atanmış akıllı kart okuyucularını koltuk oturumuna aç',
    smartcardsHint: 'Koltuk oturumu yalnızca kendi okuyucularını görür. Aynı modelden iki okuyucu varsa okuyucu kimliği okunamazsa ikisi de kapalı kalır.',
    printers: 'Yazıcılar',
    printerModes: {
      off: 'Kapalı',
      seat: 'Bu koltuğa atanmış yazıcılar',
      all: 'Bilgisayardaki bütün yazıcılar'
    } as Record<string, string>,
    printersHint:
      'Koltuk oturumu bu bilgisayarın yazıcılarını zaten doğrudan görebilir; yönlendirilen yazıcı oturumda ayrıca “yönlendirildi” ekiyle görünür. Yazdırma işi koltuk kullanıcısı adına değil, koltuğun arka plan hesabıyla gönderilir. Değişiklik koltuğun bir sonraki bağlantısında geçerli olur.',
    printersAllWarning: 'Uyarı: başka koltuklara ait yazıcılar da bu koltukta görünür ve kullanılabilir.',
    printersShort: { off: '', seat: 'Yazıcı: koltuğun', all: 'Yazıcı: hepsi' } as Record<string, string>,
    summary: (clip: string, drives: string, cards: boolean, printers = '') =>
      [
        clip !== 'kapalı' ? `Pano ${clip}` : '',
        drives !== 'Kapalı' ? `Sürücü: ${drives.toLocaleLowerCase('tr')}` : '',
        cards ? 'Akıllı kart' : '',
        printers
      ]
        .filter(Boolean)
        .join(' · ') || 'Kapalı',
    refusals: 'Son reddedilen yönlendirmeler',
    channels: { clipboard: 'Pano', drives: 'Sürücü', smartcard: 'Akıllı kart', printers: 'Yazıcı', activex: 'ActiveX istemcisi' } as Record<string, string>
  },

  /** Text per refused redirection reason (seat host, `RefusalLog`). */
  refusalReasons: {
    direction_to_seat_off: 'koltuğa aktarma kapalı',
    direction_from_seat_off: 'koltuktan aktarma kapalı',
    format_not_allowed_to_seat: 'bu biçimin koltuğa aktarılmasına izin yok',
    format_not_allowed_from_seat: 'bu biçimin koltuktan aktarılmasına izin yok',
    format_not_offered: 'istenen biçim sunulmamıştı',
    paste_from_seat_refused: 'koltuktan yapıştırma reddedildi',
    files_to_seat_off: 'dosya aktarımı koltuğa kapalı',
    files_from_seat_off: 'dosya aktarımı koltuktan kapalı',
    clipboard_from_other_seat: 'veri başka bir koltukta kopyalanmıştı',
    stale_clipboard_response: 'önceki bağlantıya ait pano yanıtı atıldı',
    clipboard_backend_missing: 'pano başlatılamadı',
    reader_not_assigned: 'okuyucu bu koltuğa ait değil',
    reader_name_ambiguous: 'okuyucu adı birden çok okuyucuda var; kimliği okunamadı',
    reader_admin_refused: 'okuyucu ayarlarını değiştirme izni yok',
    reader_source_missing: 'akıllı kart hizmeti kullanılamıyor',
    smartcard_off: 'akıllı kart yönlendirmesi kapalı',
    list_readers_failed: 'okuyucular listelenemedi',
    drive_letter_moved: 'sürücü harfi artık başka bir birimi gösteriyor',
    drive_volume_unknown: 'sürücünün birimi belirlenemedi',
    drive_acl_volume_refused: 'izinli dosya sistemi (NTFS) yönlendirilmez',
    drive_unassigned: 'sürücü artık bu koltuğa ait değil; oturumdan çıkarıldı',
    drive_is_system_drive: 'Windows sürücüsü yönlendirilmez',
    drive_root_invalid: 'geçersiz sürücü kökü',
    drive_root_unavailable: 'sürücü açılamadı',
    printer_not_redirected: 'yazıcı bu koltuğa yönlendirilmemiş',
    no_printer_queue_found: 'yönlendirilecek yazıcı kuyruğu bulunamadı',
    printer_discovery_failed: 'yazıcılar okunamadı',
    port_not_redirected: 'bağlantı noktası yönlendirilmez',
    clipboard_filter_needs_ironrdp: 'ActiveX istemcisi panoyu süzemez; pano kapalı',
    drives_need_ironrdp: 'ActiveX istemcisi sürücüleri yönlendirmez',
    smartcards_need_ironrdp: 'ActiveX istemcisi akıllı kartları yönlendirmez',
    printers_need_ironrdp: 'ActiveX istemcisi yazıcıları yönlendirmez'
  } as Record<string, string>,

  drivers: {
    kbd_mou_filter: 'Klavye/fare süzgeci',
    hid_filter: 'HID süzgeci',
    idd: 'Sanal ekran sürücüsü',
    storage_minifilter: 'Depolama süzgeci',
    source_ip_callout: 'Kaynak IP sürücüsü'
  } as Record<string, string>,

  driverStatus: {
    not_installed: 'Kurulu değil',
    installed_not_loaded: 'Kurulu, yüklenmedi',
    loaded: 'Yüklü',
    version_mismatch: 'Sürüm uyuşmuyor'
  } as Record<string, string>,

  statusTab: {
    service: 'Hizmet',
    version: 'Sürüm',
    managerVersion: 'Yönetici sürümü',
    arch: 'Mimari',
    revision: 'Yapılandırma sürümü',
    seats: 'Koltukların durumu',
    session: 'Oturum',
    since: 'Durum değişimi',
    fallback: 'Geri dönüş notları',
    backends: 'Çalışma yöntemleri',
    drivers: 'Pulsar sürücüleri',
    checks: 'Ön denetimler',
    runChecks: 'Denetimleri çalıştır',
    noChecks: 'Henüz denetim çalıştırılmadı.',
    caller: 'Bu pencere',
    admin: 'Yönetici (yükseltilmiş)',
    notAdmin: 'Standart kullanıcı',
    degraded: 'Kısıtlar'
  },

  checks: {
    os_build: 'Windows sürümü',
    secure_boot: 'Güvenli Önyükleme',
    memory_integrity: 'Bellek Bütünlüğü',
    edition: 'Windows baskısı',
    rdsh_role: 'Uzak Masaüstü Oturumu Ana Bilgisayarı rolü',
    termservice_stock: 'Uzak Masaüstü hizmeti özgün',
    reference_product: 'Başka bir çoklu koltuk yazılımı',
    foreign_input_filters: 'Başka giriş süzgeç sürücüleri',
    arch_native: 'Yerel derleme',
    admin: 'Yönetici hakları',
    smart_app_control: 'Akıllı Uygulama Denetimi',
    windows_safe_mode: 'Windows Güvenli Mod',
    driver_signing: 'Sürücü imzası'
  } as Record<string, string>,

  outcomes: {
    pass: 'Uygun',
    warn: 'Uyarı',
    fail: 'Sorun',
    info: 'Bilgi'
  } as Record<CheckOutcome, string>,

  conflicts: {
    title: 'Uygulanamayan atamalar',
    body: 'Aşağıdaki aygıtların ataması olduğu gibi uygulanamıyor. Yine de kaydedebilir ya da vazgeçip düzeltebilirsiniz.',
    saveAnyway: 'Yine de kaydet'
  },

  saved: {
    ok: 'Ayarlar kaydedildi.',
    restartSeats: (seats: string) => `Değişikliğin geçerli olması için şu koltuklar yeniden başlatılmalı: ${seats}.`,
    reboot: 'Bu değişiklik bilgisayar yeniden başlatılınca geçerli olur.',
    errors: 'Kaydedilemedi: önce aşağıdaki sorunları düzeltin.',
    movedBeyond: 'Var olmayan koltuklardaki aygıtlar boşa çıkarıldı. Kaydetmeyi unutmayın.'
  },

  unsaved: {
    title: 'Kaydedilmemiş değişiklikler',
    body: 'Yaptığınız değişiklikler henüz kaydedilmedi.',
    save: 'Kaydet',
    discard: 'Kaydetme',
    cancel: 'Vazgeç'
  },

  enable: {
    enableTitle: 'Pulsar Seat açılsın mı?',
    enableBody: 'Pulsar Seat açıldığında seçilen başlatma biçimine göre ek koltuklar çalışır. Açmadan önce bir tanı raporu kaydedilir.',
    disableTitle: 'Pulsar Seat kapatılsın mı?',
    disableBody: 'Çalışan ek koltuklar kapanır; açık oturumlardaki kaydedilmemiş işler kaybolabilir.',
    rebootTitle: 'Yeniden başlatma gerekiyor',
    rebootBody: 'Değişikliğin tamamlanması için Windows yeniden başlatılmalı. Şimdi mi yeniden başlatılsın?',
    rebootNow: 'Şimdi yeniden başlat',
    rebootFailed: 'Bilgisayar kendiliğinden yeniden başlatılamadı. Lütfen elle yeniden başlatın.',
    offerEnable: 'Koltuklar ayarlandı ama Pulsar Seat kapalı. Şimdi açıp koltukları başlatmak ister misiniz?',
    offerEnableButton: 'Aç ve başlat'
  },

  start: {
    confirmTitle: 'Koltuklar başlatılsın mı?',
    confirmBody: 'Kendiliğinden başlatılacak olarak işaretli bütün koltuklar sırayla başlatılır.',
    dontAsk: 'Bir daha sorma',
    disabled: 'Koltukları başlatmak için önce Pulsar Seat’i açın.',
    queued: 'Koltuklar başlatılıyor…',
    safeMode: 'Pulsar Seat güvenli modda; koltuklar başlatılamaz.',
    noStatus: 'Hizmetin durumu henüz bilinmiyor; biraz sonra yeniden deneyin.'
  },

  inputMoveDialog: {
    title: 'Klavye ve fare taşındı',
    body: (seat: number) =>
      `Geçiş kısayoluyla bir klavye (ve aynı göbekteki fareler) ${seat}. koltuğa taşındı. Taşıma şimdilik geçerli; kalıcı olması için kaydedin. Vazgeçerseniz aygıtlar kayıtlı koltuklarına döner.`,
    save: 'Kaydet',
    discard: 'Vazgeç'
  },

  sameAccountDialog: {
    title: 'Aynı hesap başka bir koltukta açık',
    body: 'Bu hesapla başka bir koltukta da oturum açılmış. Aynı hesabı birden fazla koltukta kullanmak önerilmez: dosyalar ve ayarlar birbirini bozabilir.',
    dontShow: 'Bir daha gösterme',
    signOut: 'Oturumu kapat',
    keep: 'Açık kalsın'
  },

  remove: {
    title: (n: number) => `${n}. koltuk silinsin mi?`,
    body: 'Bu koltuğun aygıtları boşa çıkarılır. Değişiklik kaydedilince geçerli olur.'
  },

  reset: {
    title: 'Bütün ayarlar sıfırlansın mı?',
    body: 'Koltuklar, aygıt atamaları, kayıtlı parolalar ve isteğe bağlı sistem ayarları varsayılana döner. Bu işlem geri alınamaz.',
    confirm2Title: 'Emin misiniz?',
    confirm2Body: 'Son onay: bütün Pulsar Seat ayarları silinecek.',
    confirm: 'Sıfırla',
    done: 'Ayarlar sıfırlandı.'
  },

  quit: {
    title: 'Yönetici kapatılsın mı?',
    body: 'Pencere kapatılınca yönetici bildirim alanında çalışmayı sürdürür. Programdan tamamen çıkmak istiyor musunuz? Koltuklar çalışmaya devam eder.',
    confirm: 'Çık'
  },

  tray: {
    hidden: 'Yönetici bildirim alanında çalışmaya devam ediyor. Tamamen çıkmak için simgenin menüsünden “Çıkış”ı seçin.'
  },

  view: {
    title: 'Koltuklar sekmesinin görünümü',
    scaleIcons: 'Aygıt resimlerini ekran boyutuna göre küçült',
    evenRows: 'Koltuk kutularını eşit satırlara diz',
    flashSeconds: 'Yeni takılan aygıtın vurgulanma süresi (sn)',
    showAllHubs: 'Bütün USB göbeklerini göster',
    showAllHubsHint: 'Kapalıyken boş göbekler ve yalnızca başka göbek taşıyan göbekler gizlenir.',
    sharedInSeats: 'Ortak aygıtları koltuk kutularında da göster',
    showIds: 'Aygıt adlarının yanında donanım kimliklerini göster',
    visibleSeats: 'Gösterilen koltuklar'
  },

  compare: {
    title: 'Kayıtlı yapılandırmayla karşılaştırma',
    matches: 'Takılı aygıtlar kayıtlı yapılandırmayla uyuşuyor.',
    changed: 'Yapılandırma son kayıttan bu yana değişti.',
    added: 'Kayıttan sonra eklendi',
    missing: 'Takılı değil (kayıttan sonra çıkarıldı)',
    maskChanged: 'Ataması değişti',
    was: (v: string) => `önceden: ${v}`,
    now: (v: string) => `şimdi: ${v}`,
    restore: 'Kayıtlı atamaya dön',
    newMonitorsSpare: 'Yeni ekranları boşa çıkar',
    moveAssignment: 'Atamayı yeni porta taşı',
    movedFrom: 'Bu aygıt başka bir porta takılmış görünüyor.'
  },

  cpu: {
    title: 'Koltuk başına işlemci çekirdekleri',
    hint: 'İşaretsiz koltuk bütün çekirdekleri kullanır. Her koltuğa en az bir çekirdek kalmalı.',
    hard: 'Katı sınır (tek işlemci grubu)',
    performance: 'performans',
    efficiency: 'verimlilik',
    core: (i: number) => `Çekirdek ${i}`,
    group: (g: number) => `Grup ${g}`,
    all: 'Hepsi',
    failed: 'İşlemci bilgisi alınamadı.',
    oneGroup: 'Katı sınırda seçilen çekirdekler tek bir işlemci grubunda olmalı.',
    seatWide: 'Bütün oturum',
    seatWideHint:
      '“Bütün oturum” kapalıyken çekirdekler koltuğun masaüstüne (Gezgin ve ondan başlatılanlar, Başlat menüsü) ve Pulsar Seat’in başlattığı uygulamalara uygulanır. Açıkken koltuğun oturumundaki diğer her yeni program da (yönetici olarak çalıştırılanlar dahil) bir saniye içinde bunları tercih ettiği çekirdekler olarak alır; korumalı sistem işlemlerine dokunulmaz.'
  },

  hotkey: {
    title: 'Klavye/fare geçiş kısayolu',
    body: 'Bu kısayola basan klavye ve onunla aynı koltuktaki fareler bir sonraki koltuğa geçer. Değiştiricisi her zaman Ctrl’dir.',
    warning: 'Bütün sistemde geçerli bir kısayolu dikkatle seçin: başka programların kullandığı bir tuş seçmeyin.',
    press: 'Alana tıklayıp bir tuşa basın',
    invalid: 'Bu tuş kullanılamaz.',
    off: 'Kısayolu kapat',
    current: 'Seçilen'
  },

  diagnostics: {
    title: 'Tanı raporu',
    body: 'Rapor bu bilgisayara kaydedilir; hiçbir yere gönderilmez. Parolalar asla rapora girmez.',
    maskUsers: 'Kullanıcı adlarını gizle (önerilen)',
    create: 'Raporu oluştur',
    done: (path: string) => `Rapor kaydedildi: ${path}`
  },

  import: {
    title: 'Başka bir yapılandırmayı içe aktar',
    body: 'Başka bir çoklu koltuk yazılımının dışa aktardığı aygıt atama dosyasının (JSON) içeriğini yapıştırın. Hiçbir şey kendiliğinden kaydedilmez; sonucu gözden geçirip kaydedin.',
    file: 'Dosya seç',
    run: 'İçe aktar',
    done: 'İçe aktarılan yapılandırma yüklendi. Gözden geçirip kaydedin.'
  },

  experimental: {
    title: 'Deneysel oturum yöntemi',
    ack: 'Riskleri okudum ve anladım',
    enable: 'Deneysel yöntemi seç',
    cancel: 'Vazgeç'
  },

  wizard: {
    title: 'Kurulum sihirbazı',
    intro: 'Sihirbaz her ekranı bir koltuğa çevirir ve her koltuğun klavyesini ve faresini dokunarak bulur. Başlamadan önce bütün aygıtların takılı olduğundan ve çalışma boyunca değişmeyeceğinden emin olun.',
    firstRun: 'Henüz hiçbir koltuk ayarlanmamış. Kurulum sihirbazını çalıştırmak ister misiniz?',
    variants: 'Nasıl ilerlemek istersiniz?',
    all_seats: 'Her koltukta ayrı klavye ve fare var: bütün koltukları (1. dahil) sırayla tanıt',
    keep_seat_one: '1. koltuğun şu anki aygıtları kalsın: yalnızca 2. ve sonraki koltukları tanıt',
    quick_two_seats: 'Hızlı iki koltuk: 1. koltuğun aygıtları 1. koltukta, geri kalan her şey 2. koltukta',
    seatOneDevices: '1. koltuk için kullanılacak aygıtlar',
    extend: 'Masaüstünü bütün ekranlara genişlet (önerilir)',
    start: 'Sihirbazı başlat',
    pickMonitor: 'Her ekranda büyük bir numara görünüyor. 1. koltuğun ekranındaki numaraya basın (10 için 0) ya da aşağıdan seçin.',
    pickMonitorMany: '10’dan büyük numaralı ekranlar için o ekrandaki düğmeye ya da aşağıdaki listeye tıklayın.',
    /** F1..F12 only exist up to seat 12; seats 13+ use the any-key way alone. */
    keyboard: (n: number) =>
      n <= FKEY_SEATS
        ? `${n}. koltuğun klavyesinde F${n} tuşuna iki kez basın.`
        : `Sıra ${n}. koltuğa gelince o koltuğun klavyesinde herhangi bir tuşa iki kez basın.`,
    anyKey: 'Sıra bu koltuktaysa o klavyede herhangi bir tuşa iki kez basmanız da yeter.',
    invalid: 'Bu atama kaydedilemedi. Sihirbaz sürüyor: sorunu giderip yeniden kaydedin ya da kaydetmeden çıkın.',
    gather: 'Ana klavye ve fareyi dokunarak bul',
    gatherStop: 'Aramayı bitir',
    gatherHint: '1. koltukta kullanacağınız klavyede bir tuşa basın ve fareyi oynatın.',
    gathered: 'Dokunarak bulunan ana aygıtlar',
    minResolution: (w: number, h: number) => `Sihirbaz için ana ekranın çözünürlüğü en az ${w}×${h} olmalı. Ekran ayarlarından çözünürlüğü yükseltin.`,
    noMonitor: 'Sihirbaz için en az bir ekran gerekir.',
    pagesFailed: 'Ekran sayfaları açılamadı; sihirbaz yalnızca bu pencerede sürer.',
    pageNumber: 'Bu ekranın numarası',
    pagePick: '1. koltuğun ekranı buysa bu numaraya basın ya da aşağıdaki düğmeye tıklayın.',
    pagePickButton: 'Bu ekran 1. koltuk',
    pagePickButtonOnly: '1. koltuğun ekranı buysa aşağıdaki düğmeye tıklayın (bu numara için tuş yok).',
    pageSeat: (n: number) => `${n}. koltuk`,
    pageNoSeat: 'Bu ekrana koltuk verilmedi (koltuk sınırı).',
    pageOtherSeat: (n: number) => `Şu an ${n}. koltuk ayarlanıyor. Sıra bu koltuğa gelince burada yönergeler çıkar.`,
    pageTarget: 'Bu hedefe tıklayın',
    pageKept: 'Bu koltuğun aygıtları olduğu gibi kalıyor.',
    pageDone: 'Bu koltuk tamam. Diğer koltuklar bekleniyor.',
    lastUnit: (name: string) => `Son algılanan: ${name}`,
    keyboardCount: (k: number) => `Basılan: ${k} / 2`,
    mouse: (n: number) => `${n}. koltuğun faresiyle o koltuğun ekranındaki iki hedefe tıklayın.`,
    mouseCount: (k: number) => `Tıklanan: ${k} / 2`,
    waiting: 'Bu koltuk tamam; diğer koltuklar bekleniyor.',
    done: 'Bütün koltuklar tanıtıldı.',
    cancelled: 'Sihirbaz kaydetmeden kapatıldı.',
    keys: 'Herhangi bir klavyede: Enter bu koltuğu onaylar · Geri bu koltuğu sıfırlar · Shift+Enter hepsini kaydeder · Shift+Geri hepsini sıfırlar · Shift+Esc ya da Alt+F4 kaydetmeden çıkar',
    confirmSeat: 'Bu koltuğu onayla',
    resetSeat: 'Bu koltuğu sıfırla',
    resetAll: 'Hepsini sıfırla',
    saveAll: 'Kaydet ve bitir',
    quit: 'Kaydetmeden çık',
    assigned: 'Bulunan aygıtlar',
    notAllConnected: 'Sihirbaz için bütün aygıtların takılı olması gerekir. Kayıtlı ama takılı olmayan aygıtlar var.',
    finishHint: 'Bitince bir başlatma biçimi seçip Pulsar Seat’i açmayı unutmayın.',
    stepNames: {
      pick_console_monitor: '1. koltuğun ekranı',
      keyboard: 'Klavye',
      mouse: 'Fare',
      waiting: 'Bekleniyor',
      done: 'Bitti',
      cancelled: 'İptal edildi',
      invalid: 'Kaydedilemedi'
    } as Record<string, string>
  },

  seatState: {
    off: 'Kapalı',
    waiting_for_display: 'Ekran bekleniyor',
    starting: 'Başlatılıyor',
    at_logon: 'Oturum açma ekranında',
    signed_in: 'Oturum açık',
    locked: 'Kilitli',
    stopping: 'Durduruluyor',
    error: 'Hata'
  } as Record<string, string>,

  /** Device descriptions for tooltips and the device info dialog (describe.ts). */
  describe: {
    seatsOf: (seats: number[]) => `${seats.map((s) => `${s}.`).join(', ')} koltuk`,
    explicit: 'Elle atandı',
    output: {
      internal: 'yerleşik',
      hdmi: 'HDMI',
      displayport: 'DisplayPort',
      dvi: 'DVI',
      vga: 'VGA',
      miracast: 'kablosuz (Miracast)',
      indirect: 'dolaylı (USB / sanal)',
      other: 'diğer'
    } as Record<string, string>,
    isolation: {
      descriptor: 'Erişim, aygıt güvenlik tanımıyla sınırlanıyor',
      session_letter: 'Sürücü harfi yalnızca sahibinin oturumunda görünüyor',
      minifilter: 'Depolama süzgeciyle sınırlanıyor',
      camera_consent: 'Kamera izniyle sınırlanıyor',
      shared: 'Ortak: herkes kullanabilir',
      not_supported: 'Bu aygıt bu bilgisayarda koltuğa kapatılamıyor',
      network: 'Ağ bağdaştırıcısı: her zaman ortak',
      seat_filter: 'Onu yalnızca kendi koltuğunun programları görür (akıllı kart yönlendirmesi / Pulsar Seat modülü)'
    } as Record<string, string>,
    seat: (v: string) => `Koltuk: ${v}`,
    location: (v: string) => `Konum: ${v}`,
    adapter: (v: string) => `Ekran kartı: ${v}`,
    connection: (v: string) => `Bağlantı: ${v}`,
    resolution: (w: number, h: number, hz: string) => `Çözünürlük: ${w}×${h}, ${hz} Hz`,
    ports: (n: number) => `Port sayısı: ${n}`,
    plugged: (v: string) => `Takılı: ${v}`,
    bus: (v: string, builtIn: boolean) => `Bağlantı türü: ${v}${builtIn ? ' (yerleşik)' : ''}`,
    letters: (v: string) => `Sürücü harfleri: ${v}`,
    serial: (v: string) => `Seri numarası: ${v}`,
    hardwareId: (v: string) => `Donanım kimliği: ${v}`
  },

  /** Names of special keys in the switch shortcut (hotkey.ts). */
  keyNames: { Backspace: 'Geri', Tab: 'Sekme', Space: 'Boşluk' } as Record<string, string>,

  /** Built-in device picture names (pictures.ts ids). */
  pictureNames: {
    'monitor-speakers': 'Hoparlörlü ekran',
    'monitor-curved': 'Kavisli ekran',
    tv: 'Televizyon',
    laptop: 'Dizüstü ekranı',
    'keyboard-compact': 'Kısa klavye',
    'keyboard-numpad': 'Sayı tuşlu klavye',
    'keyboard-laptop': 'Dizüstü klavyesi',
    'mouse-wireless': 'Kablosuz fare',
    'mouse-vertical': 'Dikey fare',
    trackball: 'İztopu',
    touchpad: 'Dokunmatik yüzey',
    trackpoint: 'İşaret çubuğu',
    pen: 'Kalem',
    touchscreen: 'Dokunmatik ekran',
    gamepad: 'Oyun kumandası',
    headphones: 'Kulaklık',
    'speaker-single': 'Tek hoparlör',
    headset: 'Mikrofonlu kulaklık',
    'webcam-clip': 'Ekran üstü kamera'
  } as Record<string, string>,

  /** Update dialog (update.ts). */
  update: {
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
  },

  phases: {
    queued: 'sırada',
    waiting_for_host: 'sunucu bekleniyor',
    binding_display: 'ekran bağlanıyor',
    launching_host: 'koltuk sunucusu başlatılıyor',
    connecting: 'bağlanıyor',
    waiting_for_session: 'oturum bekleniyor',
    logging_on: 'oturum açılıyor'
  } as Record<string, string>
};

const productText = {
  disabled: 'Kapalı',
  enabled: 'Açık',
  safe_mode: 'Güvenli kip (yalnızca 1. koltuk)',
  enabledStarted: 'Açık, koltuklar çalışıyor',
  enabledNotStarted: 'Açık, koltuklar başlatılmadı'
} as Record<string, string>;

const safeModeText: Record<string, string> = {
  windows_safe_mode: 'Windows güvenli kipte açıldı; Pulsar Seat çalışmıyor. Bilgisayarı normal biçimde yeniden başlatın.',
  unclean_shutdown: 'Windows bir önceki sefer düzgün kapanmadı (çökme ya da elektrik kesintisi). Döngüye girmemek için ek koltuklar başlatılmadı. Nedeni giderip yeniden başlatın; Pulsar Seat sonra normal biçimde başlar.',
  crash_loop: 'Pulsar Seat hizmeti art arda başlatılamadı ve güvenli kipte kaldı. Bir tanı raporu oluşturup Pulsar Seat’i güncelleyin.'
};

const failureText: Record<string, string> = {
  no_monitor: 'Sihirbaz için en az bir ekran gerekir',
  resolution_too_low: 'Ana ekranın çözünürlüğü sihirbaz için çok düşük',
  unknown_monitor: 'Bu ekran sihirbazın ekranları arasında yok',
  edition_unsupported: 'Bu Windows sürümü bu oturum yöntemini desteklemiyor',
  rds_not_ready: 'Uzak Masaüstü Oturumu Ana Bilgisayarı hazır değil',
  seat_limit: 'Bu bilgisayarda bu kadar koltuk çalıştırılamıyor',
  account: 'Hesap bulunamadı ya da devre dışı',
  logon_failed: 'Oturum açılamadı (parola yanlış ya da süresi dolmuş)',
  client_auth: 'Bağlantı kimlik doğrulaması reddedildi',
  host_exited: 'Koltuk sunucusu tekrar tekrar kapandı',
  session_timeout: 'Oturum zamanında başlamadı',
  display: 'Koltuğa ekran bağlanamadı',
  driver: 'Gereken Pulsar sürücüsü yüklü değil ya da imzalı değil; Pulsar Seat’i güncelleyin',
  coexistence: 'Çakışan başka bir çoklu koltuk yazılımı bulundu',
  same_account_denied: 'Bu hesap başka bir koltukta zaten açık',
  policy: 'Bir Windows ilkesi bu işlemi engelliyor',
  internal: 'Beklenmeyen bir hata oluştu',
  console_in_use: 'Konsolda kilidi açık bir oturum var; önce konsolu kilitleyin (Win+L)',
  console_busy: 'Konsolda şu an başka bir koltuk var; o koltuğu önce kilitleyin',
  session_not_locked: 'Koltuğun oturumu kilitli değil ve parola kayıtlı değil; oturumu kilitleyin ya da parolayı kaydedin',
  nla_required:
    'Parolasız koltuklar için ayrı bağlantı noktası kapalı; Gelişmiş sekmesinden açın ya da koltuğa parola kaydedin',
  loopback_listener_unavailable: 'Parolasız koltuklar için ayrı bağlantı noktası hazır değil',
  kiosk_app_missing: 'Kiosk koltuğu için bir program seçilmemiş',
  console_not_ready: 'Konsol oturumu henüz hazır değil; Windows açılışı bitince tekrar deneyin',
  not_our_session: 'Bulunan oturum bu koltuğa ait değil; koltuğu yeniden başlatın',
  fsu_disabled: 'Hızlı kullanıcı geçişi bir ilkeyle kapatılmış; ilkeyi kaldırmadan bu yöntem çalışmaz',
  no_account: 'Koltuğa bir hesap seçilmemiş',
  account_unresolved: 'Koltuğun hesabı bu bilgisayarda bulunamadı',

  elevation_cancelled: 'Yönetici izni verilmedi',
  pipe_server_untrusted: 'Denetim kanalını Pulsar Seat hizmeti dışında bir program açmış; bağlantı kurulmadı',
  shell_failed: 'Pencere açılamadı'
};

const errorText: Record<string, string> = {
  service_unavailable: t.serviceDown,
  access_denied: t.adminNeeded,
  admin_only: 'Yönetici, ayarların yalnızca yöneticilerce değiştirilmesini istedi.',
  version_mismatch: 'Yönetici ile hizmetin sürümleri uyuşmuyor. Pulsar Seat’i yeniden kurun.',
  disabled: 'Pulsar Seat kapalı. Önce açın.',
  safe_mode: 'Pulsar Seat güvenli kipte; ek koltuklar başlatılmıyor.',
  revision_conflict: 'Ayarlar bu arada başka yerden değişti. Yeniden yükleyip tekrar deneyin.',
  conflicts_not_confirmed: 'Uygulanamayan atamalar var; onaylamadan kaydedilmedi.',
  invalid_config: 'Ayarlarda hata var; kaydedilmedi.',
  invalid_value: 'Geçersiz değer.',
  not_found: 'Bulunamadı.',
  seat_running: 'Koltuk zaten çalışıyor.',
  seat_not_running: 'Koltuk çalışmıyor.',
  unsupported: 'Seçili yöntem bunu desteklemiyor.',
  not_ready: 'Henüz hazır değil; biraz sonra tekrar deneyin.',
  timeout: 'Zaman aşımı.',
  driver_unavailable: 'Gereken Pulsar sürücüsü kullanılamıyor.',
  coexistence: 'Çakışan başka bir çoklu koltuk yazılımı bulundu.',
  policy_blocked: 'Bir Windows ilkesi bu işlemi engelliyor.',
  logon_failed: 'Oturum açılamadı.',
  warning_not_acknowledged: 'Deneysel yöntem için uyarının onaylanması gerekir.',
  wizard_state: 'Sihirbaz şu anda bu işlemi kabul etmiyor.',
  os: 'Windows işlemi başarısız oldu.',
  store: 'Ayar dosyası yazılamadı.',
  not_implemented: 'Bu işlem hizmetin bu sürümünde henüz yok.',
  wrong_seat_kind: 'Bu işlem bu tür koltukta yapılamaz (yerel / uzak koltuk).',
  session_unavailable: 'Uzak koltuklar için aynı anda birden fazla oturum gerekir; seçili oturum yöntemi bunu sağlamıyor.',
  bad_request: 'Geçersiz istek.',
  internal: 'Beklenmeyen bir hata oluştu.'
};

const issueText: Record<string, string> = {
  too_many_seats: 'En fazla 16 koltuk olabilir.',
  duplicate_seat: 'Aynı koltuk numarası iki kez var.',
  invalid_seat_name: 'Koltuk adı geçersiz (1–64 karakter).',
  invalid_account: 'Oturum hesabı geçersiz.',
  seat_one_no_display: '1. koltuğun bir ekranı olmalı; kaydedilemez.',
  exclusive_on_several_seats: 'Bu aygıt yalnızca bir koltukta olabilir.',
  mask_beyond_seats: 'Aygıt var olmayan bir koltuğa atanmış.',
  duplicate_device: 'Aynı aygıt iki kez atanmış.',
  timeout_out_of_range: 'Zamanlama değeri izin verilen aralığın dışında.',
  loopback_port_invalid: 'Parolasız koltuk bağlantı noktası 1024 ile 65535 arasında olmalı ve 3389 olamaz.',
  hide_fsu_with_fast_user_switch: 'Hızlı kullanıcı değiştirmeyi gizlemek, “aynı anda bir koltuk” oturum yöntemini kapatır.',
  loopback_no_accounts:
    'Bu koltuk oturum açma ekranında açılıyor, ama parolasız koltuk bağlantı noktasını kullanabilecek hesap yok; koltuk başlayamaz. Gelişmiş sekmesindeki “ek hesaplar” listesine bu koltukta oturum açacak kişileri ekleyin ya da koltuğa bir hesap verin.',
  logon_screen_needs_listener:
    'Bu koltuk oturum açma ekranında açılıyor; parolasız koltuk bağlantı noktası kapalıyken başlatılamaz. Bağlantı noktasını açın ya da koltuğa parolalı bir hesap verin.',
  loopback_account_invalid: 'Ek hesaplardan biri geçersiz (kullanici ya da ETKİ_ALANI\\kullanici yazın, @ kullanmayın); yok sayılır.',
  invalid_hotkey: 'Geçiş kısayolu geçersiz.',
  experimental_not_acknowledged: 'Deneysel oturum yöntemi için uyarı onaylanmadı.',
  seat_without_display: 'Koltuğun ekranı yok; ekran takılana kadar bekler.',
  seat_without_keyboard: 'Koltuğun klavyesi yok.',
  seat_without_mouse: 'Koltuğun faresi yok.',
  child_of_other_seat_hub: 'Aygıt, başka bir koltuğa verilmiş bir USB göbeğine takılı.',
  same_account_on_seats: 'Aynı hesap birden fazla koltukta ve buna izin verilmiyor.',
  key_collision: 'İki aygıt ayırt edilemiyor; farklı portlara takın.',
  monitor_hub_mismatch: 'Ekranın kendi USB göbeği bir koltuğa verilmiş, ekran ise o koltukta değil.',
  cpu_unknown_id: 'Seçilen çekirdeklerden bazıları bu bilgisayarda yok; yok sayılır.',
  cpu_no_core: 'Koltuğa hiç çekirdek kalmıyor; kaydedilemez.',
  cpu_only_efficiency_cores: 'Koltuğa yalnızca verimlilik çekirdekleri verilmiş; yavaş çalışabilir.',
  invalid_ip_address: 'IP adresi geçersiz.',
  invalid_pad_ignore_device: 'Yok sayılacak kol listesinde 0xVVVV/0xPPPP biçiminde olmayan bir girdi var (ya da liste çok uzun).',
  duplicate_ip_address: 'Aynı IP adresi birden fazla koltuğa verilmiş.',
  too_many_ip_programs: 'Program listesi çok uzun; fazlası kaydedilmez.',
  invalid_ip_program: 'Program listesinde yolu boş bir satır var.',
  loopback_ip_address: '127 ile başlayan adresler yalnızca bu bilgisayarın içinde çalışır; koltuk bu adresle yerel ağa çıkamaz.',
  link_local_ip_address: '169.254 ile başlayan adresi Windows kendisi verir ve değişebilir; sabit bir adres seçin.',
  ip_binding_shared_session: 'Bu kiosk koltuğu oturumunu başka koltuklarla paylaşıyor; IP adresi sürücüyle zorlanamaz, yalnızca destekleyen programlar kullanır.',
  clone_not_allowed: 'Bir ekrana birden fazla çıkış seçilmiş ama klonlamaya izin verilmemiş.',
  same_account_fast_user_switch: 'Hızlı kullanıcı geçişinde her koltuğun ayrı bir hesabı olmalı.',
  invalid_gpu_preference: 'Ekran kartı tercihindeki programlar geçersiz (en çok 64 program, her biri tam bir .exe yolu).',
  invalid_kiosk_shell: 'Kiosk koltuğunun ayarları geçersiz (en çok 16 uygulama, adı ve yolu dolu, boşta sıfırlama 0–1440 dakika).',
  kiosk_shell_ignored_on_seat_one: '1. koltuk normal masaüstü olarak kaldığı için kiosk ayarı bu koltukta kullanılmaz.',
  seat_kind_mismatch: 'Koltuğun türü (yerel / uzak) ayarlarıyla uyuşmuyor.',
  console_seat_remote: '1. koltuk her zaman yerel koltuktur.',
  device_on_remote_seat: 'Uzak koltuğa aygıt atanamaz.',
  remote_seat_invalid: 'Uzak koltuğun ayarlarında hata var.',
  session_unavailable: 'Uzak koltuklar seçili oturum yöntemiyle başlatılamaz (aynı anda birden fazla oturum gerekir).',

  store_failed: 'Ayarlar dosyaya yazılamadı.'
};

const noticeText: Record<string, string> = {
  gpu_preference_unavailable:
    'Koltuğun ekran kartı tercihi uygulanamadı: bilgisayarda tek ekran kartı var ya da koltuğun ekran kartı Windows tarafından yüksek performanslı veya güç tasarruflu olarak tanınmıyor.',
  os_build_changed: 'Windows güncellendi. Koltuk ayarlarını gözden geçirip gerekirse yeniden kaydedin.',
  service_gap_no_input: 'Hizmet kısa bir süre yanıt vermedi; giriş bir anlığına durmuş olabilir.',
  same_account_warning: 'Aynı hesap birden fazla koltukta açık. Bu önerilmez.',
  seat_starting: 'Koltuk başlatılıyor.',
  run_entry_missing: 'Seçilen başlatma biçimi için gereken yönetici başlangıç kaydı eksik; koltuklar kendiliğinden başlamayabilir.',
  program_files_damaged: 'Pulsar Seat dosyaları eksik ya da bozuk. Lütfen yeniden kurun.',
  hardware_changed: 'Takılı aygıtlar kayıtlı yapılandırmadan farklı.',
  devices_beyond_limit: 'Koltuk sınırının üstündeki aygıtlar boşa çıkarıldı. Kaydetmeyi unutmayın.',
  reboot_needed: 'Değişikliğin tamamlanması için yeniden başlatma gerekiyor.',
  seat_start_failed: 'Bir koltuk başlatılamadı.',
  password_failing: 'Bir koltuğun kayıtlı parolası reddedildi; parolayı yeniden girin.',
  tweak_failed: 'İsteğe bağlı bir sistem ayarı uygulanamadı ya da geri alınamadı; ayrıntı hizmet günlüğünde.',
  keep_awake_failed: 'Ek koltuklar kullanılırken bilgisayarın uyumaması sağlanamadı.',
  input_move_pending: 'Kısayolla taşınan klavye ve fareler onay bekliyor.',
  credprov_failed: 'Koltuk oturum açma bileşeni kaydedilemedi ya da kaldırılamadı; ayrıntı hizmet günlüğünde.',
  credprov_logon_failed: 'Bir koltuğun kendiliğinden oturum açması kayıtlı parolayla başarısız oldu; parolayı yeniden girin.',
  switch_refused: 'Koltuğa geçilemedi; ayrıntı hizmet günlüğünde.'
};

/** Text per state / reason of the passwordless-seat listener (`loopback_listener:<state>[:<x>]`). */
const loopbackStateText: Record<string, string> = {
  unknown: 'henüz denetlenmedi',
  disabled: 'ayarlardan kapatılmış',
  not_needed: 'gerekmiyor: her koltukta parola kayıtlı',
  ready: 'hazır',
  pending_restart:
    'Uzak Masaüstü Hizmetlerinin yeniden başlaması bekleniyor; açık Uzak Masaüstü koltukları kapanınca kendiliğinden yapılır',
  port_invalid: 'bağlantı noktası geçersiz (1024–65535 olmalı ve Uzak Masaüstünün kendi bağlantı noktası olamaz)',
  port_in_use: 'bağlantı noktası başka bir program ya da başka bir Uzak Masaüstü dinleyicisi tarafından kullanılıyor',
  firewall_off: 'Windows Güvenlik Duvarı en az bir ağ profilinde kapalı; açın',
  firewall_local_rules_ignored:
    'bir grup ilkesi güvenlik duvarının yerel kuralları yok saymasını istiyor; koruma kuralı uygulanamaz',
  nla_forced_by_policy: 'bir ilke bütün bağlantılarda ağ düzeyinde kimlik doğrulamasını zorunlu tutuyor',
  tls_off_by_policy: 'bir ilke şifreli (TLS) bağlantıyı kapatıyor',
  no_accounts: 'bu bağlantı noktasını kullanabilecek hesap yok ya da hesaplar bulunamadı',
  rdp_tcp_missing: 'Uzak Masaüstü dinleyicisi (RDP-Tcp) bulunamadı',
  write_failed: 'ayarlar kayıt defterine yazılamadı',
  termservice_restart_failed: 'Uzak Masaüstü Hizmetleri yeniden başlatılamadı',
  restart_needs_reboot: 'Uzak Masaüstü Hizmetleri bu Windows sürümünde durdurulamıyor; bilgisayarı yeniden başlatın',
  not_listening: 'bağlantı noktası henüz dinlemiyor; biraz bekleyip tekrar deneyin ya da bilgisayarı yeniden başlatın',
  untrusted_listener: 'bağlantı noktasını Uzak Masaüstü Hizmetleri dışında bir program dinliyor',
  key_changed: 'dinleyici ayarları sonradan değiştirilmiş; koltuk yeniden başlatılınca düzeltilir',
  firewall_rule_missing: 'koruma için güvenlik duvarı kuralı eksik ya da değiştirilmiş'
};

/** Strings used by the label helpers in i18n.ts. */
const misc = {
  errorPrefix: (what: string) => `Hata: ${what}`,
  issueSeat: (n: number) => `${n}. koltuk`,
  listAnd: ' ve ',
  loopbackHead: 'Parolasız koltuk bağlantı noktası',
  loopbackReady: (port: string) => `hazır (${port})`,
  loopbackExposed: (port: string) =>
    `DİKKAT: kaldırılan parolasız koltuk bağlantı noktası (${port}) ağ düzeyinde kimlik doğrulaması olmadan hâlâ çalışıyor ve güvenlik duvarı onu korumuyor. Uzak Masaüstü koltuklarını kapatın ya da bilgisayarı yeniden başlatın`,
  loopbackUnloadPending: (port: string) =>
    `Kaldırılan parolasız koltuk bağlantı noktası (${port}) Uzak Masaüstü Hizmetleri yeniden başlayana kadar çalışıyor; açık Uzak Masaüstü koltukları kapanınca kaldırılır`
};

// ---------------------------------------------------------------- remote seats
// Copy of the remote seat pages (from Pulsar Split's manager, merge-plan.md step 9). Kept as
// its own object so the remote pages keep their keys.

const rt = {
  appTitle: 'Pulsar Seat Yönetici',
  nav: {
    newSeat: 'Yeni uzak koltuk',
    startService: 'Servisi başlat',
    restartService: 'Servisi yeniden başlat',
    stopService: 'Servisi durdur',
    settings: 'Ayarlar',
    host: 'Ana bilgisayar',
    logs: 'Günlükler',
    dxvk: 'DXVK oyunları',
    noSeats: 'Henüz uzak koltuk yok',
    session: (id: number) => `Oturum ${id}`,
    streaming: 'yayında',
    streamingTo: (client: string) => `yayında: ${client}`,
    sessionUnavailable: 'Başlatılamaz: eşzamanlı oturum yok',
    sessionUnavailableHint:
      'Uzak koltuklar aynı anda birden fazla Windows oturumu gerektirir. Bu bilgisayarda çalışan oturum yöntemi bunu sağlamıyor (örneğin Windows istemci sürümü ya da hızlı kullanıcı değiştirme). Koltuk başlatılamaz.'
  },
  dxvk: {
    title: 'DXVK oyunları',
    intro:
      'DXVK, eski Direct3D 8/9 oyunlarını Vulkan üzerinden çalıştırır. Dosyalar yalnızca oyunun klasörüne kopyalanır; kapatınca eski dosyalar geri gelir.',
    release: (v: string) => `İndirilmiş DXVK sürümü: ${v}`,
    noRelease: 'DXVK henüz indirilmedi. Önce yönetici komut isteminde "pulsar-split-shimctl dxvk download" çalıştırın.',
    games: 'Oyunlar',
    noGames: 'Henüz DXVK açılmış oyun yok.',
    exe: 'Oyun dosyası (.exe)',
    browse: 'Gözat…',
    pickTitle: 'Oyunun .exe dosyasını seçin',
    dlls: 'Yönlendirilecek Direct3D sürümleri',
    enable: 'DXVK aç',
    disable: 'DXVK kapat',
    deployed: (v: string | null, dlls: string) => `Açık (${dlls}${v ? `, sürüm ${v}` : ''})`,
    notDeployed: 'Kapalı (dosyalar kaldırılmış)',
    markerUnreadable: 'İşaret dosyası okunamadı; dosyalar elle kontrol edilmeli.',
    enabled: 'DXVK açıldı.',
    disabled: 'DXVK kapatıldı.',
    needDll: 'En az bir Direct3D sürümü seçin.',
    needExe: 'Oyunun tam .exe yolunu girin.',
    refresh: 'Yenile'
  },
  logs: {
    title: 'Günlükler',
    source: 'Kaynak',
    service: 'Servis',
    seat: 'Koltuk',
    refresh: 'Yenile',
    redacted: 'Şifre, PIN ya da anahtar içerebilecek satırlar gizlenir.',
    empty: 'Günlükte satır yok.'
  },
  service: {
    title: 'Servis',
    reachable: 'Servise bağlı',
    unreachable: 'Servise ulaşılamıyor',
    consoleMode: 'geliştirici (konsol) modu',
    version: 'Sürüm',
    starting: 'Servis başlatılıyor…',
    stopping: 'Servis durduruluyor…',
    restarting: 'Servis yeniden başlatılıyor…',
    pendingHint: 'Servis şu an durum değiştiriyor, biraz bekleyin.'
  },
  serviceState: {
    not_installed: 'Kurulu değil',
    stopped: 'Durdu',
    running: 'Çalışıyor',
    pending: 'Geçişte',
    unknown: 'Bilinmiyor'
  } satisfies Record<ServiceRunState, string>,
  settings: {
    title: 'Ayarlar',
    windows: 'Windows',
    startWithWindows: 'Windows ile başlat',
    isolation: 'Yalıtım',
    hidIsolation: 'HID aygıtlarını yalıt',
    processPatching: 'İşlem yamalamayı etkinleştir',
    steamIsolation: 'Steam’i koltuklara ayır',
    isolationNote:
      'HID yalıtımı ve Steam ayrımı yeni başlatılan koltuklarda geçerli olur. İşlem yamalama, Pulsar Seat modülünü seçtiğiniz uygulamalara yükler (x64 ve ARM64 bilgisayarlar). ARM64 bilgisayarda x64 uygulamalar modülü yalnızca Pulsar Seat onları başlattığında alır (yayın uygulamaları, kiosk uygulamaları); kendiniz başlattığınız bir x64 uygulama yamalanmaz ve Windows’un kullandığı listeye eklenmez.',
    antiCheat:
      'Uyarı: Hile önleme yazılımı kullanan bazı oyunlar, işlemlere yüklenen modülü ya da uzak oturum gizlemeyi sorun olarak görebilir.',
    isolationState: 'Yalıtım durumu',
    installed: 'Modül kurulu',
    notInstalled: 'Modül kurulu değil (yalnızca aygıt güvenliği)',
    unsupportedArch: 'Bu bilgisayarda bu mimari için işlem yamalama modülü kurulu değil.',
    killSwitch: 'Acil kapatma açık: modül hiçbir işlemde kanca kurmuyor.',
    foreign: (s: string) => `Başka bir ürünün yamalama veritabanı bulundu (${s}). Önce o ürünün işlem yamalamasını kapatın.`,
    targets: 'Modülün yükleneceği uygulamalar (dosya adı)',
    targetsGameInput:
      'GameInputSvc.exe (Windows oyun girişi hizmeti) eklenirse orada bir kumanda odağı filtresi açılır: uzak bir koltuğun penceresi öne gelince bu bilgisayardaki kumanda odağını almaz. Deneyseldir; listede olmadıkça kapalıdır.',
    addTarget: 'Ekle',
    maskApps: 'Uzak oturumu gizlenecek uygulamalar (tam yol)',
    maskHint: 'Uygulama, koltukta “uzak oturumda çalışıyorum” bilgisini görmez. Yalnızca gerekli oyunlar için açın.',
    pads: 'Oyun kumandaları',
    padBackend: 'Sanal kumanda sürücüsü',
    auto: 'Otomatik',
    bluetoothConsoleOnly: 'Bluetooth yalnızca fiziksel ekranda (konsol oturumu) kullanılsın',
    padAudio: 'DualSense hoparlör, mikrofon ve titreşim sesi uzak koltuklara da gitsin (yalnızca Pulsar akışı; hizmet yeniden başlayınca geçerli)',
    displayBackend: 'Sanal ekran yöntemi',
    log: 'Kayıt',
    verbosity: 'Ayrıntı düzeyi'
  },
  verbosity: {
    none: 'Hiçbiri',
    error: 'Hata',
    warning: 'Uyarı',
    info: 'Bilgi',
    debug: 'Hata ayıklama'
  } satisfies Record<Verbosity, string>,
  seat: {
    actions: 'İşlemler',
    start: 'Koltuğu başlat',
    stop: 'Koltuğu durdur',
    delete: 'Koltuğu sil',
    pair: 'Yeni cihaz eşleştir',
    general: 'Koltuk ayarları',
    displayName: 'Görünen ad',
    port: 'Port',
    portHint: (p: number) => `Kullanılan port aralığı: ${p - 5}–${p + 21}`,
    startWithService: 'Servisle birlikte başlat',
    stopAction: 'Durdurunca oturum',
    stopLogOff: 'Oturumu kapat',
    stopDisconnect: 'Bağlantıyı kes, oturumu açık bırak',
    user: 'Kullanıcı',
    userName: 'Windows kullanıcısı',
    chooseUser: 'Kullanıcı seçin',
    password: 'Şifre',
    passwordSet: 'Şifre kayıtlı',
    passwordMissing: 'Şifre girilmedi',
    savePassword: 'Şifreyi kaydet',
    passwordSaved: 'Şifre kaydedildi.',
    testCredentials: 'Kullanıcı bilgilerini sına',
    show: 'Göster',
    hide: 'Gizle',
    display: 'Görüntü',
    renderAdapter: 'Ekran kartı (boş: en uygun)',
    scale: 'Masaüstü ölçeği',
    scaleDynamic: 'Dinamik (istemciyi izle)',
    scaleStatic: 'Sabit',
    superSampling: 'Süper örnekleme',
    sdrWhite: 'SDR beyaz düzeyi',
    forceSdr: 'HDR akışlarda SDR’ye zorla',
    autostart: 'Otomatik başlayan uygulamalar',
    autostartPath: 'Uygulama yolu (.exe)',
    autostartArgs: 'Parametreler',
    autostartAsSystem: 'SYSTEM olarak çalıştır',
    addApp: 'Uygulama ekle',
    remove: 'Kaldır',
    lockedHint: 'Koltuk çalışırken ayarları değiştirilemez.',
    saved: 'Kaydedildi',
    crashes: (n: number) => `Akış sunucusu ${n} kez çöktü`,
    lastError: 'Son hata',
    failure: 'Başarısız olma nedeni',
    phase: 'Adım',
    runtime: 'Çalışma durumu',
    session: (id: number) => `Windows oturumu: ${id}`,
    clientName: 'RDP istemci adı',
    displayBackend: 'Ekran',
    hdr: 'HDR',
    hdrOk: 'Kullanılabilir',
    audio: 'Ses',
    pads: 'Kumandalar',
    noPads: 'Bağlı sanal kumanda yok',
    stream: 'Akış sunucusu',
    streamRunning: 'Çalışıyor',
    streamActive: 'Yayın sürüyor',
    isolationHealth: 'Yalıtım modülü',
    adapterAuto: 'En uygun ekran kartı',
    refresh: 'Yenileme hızı (Hz)',
    luminance: 'HDR parlaklığı (gelişmiş)',
    maxNits: 'En yüksek (nit)',
    maxFullFrame: 'Tam ekran en yüksek (nit)',
    audioSection: 'Ses',
    streamAudio: 'Koltuğun sesini akışa ekle',
    microphone: 'Mikrofon (yalnızca Pulsar istemcisi)',
    microphoneHint:
      'Pulsar istemcisinin mikrofonu koltuktaki uygulamalara mikrofon olarak verilir. Mikrofonu destekleyen bir pulsar-host gerekir; Moonlight mikrofon taşımaz. Değişiklik koltuk yeniden başlatılınca geçerli olur.',
    mic: 'Mikrofon',
    micState: {
      off: 'Kapalı',
      unsupported: 'Bu pulsar-host sürümü mikrofonu desteklemiyor',
      waiting: 'İstemci bağlantısı bekleniyor',
      connected: 'Bağlı',
      failed: 'Mikrofon kanalı açılamadı',
      closed: 'Bağlı değil'
    } as Record<string, string>,
    browse: 'Gözat…',
    pickAutostart: 'Başlatılacak programı seçin',
    gpuNoHardware: 'Donanım ekran kartı bulunamadı: görüntü işlemcide kodlanır, yayın yavaş olabilir.',
    gpuNoEncoder: (name: string) => `${name} için donanım video kodlayıcısı bilinmiyor; kodlama işlemciye düşebilir.`,
    gpuHybrid:
      'Birden çok ekran kartı var (hibrit sistem). Kodlayıcısı olan kartı burada elle seçin; otomatik seçim en çok belleği olanı alır.',
    streamSection: 'Akış',
    gamepad: 'Kumanda türü',
    gamepadAuto: 'Otomatik',
    capture: 'Görüntü yakalama',
    captureDefault: 'Varsayılan',
    minFps: 'En düşük FPS hedefi',
    firewallDomain: 'Güvenlik duvarında etki alanı ağlarına da aç',
    keepPairing: 'Koltuk silinince eşleşmeleri sakla',
    apps: 'Uygulama listesi (istemcide görünenler)',
    appName: 'Ad',
    appCmd: 'Komut',
    saveApps: 'Listeyi kaydet',
    appsSaved: 'Uygulama listesi kaydedildi.',
    identity: 'Koltuk kimliği',
    regenerate: 'Kimliği yenile (bütün eşleşmeler silinir)',
    regenerateConfirm: 'Koltuğun sertifikası yenilenir ve eşleşmiş bütün cihazların yeniden eşleşmesi gerekir.',
    regenerateConfirmPulsar:
      'Koltuğun kimliği yenilenir: Pulsar kimlik numarası değişir ve eşleşmiş bütün cihazların yeniden eşleşmesi gerekir.',
    identityDone: 'Kimlik yenilendi.',
    streamBackend: 'Yayın sunucusu',
    backendSunshine: 'Sunshine (Moonlight uyumlu)',
    backendPulsar: 'Pulsar',
    backendMissing: 'kurulu değil',
    backendMissingHint: (b: string) => `${b} bu bilgisayarda kurulu değil; koltuk bu yayın sunucusuyla başlatılamaz.`,
    backendStopFirst: 'Yayın sunucusunu değiştirmek için önce koltuğu durdurun.',
    relayId: 'Pulsar kimliği',
    relayState: (s: string) =>
      (({ ok: 'bağlı', auth_required: 'giriş bekliyor', unreachable: 'ulaşılamıyor', version_mismatch: 'sürüm uyumsuz' }) as Record<string, string>)[s] ?? s
  },
  seatState: {
    stopped: 'Durdu',
    booting: 'Açılıyor',
    running: 'Çalışıyor',
    stopping: 'Duruyor',
    failed: 'Başarısız'
  } satisfies Record<RemoteSeatState, string>,
  phase: {
    user: 'Kullanıcı denetleniyor',
    session: 'Oturum açılıyor',
    display: 'Ekran hazırlanıyor',
    audio: 'Ses hazırlanıyor',
    stream: 'Akış sunucusu başlatılıyor',
    pads: 'Kumandalar hazırlanıyor',
    isolation: 'Yalıtım ayarlanıyor',
    autostart: 'Uygulamalar başlatılıyor'
  } as Record<string, string>,
  host: {
    title: 'Ana bilgisayar',
    audioCleanup: 'Eski ses aygıtları',
    audioCleanupHint:
      'Biten Uzak Masaüstü oturumlarından kalan, artık bağlı olmayan "Uzak Ses" aygıt kayıtlarını siler. Bağlı aygıtlara ve konsol aygıtlarına dokunmaz.',
    audioCleanupRun: 'Eski ses aygıtlarını temizle',
    audioCleanupConfirm: 'Kalan aygıt kayıtları kalıcı olarak silinsin mi? Her silme servis günlüğüne yazılır.',
    audioCleanupYes: 'Evet, temizle',
    audioCleanupNo: 'Vazgeç',
    audioCleanupDone: (removed: number, failed: number) =>
      removed === 0 && failed === 0 ? 'Silinecek eski aygıt yok.' : `${removed} aygıt silindi, ${failed} aygıt bırakıldı.`,
    edition: 'Windows sürümü',
    class: 'Çoklu koltuk durumu',
    rdsReady: 'Uzak Masaüstü hizmeti',
    ready: 'Hazır',
    notReady: 'Hazır değil',
    port: 'RDP portu',
    maxSeats: 'Aynı anda en fazla koltuk',
    unlimited: 'Sınırsız',
    licensing: 'RDS lisanslama',
    licensingMode: (m: number | null) => (m === 2 ? 'Cihaz başına' : m === 4 ? 'Kullanıcı başına' : 'Yapılandırılmamış'),
    graceDays: (d: number) => `Deneme süresinden ${d} gün kaldı`,
    warnings: 'Uyarılar',
    noWarnings: 'Uyarı yok.',
    multiSeat: 'Çoklu koltuk',
    multiSeatToggle: 'Bu bilgisayarı çoklu koltuk için hazırla',
    multiSeatHint:
      'Uzak Masaüstü ayarlarını koltuklar için değiştirir (özgün değerler saklanır ve kapatınca geri yüklenir). Kapatmak bütün koltukları durdurur.',
    passive:
      'Başka bir çoklu koltuk ürünü bulundu. Pulsar Seat pasif modda: sistemde hiçbir şey değiştirmez ve koltuk başlatmaz.',
    coexistence: 'Uyumluluk denetimi',
    noFindings: 'Sorun bulunmadı.',
    drivers: 'Sürücüler',
    displayDrivers: 'Sanal ekran',
    padDrivers: 'Sanal kumanda',
    installedYes: 'Kurulu',
    installedNo: 'Kurulu değil',
    signed: 'İmzalı',
    unsigned: 'İmzasız',
    remoteCapable: 'Uzak oturumda çalışır',
    consoleOnly: 'Yalnızca fiziksel ekranda',
    hdrAvailable: 'HDR kullanılabilir',
    hdrUnavailable: (why: string) => `HDR yok: ${why}`,
    experimental: 'Deneysel: istemci Windows’ta çoklu oturum',
    experimentalHint:
      'Yalnızca Windows Pro / Enterprise içindir ve sizin kurduğunuz açık kaynaklı bir sarmalayıcı gerektirir. Varsayılan olarak kapalıdır.',
    twState: {
      off: 'Kapalı',
      on: 'Açık',
      foreign_active: 'Başka bir araç Uzak Masaüstü hizmetini yönlendirmiş; Pulsar Seat dokunmaz.',
      blocked: 'Otomatik kapatıldı (desteklenmeyen sürüm ya da sarmalayıcı sorunu)'
    } as Record<string, string>,
    twEnable: 'Aç…',
    twDisable: 'Kapat',
    twNoCandidate: 'Kurulu bir sarmalayıcı bulunamadı.',
    refresh: 'Yenile'
  },
  hostClass: {
    home: 'Windows Home: çoklu koltuk desteklenmiyor.',
    client_multi_session_unavailable: 'Windows istemci sürümü: aynı anda yalnızca bir uzak oturum.',
    server_without_rdsh: 'Windows Server, Uzak Masaüstü Oturumu Ana Bilgisayarı rolü yok: yalnızca bir deneme koltuğu.',
    server_rdsh: 'Windows Server + Uzak Masaüstü Oturumu Ana Bilgisayarı: çoklu koltuk hazır.'
  } as Record<string, string>,
  hostWarning: {
    rdsh_missing: 'Uzak Masaüstü Oturumu Ana Bilgisayarı rolü kurulu değil.',
    licensing_not_configured: 'RDS lisanslama yapılandırılmamış; yalnızca bir koltuk çalışır.',
    grace_ends: 'RDS lisanslama deneme süresi bitmek üzere.',
    policy_override: 'Bir grup ilkesi Pulsar Seat’in ayarını geçersiz kılıyor.',
    seat_user_is_admin: 'Koltuk kullanıcısı yönetici; normal bir kullanıcı önerilir.',
    foreign_wrapper: 'Uzak Masaüstü hizmeti başka bir sarmalayıcıya yönlendirilmiş.',
    autostart_run_as_system_ignored: '“SYSTEM olarak çalıştır” yok sayıldı; uygulama koltuk kullanıcısıyla başlatıldı.',
    autostart_failed: 'Otomatik başlayan bir uygulama başlatılamadı.',
    adapter_fallback: 'Seçilen ekran kartı bulunamadı; varsayılan kullanılıyor.',
    firewall: 'Güvenlik duvarı kuralları eklenemedi; yerel ağdaki cihazlar koltuğa bağlanamayabilir.',
    control_unavailable: 'Akış sunucusunun denetim kanalı açılamadı; eşleştirme bu koltukta kullanılamıyor.',
    capture_error: 'Akış sunucusu görüntü yakalayamadı.'
  } as Record<string, string>,
  signal: {
    ref_arp: 'Başka bir çoklu koltuk ürünü kurulu.',
    ref_service: 'Başka bir çoklu koltuk ürününün hizmeti var.',
    termsrv_wrapped: 'Uzak Masaüstü hizmeti yönlendirilmiş.',
    bluetooth_wrapped: 'Bluetooth hizmeti yönlendirilmiş.',
    rdp_idd_redirected: 'RDP ekran sürücüsü değiştirilmiş.',
    ref_shim_db: 'Başka bir ürünün uyumluluk veritabanı kurulu.',
    ref_shim_exe_map: 'Başka bir ürünün uyumluluk eşlemesi var.',
    ref_shim_arp: 'Başka bir ürünün yamalama bileşeni kurulu.',
    ref_shim_file: 'Başka bir ürünün yamalama dosyası var.',
    compat_layer_forced: 'Bilgisayar genelinde bir uyumluluk katmanı zorlanmış.',
    ref_pad_driver: 'Başka bir ürünün kumanda sürücüsü kurulu.',
    ref_root_cert: 'Başka bir ürünün kök sertifikası kurulu.',
    kernel_helper_residue: 'Eski bir çekirdek yardımcısından kalıntı var.',
    defender_exclusions: 'Microsoft Defender dışlamaları bulundu.',
    ref_firewall_rules: 'Başka bir ürünün güvenlik duvarı kuralları var.',
    rds_residue: 'Uzak Masaüstü ayarlarında kalıntı var.',
    upstream_sunshine: 'Ayrıca kurulmuş bir Sunshine var (port çakışabilir).',
    vigem_present: 'ViGEmBus sürücüsü kurulu.'
  } as Record<string, string>,
  hdrReason: {
    force_sdr: 'koltukta SDR zorlanmış',
    rdp_display: 'Uzak Masaüstü ekranı HDR desteklemez',
    os_too_old: 'Windows sürümü çok eski',
    iddcx_too_old: 'ekran sürücüsü arayüzü eski',
    driver_no_fp16: 'sürücü HDR yüzeylerini desteklemiyor',
    gpu_unsupported: 'ekran kartı desteklemiyor',
    driver_not_installed: 'sürücü kurulu değil',
    server_needs_whql: 'Windows Server’da WHQL imzalı sürücü gerekir'
  } as Record<string, string>,
  displayBackend: {
    rdp_session: 'Uzak Masaüstü ekranı (varsayılan)',
    vdd: 'Virtual Display Driver (imzalı, yalnızca fiziksel ekran)',
    pulsar_idd: 'Pulsar sanal ekran sürücüsü (imzasız, deneme)'
  } as Record<string, string>,
  padBackend: {
    vigem: 'ViGEmBus (Xbox 360 / DualShock 4)',
    usbip_ds5: 'usbip-win2 (DualSense)',
    pulsar_pad: 'Pulsar kumanda sürücüsü (imzasız, deneme)'
  } as Record<string, string>,
  audioState: {
    capturing: 'Ses akışa ekleniyor',
    no_device: 'Oturumda ses aygıtı yok',
    blocked_by_policy: 'Grup ilkesi sesi engelliyor',
    disabled: 'Kapalı',
    unknown: 'Bilinmiyor'
  } as Record<string, string>,
  audioLimitation: {
    no_microphone: 'Bu koltuktaki uygulamalar mikrofon göremez.',
    no_controller_audio: 'Kumanda hoparlörü / kulaklık girişi desteklenmiyor.',
    steam_remote_play_unsupported: 'Steam Remote Play koltuklarda desteklenmiyor.',
    surround_is_stereo: 'Çok kanallı ses stereo olarak aktarılır.',
    policy_blocks_audio: 'Grup ilkesi ses yönlendirmeyi engelliyor.',
    mic_policy_blocked: 'Grup ilkesi mikrofon yönlendirmeyi engelliyor.',
    mic_needs_pulsar: 'Mikrofon yalnızca Pulsar istemcisiyle çalışır; Moonlight mikrofon göndermez.'
  } as Record<string, string>,
  failure: {
    seat_booting: 'Bir koltuk açılıyor; bitince yeniden deneyin.',
    dxvk_hash_mismatch: 'DXVK dosyaları beklenen sürümle eşleşmiyor; yeniden indirin.',
    backend_missing: 'Seçilen yayın sunucusu bu bilgisayarda kurulu değil.',
    backend_pulsar: 'Bu işlem Pulsar yayın sunucusunda yok.',
    backend_sunshine: 'Bu işlem Sunshine yayın sunucusunda yok.',
    edition_unsupported: 'Bu Windows sürümü çoklu koltuğu desteklemiyor.',
    coexistence: 'Başka bir çoklu koltuk ürünü bulunduğu için koltuk başlatılmadı.',
    rds_not_ready: 'Uzak Masaüstü hizmeti hazır değil.',
    seat_limit: 'Aynı anda çalışabilecek koltuk sınırına ulaşıldı.',
    account: 'Koltuk kullanıcısı kullanılamıyor.',
    password_expired: 'Koltuk kullanıcısının şifresinin süresi dolmuş.',
    rdu_membership: 'Kullanıcı “Uzak Masaüstü Kullanıcıları” grubuna eklenemedi.',
    profile: 'Kullanıcı profili yüklenemedi.',
    client_auth: 'Oturum açılamadı: kullanıcı adı ya da şifre yanlış.',
    cert_mismatch: 'Uzak Masaüstü sertifikası beklenenle eşleşmiyor.',
    session_timeout: 'Oturum zamanında açılmadı.',
    client_exited: 'Oturumu tutan yardımcı program beklenmedik şekilde kapandı.',
    token: 'Yardımcı program için güvenli bir kimlik oluşturulamadı.',
    display_timeout: 'Koltuğun ekranı zamanında hazır olmadı.',
    no_display: 'Koltuk oturumunda ekran yok.',
    driver_not_installed: 'Seçilen sanal ekran sürücüsü kurulu değil.',
    session_attach_unsupported: 'Bu ekran sürücüsü uzak oturumlarda çalışmıyor.',
    stream_server: 'Akış sunucusu tekrar tekrar çöktü.',
    server_binary_missing: 'Akış sunucusu kurulu değil.',
    server_hash_mismatch: 'Akış sunucusu dosyası doğrulanamadı.',
    port_in_use: 'Koltuğun portlarından biri kullanımda.',
    firewall: 'Güvenlik duvarı kuralları eklenemedi.',
    multi_session_off: 'Önce “Ana bilgisayar” sayfasında çoklu koltuğu açın.',
    shim_foreign: 'Başka bir ürünün işlem yamalaması açık.',
    shim_blocked: 'Windows uygulama denetimi modülün yüklenmesini engelledi.',
    server_without_control_pipe: 'Bu koltukta uygulama içinden eşleştirme kullanılamıyor.',
    no_pending_request: 'Bekleyen bir eşleştirme isteği yok.',
    wrong_pin: 'PIN yanlış.',
    pair_timeout: 'Eşleştirme süresi doldu.',
    warning_not_acknowledged: 'Önce uyarıyı okuyup onaylayın.',
    not_client_sku: 'Bu modül yalnızca Windows Pro / Enterprise içindir.',
    foreign_wrapper: 'Uzak Masaüstü hizmeti başka bir araç tarafından yönlendirilmiş.',
    unknown_termsrv_build: 'Bu Windows güncellemesi sarmalayıcı tarafından henüz desteklenmiyor.',
    wrapper_missing: 'Sarmalayıcı bulunamadı.',
    wrapper_unsigned: 'Sarmalayıcının imzası kabul edilmedi.',
    wrapper_insecure_acl: 'Sarmalayıcı klasörüne yönetici olmayanlar yazabiliyor; izinleri düzeltin.',
    wrapper_arch: 'Sarmalayıcı bu bilgisayarın mimarisine uygun değil.',
    termservice_restart: 'Uzak Masaüstü hizmeti yeniden başlatılamadı.',
    reference_product_present: 'Başka bir çoklu koltuk ürünü kurulu.',
    dxvk_not_downloaded: 'DXVK henüz indirilmedi.',
    bad_app_list: 'Uygulama listesi geçersiz.',
    unsupported: 'Bu özellik bu bilgisayarda kullanılamıyor.',
    cancelled: 'Başlatma iptal edildi.'
  } as Record<string, string>,
  experimental: {
    title: 'Deneysel özelliği aç',
    wrapper: 'Sarmalayıcı',
    ack: 'Riskleri okudum ve anladım; sorumluluğu kabul ediyorum.',
    enable: 'Aç ve Uzak Masaüstü hizmetini yeniden başlat',
    cancel: 'Vazgeç',
    working: 'Koltuklar durduruluyor ve hizmet yeniden başlatılıyor…'
  },
  newSeat: {
    title: 'Yeni uzak koltuk',
    name: 'Ad',
    hint: 'Yalnızca İngilizce harfler ve rakamlar; boşluk yok.',
    create: 'Oluştur',
    cancel: 'Vazgeç'
  },
  deleteSeat: {
    title: (n: string) => `“${n}” silinsin mi?`,
    body: 'Koltuğun ayarları ve oluşturulan dosyaları silinir.',
    warning: 'Bu işlem geri alınamaz.',
    confirm: 'Sil',
    cancel: 'Vazgeç'
  },
  pair: {
    title: (n: string) => `${n} için cihaz eşleştirme`,
    waiting: 'Cihazdan eşleştirme isteği bekleniyor…',
    pending: (d: string) => `“${d}” eşleşmek istiyor. Cihazdaki PIN’i yazın.`,
    paired: 'Cihaz eşleştirildi.',
    unavailable: 'Bu koltukta uygulama içinden eşleştirme kullanılamıyor (akış sunucusu denetim kanalı yok).',
    notRunning: 'Eşleştirme için koltuk çalışıyor olmalı.',
    cancelRequest: 'İsteği reddet',
    clients: 'Eşleşmiş cihazlar',
    noClients: 'Eşleşmiş cihaz yok.',
    unpair: 'Eşleşmeyi kaldır',
    unpairAll: 'Hepsini kaldır',
    step1: 'Cihazınızda Pulsar ya da Moonlight uygulamasını açın.',
    step2: 'Yeni ana bilgisayar ekleyin ve aşağıdaki adreslerden birini yazın.',
    step3: 'Eklenen bilgisayarı seçin; ekranda dört haneli bir PIN görünecek.',
    step4: 'PIN’i buraya yazıp gönderin.',
    addresses: 'Adresler',
    recommended: 'önerilen',
    qr: (a: string) => `Adres için QR kodu: ${a}`,
    qrHint: 'Telefonla okutarak adresi kopyalayabilirsiniz.',
    noAddress: 'Yerel ağ adresi bulunamadı.',
    pin: 'PIN',
    clientName: 'Cihaz adı',
    submit: 'PIN’i gönder',
    sent: 'PIN gönderildi.',
    close: 'Kapat',
    pinInvalid: 'PIN dört rakamdan oluşmalı.',
    // Pulsar seats: the operator approves requests instead of typing a PIN.
    pulsarStep1: 'Cihazınızda Pulsar uygulamasını açın.',
    pulsarStep2: 'Aşağıdaki Pulsar kimliğini ya da yerel ağdaki koltuk adını seçin.',
    pulsarStep3: 'Bağlantı isteği burada görünür; izin verin ya da reddedin.',
    pulsarWaiting: 'Bağlantı isteği bekleniyor…',
    requestTitle: 'Bağlantı isteği',
    requestFrom: (d: string, kind: string) => `“${d}” ${kind === 'pair' ? 'eşleşmek' : 'bağlanmak'} istiyor.`,
    keyFp: 'Anahtar parmak izi',
    passwordWrong: 'Cihaz yanlış şifre girdi.',
    allow: 'İzin ver',
    viewOnly: 'Yalnızca izle',
    deny: 'Reddet',
    answered: 'Yanıt gönderildi.',
    otp: 'Tek kullanımlık şifre',
    showOtp: 'Göster',
    refreshOtp: 'Yenile',
    connectPassword: 'Kalıcı bağlantı şifresi',
    connectPasswordHint: 'Yalnızca yazılır; kayıtlı şifre gösterilmez.',
    connectPasswordSave: 'Şifreyi kaydet',
    connectPasswordRemove: 'Şifreyi kaldır',
    connectPasswordRemoveConfirm: 'Kalıcı bağlantı şifresi kaldırılsın mı? Cihazlar bu şifreyle artık bağlanamaz.',
    connectPasswordRemoveYes: 'Evet, kaldır',
    connectPasswordRemoveNo: 'Vazgeç',
    connectPasswordSaved: 'Bağlantı şifresi kaydedildi.',
    connectPasswordCleared: 'Bağlantı şifresi kaldırıldı.',
    relayAuth: 'Aktarım sunucusu giriş istiyor',
    relayPassword: 'Aktarım sunucusu şifresi',
    relayTotp: 'Doğrulama kodu (6 hane, isteğe bağlı)',
    relayAuthSend: 'Giriş bilgilerini gönder',
    relayAuthSent: 'Giriş bilgileri gönderildi.',
    results: {
      allowed: 'Bağlantıya izin verildi.',
      allowed_view_only: 'Yalnızca izleme izni verildi.',
      denied: 'İstek reddedildi.',
      password: 'Cihaz şifreyle bağlandı.',
      paired: 'Cihaz eşleştirildi.',
      timeout: 'İsteğin süresi doldu.',
      client_gone: 'Cihaz bağlantıyı kesti.',
      locked_out: 'Çok fazla yanlış deneme; cihaz bir süre engellendi.'
    } as Record<string, string>
  },
  patching: {
    install: {
      title: 'İşlem yamalama kurulsun mu?',
      bullets: [
        'Denetleyici (HID) yalıtımını iyileştirir.',
        'Steam yalıtımını kullanılabilir hâle getirir.',
        'Uzak oturumda çalışmayı reddeden uygulamaları düzeltir.'
      ],
      question: 'Uyumluluk katmanı şimdi kurulsun mu?',
      confirm: 'Kur',
      progress: 'Uyumluluk katmanı kuruluyor…'
    },
    remove: {
      title: 'İşlem yamalama kaldırılsın mı?',
      bullets: [
        'Denetleyici (HID) yalıtımı zayıflar.',
        'Steam yalıtımı kapanır.',
        'Uzak oturumda çalışmayı reddeden uygulamalar yeniden çalışmayabilir.'
      ],
      question: 'Uyumluluk katmanı şimdi kaldırılsın mı?',
      confirm: 'Kaldır',
      progress: 'Uyumluluk katmanı kaldırılıyor…'
    },
    warning:
      'Uyumluluk katmanları Windows’un program yükleme yolunda çalışır; bir hata tüm sistemi kullanılamaz hâle getirebilir.',
    caution: 'Dikkatli olun.',
    cancel: 'Vazgeç',
    rebootTitle: 'Yeniden başlatma gerekiyor',
    rebootBody: 'Değişikliğin tam olarak etkili olması için Windows’un yeniden başlatılması gerekir.',
    rebootNow: 'Şimdi yeniden başlat',
    rebootLater: 'Sonra kendim başlatırım'
  },
  cred: {
    title: 'Kullanıcı bilgileri sınanıyor',
    pending: 'Windows oturum açma denemesi yapılıyor…',
    okTitle: 'Kullanıcı bilgileri doğru',
    okBody: 'Kayıtlı şifreyle bu kullanıcı adına oturum açılabiliyor.',
    failTitle: 'Oturum açılamadı',
    failHint: 'Kullanıcı adını ve şifreyi kontrol edip yeniden kaydedin.',
    disabled: 'Bu hesap devre dışı.',
    locked: 'Bu hesap kilitli (çok sayıda hatalı deneme).',
    expired: 'Bu hesabın şifresinin süresi dolmuş; Windows’ta yeni bir şifre belirleyin.',
    fix: 'Hesabı etkinleştir / kilidini aç',
    confirmTitle: 'Hesap değiştirilsin mi?',
    confirmBody: 'Pulsar Seat bu Windows hesabını etkinleştirecek ve kilidini açacak. Bu, hesabın kendisini değiştirir.',
    confirm: 'Evet, değiştir',
    retest: 'Yeniden sına',
    close: 'Kapat'
  },
  common: {
    examplePath: 'C:\\Oyunlar\\oyun.exe',
    exampleExe: 'oyun.exe',
    loading: 'Yükleniyor…',
    close: 'Kapat',
    error: 'Hata'
  }
};

const remoteFieldErrorText: Record<RemoteFieldError, string> = {
  name_empty: 'Bir ad yazın.',
  name_chars: 'Yalnızca İngilizce harf ve rakam kullanılabilir.',
  name_long: 'Ad en fazla 128 karakter olabilir.',
  name_duplicate: 'Bu adda bir koltuk zaten var.',
  display_name: 'Görünen ad boş olamaz, başında ya da sonunda boşluk olamaz.',
  display_name_duplicate: 'Bu görünen ad başka bir koltukta kullanılıyor.',
  port: 'Port 1029 ile 65514 arasında bir sayı olmalı.',
  port_overlap: 'Bu port aralığı başka bir koltukla çakışıyor.'
};

const remoteErrorText: Record<RemoteErrorCode, string> = {
  version_mismatch: 'Yönetici ile servisin sürümleri uyuşmuyor.',
  bad_request: 'Geçersiz istek.',
  not_found: 'Koltuk bulunamadı.',
  invalid_name: remoteFieldErrorText.name_chars,
  duplicate_name: remoteFieldErrorText.name_duplicate,
  invalid_display_name: remoteFieldErrorText.display_name,
  invalid_port: remoteFieldErrorText.port,
  port_overlap: remoteFieldErrorText.port_overlap,
  no_free_port: 'Boş port aralığı kalmadı.',
  invalid_value: 'Geçersiz değer.',
  invalid_user: 'Windows kullanıcısı kullanılamıyor (yok, devre dışı, kilitli ya da şifresinin süresi dolmuş).',
  user_in_use: 'Bu kullanıcı başka bir koltukta ya da konsolda kullanılıyor.',
  missing_credentials: 'Önce kullanıcı ve şifre girin.',
  seat_running: 'Koltuk çalışırken bu değiştirilemez.',
  not_implemented: 'Bu özellik henüz hazır değil (sonraki aşamada gelecek).',
  store: 'Ayarlar kaydedilemedi.',
  service_unavailable: 'Servise ya da koltuğun akış sunucusuna ulaşılamıyor.',
  internal: 'Beklenmeyen bir hata oluştu.',
  unsupported: 'Bu özellik bu bilgisayarda kullanılamıyor.',
  edition_unsupported: 'Bu Windows sürümü çoklu koltuğu desteklemiyor.',
  coexistence: 'Başka bir çoklu koltuk ürünü bulunduğu için işlem yapılmadı.',
  seat_failed: 'Koltuk başlatılamadı.',
  timeout: 'İşlem zaman aşımına uğradı.',
  driver_missing: 'Gerekli sürücü kurulu değil.',
  policy_blocked: 'Bir grup ilkesi bu işlemi engelliyor.',
  warning_not_acknowledged: 'Önce uyarıyı okuyup onaylayın.',
  no_pending_request: 'Bekleyen bir eşleştirme isteği yok.',
  wrong_pin: 'PIN yanlış.',
  hash_mismatch: 'Dosya doğrulanamadı.',
  firewall: 'Güvenlik duvarı ayarlanamadı.'
};

export const tr: Locale = {
  t,
  productText,
  safeModeText,
  failureText,
  errorText,
  issueText,
  noticeText,
  loopbackStateText,
  misc,
  rt,
  remoteFieldErrorText,
  remoteErrorText
};
