# Pulsar Seat — sorun giderme ve kurtarma

> Türkçe kopya. Esas metin İngilizcedir: [troubleshooting.md](troubleshooting.md). Yönetici
> uygulaması Türkçe açıksa buradaki adlar ekrandakilerle aynıdır.

Bu sayfa kullanıcılar içindir. Pulsar Seat'in kendi sürücüleri (klavye/fare süzgeci, sanal
ekran, USB depolama kapısı) **imzalanana kadar yüklenmez**; bugünkü sürümde bu adımlara
normalde gerek yoktur. Sürücüler imzalanıp kurulduktan sonra bir şey ters giderse buradaki
adımlar bilgisayarı eski hâline döndürür.

Adlar (hepsi Pulsar Seat'e aittir):

| Parça | Hizmet adı | Sürücü paketi (INF) | Dosya |
| --- | --- | --- | --- |
| Klavye / fare / HID süzgeci | `PulsarSeatInput` | `seat-input-filter.inf` | `seatinput.sys` |
| USB depolama kapısı (minifilter) | `PulsarSeatStorageGate` | `seat-storage-filter.inf` | `pulsarseatstoragegate.sys` |
| Sanal ekran (IddCx) | aygıt `Root\PulsarSeatIdd` (ek ekran kartı başına bir `SWD\PulsarSeatIdd\gpuN`; hizmet durunca kaybolur) | `seat-idd.inf` | `seat-idd.dll` |
| Pulsar Seat hizmeti | `PulsarSeat` | — | `pulsar-seat-service.exe` |
| Oturum açma bileşeni (kimlik sağlayıcı + süzgeç) | — | — | `pulsar-seat-credprov.dll` |

Sürücü deposundaki adı Windows `oem<N>.inf` olarak verir (`N` her bilgisayarda farklıdır).

Bilinen sınırlar (görüntü):

- Kopya korumalı (HDCP) görüntü 2. ve sonraki koltuklarda gösterilmez (ör. bazı film
  uygulamaları siyah ekran gösterir). Bu bir arıza değildir; bu koltukların ekranı sanal
  ekran ve Pulsar Seat'in kendi gösterimi üzerinden gelir.
- HDR ve değişken yenileme (VRR) yalnızca ekrandan "Görüntü çıkışları" penceresinde
  açılırsa ve ekran destekliyorsa çalışır. Koltuğun ekranı masaüstünden çıkarılmamışsa
  (yedek gösterim) HDR yoktur, görüntü SDR olarak gösterilir.
- Ekran kartı tercihi Windows'un grafik ayarlarındaki kaydı kullanır; koltuk kullanıcısı
  oturum açınca yazılır, Pulsar Seat kapatılınca geri alınır. Kullanıcı aynı programı
  Windows ayarlarında değiştirirse onun seçimi korunur.

## 1. Windows açılıyor: önce Pulsar Seat'i kapatın

1. Yönetici uygulamasında **Pulsar Seat'i kapatın** (ana anahtar). Bütün aygıtlar 1. koltuğa
   döner, ek koltuklar kapanır, isteğe bağlı sistem ayarları geri alınır.
2. Olmazsa yönetici olarak açılmış bir komut isteminde:

   ```bat
   "C:\Program Files\Pulsar Seat\pulsar-seat-service.exe" disable
   sc stop PulsarSeat
   ```

## 2. Windows açılıyor: sürücüleri kaldırın (pnputil)

Yönetici komut isteminde:

```bat
pnputil /enum-drivers
```

Listede **Özgün Ad** (Original Name) `seat-input-filter.inf`, `seat-storage-filter.inf` ya da
`seat-idd.inf` olan satırların **Yayımlanan Ad** (Published Name) değerini (`oem<N>.inf`) not
edin ve her biri için:

```bat
pnputil /delete-driver oem<N>.inf /uninstall
```

`/uninstall` süzgeç kaydını yalnızca Pulsar Seat'in eklediği aygıtlardan siler; başka
yazılımların süzgeçlerine dokunmaz. Ardından bilgisayarı yeniden başlatın.

## 3. Klavye ya da fare çalışmıyor ama Windows açılıyor

Klavye/fare süzgecinin bir **güvenli geçiş** anahtarı vardır: açıkken süzgeç hiçbir şeyi
yönlendirmez, her tuş ve fare hareketi normal yoluna gider.

```bat
reg add HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeatInput\Parameters /v PassThrough /t REG_DWORD /d 1 /f
```

Yeniden başlatın. Ekran klavyesi (Erişim Kolaylığı > Ekran Klavyesi) ya da uzaktan masaüstü
bu komutu yazmak için kullanılabilir. Güvenli Mod'da süzgeç zaten her şeyi geçirir.

## 4. Windows açılmıyor: Windows Kurtarma Ortamı (WinRE)

Açılış iki kez yarıda kalırsa Windows kendiliğinden kurtarma ortamını açar. Ya da kurulum
USB'sinden başlatıp **Bilgisayarınızı onarın** seçin. Sonra:
**Sorun gider > Gelişmiş seçenekler > Komut İstemi**.

Kurtarma ortamında Windows bölümünün harfi çoğu zaman `C:` değildir. Önce bulun:

```bat
dir C:\Windows
dir D:\Windows
```

Aşağıda Windows bölümü `D:` diye yazılmıştır; kendi harfinizle değiştirin.

### 4a. Önerilen yol: süzgeci güvenli geçişe alın

```bat
reg load HKLM\OFFSYS D:\Windows\System32\config\SYSTEM
reg query HKLM\OFFSYS\Select /v Current
```

`Current` değeri hangi denetim kümesinin kullanıldığını söyler (`0x1` → `ControlSet001`).

```bat
reg add HKLM\OFFSYS\ControlSet001\Services\PulsarSeatInput\Parameters /v PassThrough /t REG_DWORD /d 1 /f
reg add HKLM\OFFSYS\ControlSet001\Services\PulsarSeatStorageGate /v Start /t REG_DWORD /d 4 /f
reg unload HKLM\OFFSYS
```

Komut istemini kapatıp **Devam et** ile Windows'u başlatın. Sonra 2. adımdaki gibi sürücüleri
kaldırın.

**Uyarı:** `PulsarSeatInput` için `Start=4` (devre dışı) **yazmayın**. Bu süzgeç klavye ve
fare aygıtlarının kendi `UpperFilters` listesindedir; yüklenemeyen bir süzgeç o aygıtları
tamamen durdurur. Klavye süzgecini çevrim dışı kapatmanın güvenli yolu `PassThrough=1` ya da
aşağıdaki paket kaldırmadır. Depolama kapısı (`PulsarSeatStorageGate`) bir minifilter'dır,
onun için `Start=4` güvenlidir.

### 4b. Paketleri çevrim dışı kaldırın (dism)

```bat
dism /image:D:\ /get-drivers /format:table
```

Listede `seat-input-filter.inf`, `seat-storage-filter.inf` ya da `seat-idd.inf` olan
satırların yayımlanan adını (`oem<N>.inf`) bulun ve her biri için:

```bat
dism /image:D:\ /remove-driver /driver:oem<N>.inf
```

Çevrim dışı kaldırmada Windows aygıtların süzgeç listesini her zaman temizleyemez. Bu yüzden
`dism` ile kaldırmadan önce 4a'daki `PassThrough=1` adımını da yapın. Windows açılınca 2.
adımdaki `pnputil /delete-driver … /uninstall` komutu kalanları temizler.

### 4c. Son çare: elle süzgeç kaydını silmek

Yalnızca 4a ve 4b işe yaramazsa. Çevrim dışı kayıt defterinde (`reg load`, yukarıdaki gibi)
`HKLM\OFFSYS\ControlSet001\Enum\` altında klavye ve fare aygıtlarının `UpperFilters`
değerlerine bakın. Değerden **yalnızca** `PulsarSeatInput` satırını silin; `kbdclass`,
`mouclass` ve başka satırlara dokunmayın. `regedit` kurtarma ortamında da çalışır: **Dosya >
Kovan Yükle** ile `D:\Windows\System32\config\SYSTEM` açılabilir. İşiniz bitince kovanı
**Kovanı Kaldır** ile kapatın.

## 5. Ek koltuklar başlamıyor

- Yönetici uygulamasında bir **Tanı raporu** oluşturun. Rapor
  `%ProgramData%\Pulsar Seat\diag\` klasörüne yazılır; parola ve kullanıcı adları içermez.
- Hizmet günlüğü: `%ProgramData%\Pulsar Seat\logs\service.log`.
- Windows güncellemesinden sonra deneysel istemci oturum modülü kendiliğinden kapanabilir;
  bu durumda koltuklar hızlı kullanıcı değiştirmeye döner.
- Hızlı kullanıcı değiştirme yönteminde koltuklar bilgisayar açılırken kendiliğinden
  başlamaz (her başlatma konsolu o koltuğa geçirir). Koltuğu yönetici uygulamasından
  başlatın.
- Koltuk sunucusu (`pulsar-seat-host.exe`) güvenlik için her koltukta o koltuğa ait ayrı bir
  yerel hesapla (**`PulsarSeatHost02`**, **`PulsarSeatHost03`** …) ve düşük bütünlük düzeyinde
  çalışır; böylece bir koltuğun sunucusu başka bir koltuğunkine erişemez. Bu hesaplar
  **`PulsarSeatHosts`** adlı yerel grubun üyesidir. Koltuk silinirse hesabı hizmetin bir
  sonraki başlangıcında, Pulsar Seat kaldırılırsa hepsi silinir. Koltuğun hesabı
  kurulamazsa o koltuk ortak **`PulsarSeatHost`** hesabıyla çalışır (yerel bağlantı
  dinleyicisi de bu ortak hesabı kullanır). Koltuğa yazıcı yönlendirmesi açıksa koltuk
  sunucusu çalıştığı sürece hesabın kullanıcı profili yüklenir (`C:\Users\PulsarSeatHost02`
  gibi bir klasör oluşur). Hesabı hizmet kendisi oluşturur ve her
  başlangıçta parolasını rastgele yeniler (parola yalnızca Windows'un korumalı LSA
  deposunda durur, kimse yazmaz). Bu hesapla Windows'ta oturum açılamaz, ağdan
  bağlanılamaz; oturum açma ekranında da görünmez. Silmeyin: silinirse hizmet bir sonraki
  başlangıçta yeniden oluşturur. Hesap `Users` dışında herhangi bir yerel gruba
  (`Administrators`, `Backup Operators`, `Remote Desktop Users` gibi; doğrudan ya da bir etki
  alanı grubu üzerinden) eklenirse ya da grupları okunamazsa hizmet onu kullanmaz. Bu
  durumda hesabı o gruplardan çıkarın ve hizmeti yeniden başlatın.
- **Pano paylaşımı:** bu hesapla çalışan koltuk sunucusu panoya yalnızca koltuğun
  ayarlarında pano paylaşımı açıksa erişir. Ayarı değiştirdikten sonra koltuğu yeniden
  başlatın; değişiklik koltuk sunucusu yeniden başlayınca etkili olur.
- Bir koltuğun kendi hesabı kurulamazsa ya da koltuk sunucusu bu hesapla masaüstüne
  veya hizmete ulaşamazsa, o koltuk hizmetin o çalışması boyunca ortak `PulsarSeatHost`
  hesabına döner. Ortak hesap da kurulamazsa ya da ulaşamazsa hizmet bir önceki ayara
  (SYSTEM, düşük bütünlük düzeyi) döner. Her ikisi de `service.log` dosyasına yazılır.
- `PulsarSeatHosts` adında Pulsar'ın oluşturmadığı bir grup ya da kullanıcı varsa koltuk
  başına hesaplar kapalı kalır ve bütün koltuklar ortak hesabı kullanır; o adı
  değiştirip hizmeti yeniden başlatın.
- Oturum açma ekranında açılan (hesabı olmayan) koltuklar parolasız koltuk bağlantı
  noktasından bağlanır. Bu bağlantı noktasını yalnızca koltuklara atanmış hesaplar ve
  Gelişmiş sekmesindeki **ek hesaplar** kullanabilir. Hiç hesap yoksa bu koltuklar
  başlayamaz; yönetici uygulaması koltukta bunu uyarı olarak gösterir. Bu koltuklarda
  oturum açacak kişileri ek hesaplara yazın ya da koltuğa bir hesap verin.
- Bir bilgisayarda koltuk yine de hiç bağlanmıyorsa, yönetici komut isteminde aşağıdaki
  değerlerden birini ayarlayıp hizmeti yeniden başlatın:

  ```
  reg add HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters /v HostTokenCompat /t REG_DWORD /d 2 /f
  ```

  - `3`: koltuk başına hesap kullanılmaz; bütün koltuk sunucuları ortak `PulsarSeatHost`
    hesabıyla çalışır (önceki sürümlerin davranışı).
  - `2`: ayrı hesap kullanılmaz; koltuk sunucusu SYSTEM olarak, düşük bütünlük
    düzeyinde çalışır.
  - `1`: en eski ayar (SYSTEM, ayrıcalıksız, sistem bütünlük düzeyi). Yalnızca `2` de
    yardımcı olmazsa kullanın.

  Geri almak için aynı değeri silin (`reg delete … /v HostTokenCompat /f`) ve sorunu
  bildirin.

## 5a. Çekirdek sınırı ve kaynak IP adresi neyi kapsar?

- **Koltuğa ayrılan işlemci çekirdekleri** yalnızca Pulsar Seat'in o koltukta kendisi
  başlattığı programlara uygulanır: koltuğun başlangıç uygulamaları ve Pulsar Seat
  başlatıcısından açılan programlar (ve bunların açtığı programlar).
- **Sınırlanmayanlar:** masaüstünden, Başlat menüsünden ya da Dosya Gezgini'nden açılan
  programlar; yönetici olarak (UAC onayıyla) çalıştırılan programlar.
- Çekirdek ayarını değiştirdiğinizde çalışan koltuk yeni ayarı hemen alır. Kesin sınırı
  kaldırmak ise ancak o koltukta oturum kapatılıp yeniden açıldığında etkili olur.
- **Koltuğun kaynak IP adresi**, Pulsar Seat'in kaynak IP sürücüsü yüklüyse zorla
  uygulanır: koltuk oturumundaki programların IPv4 bağlantıları koltuğun adresinden çıkar
  (Ağ adresleri sekmesi "Kaynak IP sürücüsü yüklü" der). Sürücü Microsoft imzası alana kadar
  yüklenmez; o zamana kadar yalnızca ayarlarında "adrese bağlan" seçeneği olan ya da
  `PULSAR_SEAT_SOURCE_IP` ortam değişkenini okuyan programlar bu adresi kullanabilir.
  Değişken, Pulsar Seat'in başlattığı programlara verilir; bu programların açtığı programlar
  da kurala bakılmaksızın değişkeni devralır.
- Sürücü yüklüyken de dokunulmayanlar: programın kendisinin seçtiği adres, bu bilgisayarın
  içindeki (127.x) bağlantılar, IPv6 bağlantıları ve oturumunu başka koltuklarla paylaşan
  kiosk koltukları.
- Sürücü ağ bağlantılarını bozarsa: `HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeatWfp\Parameters`
  altında `PassThrough` değerini 1 yapıp bilgisayarı yeniden başlatın (Güvenli Mod'da sürücü
  zaten hiçbir şey yapmaz). Kaldırmak için: `pnputil /enum-drivers` ile `seat-wfp.inf`
  paketinin `oem<N>.inf` adını bulun, `sc stop PulsarSeatWfp` ve
  `pnputil /delete-driver oem<N>.inf /uninstall`.

## 5b. Parolasız koltuklar için ayrı bağlantı noktası (Windows Server)

Parolası kayıtlı olmayan (oturum açma ekranıyla başlayan) Uzak Masaüstü koltukları için
Pulsar Seat ikinci bir Uzak Masaüstü dinleyicisi kurar:

- Kayıt defteri anahtarı:
  `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\PulsarSeat-Loopback`
- Bağlantı noktası: varsayılan **33389** (Gelişmiş sekmesinden değiştirilebilir).
- Güvenlik duvarı kuralı (gelen, engelle): **`Pulsar Seat - loopback RDP (block remote) <port>`**,
  örneğin `Pulsar Seat - loopback RDP (block remote) 33389`. Eski sürümler yalnızca
  `Pulsar Seat - loopback RDP (block remote)` adını kullanıyordu.

Bu dinleyicide ağ düzeyinde kimlik doğrulaması kapalıdır; bilgisayarın dışından
bağlanılamamasını yalnızca bu güvenlik duvarı kuralı sağlar. Bu yüzden Windows Güvenlik
Duvarı bir profilde kapalıysa ya da bir grup ilkesi yerel kuralları yok sayıyorsa Pulsar Seat
dinleyiciyi kaldırır. Uzak Masaüstü koltukları açıkken Uzak Masaüstü Hizmetleri yeniden
başlatılamaz; kaldırılan dinleyici o zamana kadar çalışmaya devam eder. Yönetici
uygulaması bunu gösterir: Uzak Masaüstü koltuklarını kapatın ya da bilgisayarı yeniden
başlatın.

Pulsar Seat kaldırıldıktan sonra anahtar ya da kural kaldıysa, yönetici komut isteminde:

```
reg delete "HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\PulsarSeat-Loopback" /f
netsh advfirewall firewall delete rule name="Pulsar Seat - loopback RDP (block remote) 33389"
netsh advfirewall firewall delete rule name="Pulsar Seat - loopback RDP (block remote)"
```

Bağlantı noktasını değiştirdiyseniz kural adındaki `33389` yerine o numarayı yazın. Kalan
kuralları görmek için: `netsh advfirewall firewall show rule name=all | findstr /C:"Pulsar Seat"`.
**Önce anahtarı silin, sonra bilgisayarı yeniden başlatın, kuralı en son silin**: kural
silinip dinleyici hâlâ çalışıyorsa bağlantı noktası ağa açık kalır.

Windows açılmıyorsa (kurtarma ortamı, `reg load HKLM\OFFSYS D:\Windows\System32\config\SYSTEM`):
`HKLM\OFFSYS\ControlSet001\Control\Terminal Server\WinStations\PulsarSeat-Loopback`
anahtarını silin, `reg unload HKLM\OFFSYS` ile kapatın. Güvenlik duvarı kuralı Windows
açıldıktan sonra yukarıdaki `netsh` komutuyla silinir.

## 6. Pulsar Seat'i tamamen kaldırmak

**Ayarlar > Uygulamalar** içinden kaldırın. Kaldırma; hizmeti, sürücü paketlerini, kayıtlı
koltuk parolalarını, koltuk sunucusunun `PulsarSeatHost` hesabını (oturum hakları ve
parolasıyla birlikte) ve Pulsar Seat'in yaptığı sistem ayarlarını (kilit ekranı görseli, hızlı
kullanıcı değiştirmeyi gizleme, sürücü güncellemelerini dışarıda bırakma, güç ayarları) geri
alır. Aygıtlara, aygıt sınıflarına ve yazıcılara verilen erişim izinleri de eski hâline
döndürülür; geri alınamayan bir şey kalırsa kaldırma bunu ekrana yazar. Kendi verilerinizin de silinmesini kaldırma sırasında ayrıca seçebilirsiniz.

Kaldırma, hesabı silmeden önce hizmeti durdurur ve koltuk sunucularına açık oturumların
masaüstünde verilmiş izinleri geri alır. Hizmet durdurulamazsa hesap silinmez; hizmeti
durdurup kaldırmayı yeniden çalıştırın. Geri alınamayan bir masaüstü izni, o oturum
kapanınca kendiliğinden kalkar.

## 7. Oturum açma ekranında kutucuklar yok ya da yanlış (kimlik sağlayıcı)

Pulsar Seat, "tek seferde bir koltuk" (hızlı kullanıcı değiştirme) kipinde koltuğun
kayıtlı parolasıyla kendiliğinden oturum açmak için oturum açma ekranına küçük bir bileşen
ekler (`pulsar-seat-credprov.dll`). Bu bileşen **yalnızca** Pulsar Seat açıkken ve bu kip
kullanılırken kayıtlıdır; Pulsar Seat'i kapatınca ya da kaldırınca kaydı silinir.

Bileşen kendini güvenli tarafta tutar: Pulsar Seat hizmeti yanıt vermezse hiçbir kutucuğu
gizlemez ve kendisi de görünmez. Diğer oturum açma seçeneklerini (parola, PIN) yalnızca
bir koltuk kendiliğinden oturum açarken, yalnızca o koltuğun oturumunda gizler. Kilit
ekranında asla kendiliğinden oturum açmaz.

**Kayıtlı parola kabul edilmezse** ("Kayıtlı parola kabul edilmedi. Parolayı girin."):
Pulsar Seat parolayı bir daha denemez. Aynı ekranda PIN, Windows Hello ya da "Diğer
kullanıcı" kutucukları gizli kalabilir. Koltuk hesabının parolasını yazın, "Koltuk
değiştir" bağlantısını kullanın ya da Ctrl+Alt+Del'e basıp (veya ekranı kilitleyip)
yeni bir oturum açma ekranı açın: yeni ekranda bütün kutucuklar yeniden görünür. Kayıtlı
parolayı Pulsar Seat yöneticisinde yeniden girin.

Yine de oturum açma ekranında hiç kutucuk yoksa ya da Pulsar Seat kutucuğu takılı kalırsa:

### 7a. Windows açılıyor ve başka bir hesapla girebiliyorsunuz

Yönetici olarak açılmış bir komut isteminde:

```bat
"C:\Program Files\Pulsar Seat\pulsar-seat-service.exe" disable
```

Bileşen kalıcı olarak kapalı kalsın istiyorsanız (Pulsar Seat açık olsa bile):

```bat
reg add "HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters" /v CredProvDisabled /t REG_DWORD /d 1 /f
sc stop PulsarSeat
sc start PulsarSeat
```

Geri almak için aynı değeri silin
(`reg delete "HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters" /v CredProvDisabled /f`).

### 7b. Güvenli mod

Güvenli modda Pulsar Seat hizmeti çalışmaz; bileşen hizmete ulaşamadığı için hiçbir şeyi
gizlemez ve görünmez. Normal Windows oturum açma ekranı gelir. Oradan 7a'daki komutları
çalıştırın. (Güvenli moda geçmek için oturum açma ekranında **Shift** basılıyken
**Yeniden başlat**, sonra **Sorun gider > Gelişmiş seçenekler > Başlangıç Ayarları**.)

### 7c. Elle kayıt silmek (çalışan Windows'ta)

Yönetici komut isteminde Pulsar Seat'e ait yalnızca şu dört anahtarı silin; başka
anahtarlara dokunmayın:

```bat
reg delete "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Provider Filters\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
reg delete "HKLM\SOFTWARE\Classes\CLSID\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\SOFTWARE\Classes\CLSID\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
```

### 7d. Windows'a hiç girilemiyorsa: kurtarma ortamından (WinRE)

§4'teki gibi kurtarma ortamında komut istemini açın ve Windows bölümünü bulun (aşağıda
`D:`). Kayıt defterinin SOFTWARE kovanını yükleyip yalnızca Pulsar Seat anahtarlarını
silin, sonra kovanı kaldırın:

```bat
reg load HKLM\OFFSOFT D:\Windows\System32\config\SOFTWARE
reg delete "HKLM\OFFSOFT\Microsoft\Windows\CurrentVersion\Authentication\Credential Provider Filters\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\OFFSOFT\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
reg delete "HKLM\OFFSOFT\Classes\CLSID\{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}" /f
reg delete "HKLM\OFFSOFT\Classes\CLSID\{C086BD57-C066-4FBA-ACED-7679C157E3DC}" /f
reg unload HKLM\OFFSOFT
```

Bir anahtar "bulunamadı" derse sorun değil; o parça zaten yoktur. Sonra bilgisayarı
yeniden başlatın. Kayıt silindiğinde Windows'un kendi oturum açma ekranı eksiksiz geri
gelir.


## 8. Görüntü bozuk, donuk ya da siyah ekran (H.264)

Koltuk görüntüsü sunucudan grafik hattıyla (EGFX) gelir; çoğunlukla H.264 olarak
kodlanır ve ekran kartında çözülür. Bazı sürücülerde bu yol bozuk renkler, donan bölgeler
ya da siyah ekran verebilir. Koltuk sunucusu (`pulsar-seat-host.exe`) aşağıdaki ortam
değişkenlerini okur. Bunları **sistem ortam değişkeni** olarak ayarlayın (yönetici komut
istemi), sonra koltuğu yönetici uygulamasından yeniden başlatın (değer koltuk sunucusu
her başladığında okunur). Önce birincisini deneyin, yetmezse ikincisini ekleyin:

```
setx /M PULSAR_SEAT_H264_DECODER software
setx /M PULSAR_SEAT_GFX no-h264
```

- `PULSAR_SEAT_H264_DECODER=software`: H.264 ekran kartı yerine işlemcide çözülür. İlk
  deneyin; görüntü düzelirse sorun ekran kartı sürücüsündedir.
- `PULSAR_SEAT_GFX=no-h264`: grafik hattı kalır ama H.264 istenmez (RemoteFX ve benzeri
  kodlayıcılar kullanılır).
- `PULSAR_SEAT_GFX=bitmap` (ya da `off`): grafik hattı hiç açılmaz, eski bitmap
  güncellemeleri kullanılır. En yavaş ama en uyumlu yol.
- `PULSAR_SEAT_GFX_ZERO_COPY=0`: ekran kartında çözülen görüntü işlemciye hiç kopyalanmadan
  doğrudan ekrana verilir ("sıfır kopya" yolu). Görüntü yalnızca bu yol açıkken bozuluyorsa
  (yanlış renkler, titreyen ya da eski kalan bölgeler, imleç izi) bu değişkenle yolu
  kapatın; H.264 yine ekran kartında çözülür, görüntü eskisi gibi işlemci üzerinden
  gelir.
- Geri almak için değişkeni silin (`reg delete "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" /v PULSAR_SEAT_GFX /f`)
  ve koltuğu yeniden başlatın.

Koltuk sunucusu günlüğünde (`%ProgramData%\Pulsar Seat\logs\host-NN.log`, NN = koltuk
numarası, örneğin `host-02.log`) şu satırı arayın:

```
seat host: gfx avc444 v0xa0701 decoder=hardware frames=… h264=… errors=… pic_dropped=… dropped=… resyncs=… fallbacks=… …
```

- İlk sözcük kullanılan yoldur: `avc444` / `avc420` (H.264), `egfx` (H.264'süz grafik
  hattı), `bitmap` (grafik hattı yok).
- `decoder=`: `hardware` (ekran kartı), `software` (işlemci), `unavailable` (H.264
  çözücü başlamadı), `none` (henüz H.264 yok).
- `gpu=`: sıfır kopya yolu. `ready` (açık), `off` (bu çıkış için yok ya da kapalı),
  `failed` (başlamadı ya da çalışırken hata verdi; bu bağlantı boyunca işlemci yolu).
  `gpu_frames` ekran kartından ekrana giden kare sayısıdır; `demotions` sürekli artıyorsa
  sunucu bu yolun desteklemediği şeyler gönderiyor demektir (sorun değil, görüntü işlemci
  yolundan gelir).
- `present_skipped`, `acks_held` artıyorsa ekran yetişemiyor; koltuk sunucusu bu sırada
  kareleri birleştirir ve sunucudan yavaşlamasını ister. `acks_forced` sürekli artıyorsa
  ekran uzun süre takılı kalıyor demektir; günlüğü sorun bildirimine ekleyin.
- `errors`, `pic_dropped`, `dropped` artıyorsa görüntünün bir kısmı çizilemedi; koltuk
  sunucusu bu durumda sunucudan ekranı yeniden ister (`resyncs`). `fallbacks` 0'dan
  büyükse ekran kartıyla çözme sürekli başarısız olduğu için çalışırken işlemciye
  geçildi.
- Sayılar sürekli artıyor ve görüntü düzelmiyorsa yukarıdaki ayarları deneyin ve
  günlüğü sorun bildirimine ekleyin.


## 9. Kiosk koltukları: fare bir alana kilitli, pencere öne gelmiyor

Kiosk koltukları (aynı Windows oturumunda birden çok koltuk) çalışırken 1. koltuk da kiosk
koltuğuysa Pulsar Seat sistem imlecini bir "odak alanına" kilitler ve öne gelen pencereyi
sabitler. Kiosk kabuğu çökerse ya da hizmet yanıt vermezse fare bu alanda kalabilir ya da
Windows pencereleri öne gelmeyebilir.

1. Konsol klavyesinde **Ctrl+Alt+Del**'e basın, sonra **İptal**'i seçin. Windows bu
   sırada imleç kilidini kaldırır.
2. Kiosk koltuğunda ana ekrana dönmek için alt çubuktaki düğmeyi ya da
   **Ctrl+Alt+Home**'u kullanın (Ctrl+Alt+Del kiosk koltuğunda bütün koltukları etkiler).
3. Sorun sürüyorsa yönetici komut isteminde kiosk koltuklarını kapatın; hizmet durunca
   bütün kiosk kabukları ve içlerindeki uygulamalar kapanır:

```
"C:\Program Files\Pulsar Seat\pulsar-seat-service.exe" disable
sc stop PulsarSeat
```

4. Kilit hâlâ kalkmadıysa oturumu kapatıp yeniden açın; imleç kilidi ve ön plan kilidi
   oturumla birlikte biter.

Bilinen sınırlar: ham giriş, DirectInput ya da XInput kullanan oyunlar koltuğun kendi
aygıtlarını yalnızca uygulama başına yalıtım açıksa alır. Tam ekran oyunlar için kiosk
uygulamasında "Tam ekran" seçin; bilgisayarda aynı anda yalnızca bir oyun gerçek tam
ekranda olabilir. Mağaza (UWP / MSIX) uygulamaları uygulama kimliğiyle (AUMID) eklenir ve
her Windows sürümünde açılmayabilir. Giriş süzgeci kurulu değilse Pulsar kiosk
klavyelerinin sistem tuşlarını aygıta göre ayırıp 1. koltuktan uzak tutmaya çalışır; kiosk
farelerinin hareketi 1. koltuğun imlecini yine oynatabilir (yönetici uygulamasında "Giriş
tam yalıtılmıyor" notu). Ctrl+Alt+Del ve Win+L hiçbir kullanıcı kipi programıyla
engellenemez.

## 9a. Kiosk koltuğu kendi kullanıcısıyla açılmıyor / eski ayarlar kayboldu

2026-11-17'den beri 2. ve sonraki kiosk koltukları kendi Windows kullanıcısıyla çalışır:
koltuğa hesap ve parola kayıtlıysa o hesapla, değilse Pulsar'ın oluşturduğu gizli yerel
hesapla (`PulsarKiosk02` …). Bu hesaplar oturum açma ekranında görünmez ve Uzak Masaüstü
ya da ağ üzerinden oturum açamaz.

1. Güncellemeden önce kiosk uygulamalarının ayarları ve verileri 1. koltuğun kullanıcısının
   profilindeydi; yeni koltuk kullanıcısının profili boş başlar. Bu veriler taşınmaz;
   gerekiyorsa uygulamanın kendi dışa / içe aktarma özelliğini kullanın.
2. Yönetici uygulamasında "Bir kiosk koltuğunun kendi kullanıcısı açılamadı" notu varsa o
   koltuk 1. koltuğun kullanıcısıyla çalışıyordur. Hizmet günlüğünde (`logs\service.log`)
   `could not be logged on` satırına bakın: kayıtlı parola yanlışsa koltuğun hesabını ve
   parolasını yeniden girin; `PulsarKiosk..` adında Pulsar'ın oluşturmadığı bir hesap
   varsa onu yeniden adlandırın.
3. Eski davranışa dönmek için Temel sekmesinde "Her kiosk koltuğu kendi Windows
   kullanıcısıyla çalışsın" seçeneğini kapatın. Bir koltuk kiosk koltuğu olmaktan çıkınca
   Pulsar'ın o koltuktaki hesabı hizmetin sonraki açılışında yalnızca **devre dışı
   bırakılır**; profili (kayıtlar, tarayıcı profilleri) kalır. Kalıcı olarak silmek için
   koltuk menüsündeki "Koltuğun kiosk kullanıcısını sil…" komutunu kullanın. Kaldırmada
   hesaplar silinir; profilleri yalnızca "ayarları ve günlükleri de sil" seçilirse
   (`REMOVEUSERDATA=1`). Seçeneği kapatmak hiçbir şeyi silmez.
4. Kiosk koltukları bir güvenlik sınırı değildir: bütün kiosk koltukları tek bir Windows
   masaüstünü paylaşır, bir koltuktaki program diğerlerinin klavyesini, ekranını ve
   panosunu okuyabilir (normal pano çağrılarını kullanan uygulamalar için Pulsar'ın koltuk
   başına panosu vardır). Bir kiosk koltuğunda kişisel ya da iş hesabıyla oturum açmayın;
   koltuğun kayıtlı hesabı kullanılırsa yönetici uygulaması uyarır.

## 10. Kiosk koltuğunun sesi kendi aygıtından çıkmıyor

Kiosk koltuğuna yalnızca ona atanmış bir ses aygıtı (hoparlör, kulaklık, mikrofon)
verildiyse, kiosk kabuğu koltuğun her uygulamasına bu aygıtı "uygulama başına varsayılan
aygıt" olarak verir (Windows 10 1803 ve sonrası). Yönetici uygulamasında Kiosk sütununun
altındaki "Ses:" satırı koltuğun aygıtını gösterir.

1. "koltuğa ayrı ses aygıtı atanmamış" yazıyorsa ses aygıtını yalnızca bu koltuğa atayın
   (birden çok koltuğa atanmış aygıt sayılmaz).
2. Koltuğun alt çubuğunda "uygulama başına ses aygıtı yok" ya da "yönlendirilemedi"
   yazıyorsa Windows bu ayarı kabul etmedi; ses 1. koltuğun varsayılan aygıtından çıkar.
   Bu ayar belgelenmemiş bir Windows arabirimiyle yapılır ve bir Windows güncellemesi onu
   bozabilir.
3. Uygulama sesi hâlâ eski aygıttan çalıyorsa uygulamayı "Oturumu kapat" ile yeniden
   başlatın: bazı uygulamalar yeni aygıtı yalnızca yeni açtıkları seste kullanır.
4. Aynı programı iki kiosk koltuğunda çalıştırmayın: Windows bu ayarı program başına
   saklar, iki koltuk aynı aygıtı paylaşır (yönetici uygulaması bunu uyarır).
5. Windows Ayarlar > Sistem > Ses > Uygulama ses düzeyi ve cihaz tercihleri sayfasında
   kalmış bir kayıt varsa oradan "Sıfırla" ile temizleyebilirsiniz.

## 10a. Steam ya da başka bir program yalnızca bir koltukta çalışıyor

- **Steam:** *Gelişmiş > Uyumluluk > Her koltukta Steam* seçeneğini açın, `steam.exe`'yi
  işlem başına yama hedef listesine ekleyin ve her koltuğa kendi Steam hesabını verin. 1.
  koltuk Steam'in olağan davranışını korur. Tek bir Steam kurulumunu paylaşan iki koltuk
  oyun kitaplığını da paylaşır: aynı oyunu iki koltukta aynı anda kurmak ya da güncellemek
  başarısız olabilir. Kendi kitaplığı gereken koltuk kendi Steam kurulum klasörünü alır (o
  `steam.exe`'yi koltuğun başlangıç uygulamalarına ekleyin).
- **Eski Steam eklentisi:** ön denetim listesinde *Eski Steam eklenti DLL'si* görünürse,
  `steam.exe`'nin yanında eski bir "multibox" aracından kalma bir `version.dll` vardır.
  *Gelişmiş > Uyumluluk > Eski Steam eklenti DLL'si > Ara* ile onu Pulsar Seat'in karantina
  klasörüne taşıyabilir (ve geri koyabilirsiniz). Pulsar Seat onu asla silmez.
- **Diğer tek kopyalı programlar:** programı *Tek kopyalı programlar* altına, çalışan bir
  kopyayı bulmak için kullandığı nesnenin adıyla ekleyin (adı programın üreticisi ya da
  belgeleri verir; Sysinternals Handle gibi araçlar adlandırılmış nesneleri gösterir).
  Program işlem başına yama hedef listesinde de olmalıdır. Hile koruması olan oyunları asla
  eklemeyin. Bilgisayar genelinde tek bir hizmetle konuşan programlar bu yolla ayrılamaz.
- **Koltuğa göre değişmesi gereken ayarlar:** koltuk menüsündeki *Ortam ve oturum açma…*
  yalnızca o koltuğun kullanıcısı için ortam değişkenleri ayarlar (ör. bir oyunun bir
  değişkenden okuduğu profil klasörü).

## 10b. Koltuk bilgisayarında Windows Update, hızlı başlatma, Defender

Üçü de isteğe bağlıdır ve varsayılan olarak kapalıdır (*Gelişmiş > Uyumluluk*). Her biri
yalnızca belgelenmiş ayarları yazar, kaydedilir ve kapattığınızda, Pulsar Seat'i
kapattığınızda ya da kaldırdığınızda geri gelir. Windows Update ayarları güncellemeleri
erteler ve etkin saatleri ayarlar; güncellemeleri asla engellemez, Windows Home da bu
ayarları yok sayar. Defender dışlamaları `defender_unavailable` bildirirse başka bir
virüsten koruma ürünü etkindir: dışlamaları orada ekleyin.

## 11. Dil

Yönetici uygulaması Windows görüntüleme dilini izler (İngilizce; Türkçe Windows'ta Türkçe).
Elle seçmek için: **Temel ayarlar > Görünüm > Dil**; yönetici seçilen dilde yeniden yüklenir.
Seçim kullanıcı başınadır. Hizmetin ya da kurulumun yazdığı metinler (Olay Günlüğü, koltuk
aracısının bildirim alanı menüsü, kiosk çubuğu, oturum açma kutucukları, hizmet açıklaması)
onları yazan işlemin Windows arayüz dilini izler (HYPOTHESIS: LocalSystem olarak çalışan
hizmet için bu, bilgisayarın varsayılan arayüz dilidir). Bunlar için tek bir dili zorlamak
isterseniz `PULSAR_SEAT_LANG` sistem ortam değişkenini `en` ya da `tr` yapıp bilgisayarı
yeniden başlatın (hizmetler sistem ortam değişkenlerini yalnızca Windows açılırken okur).
MSI İngilizcedir; Türkçe MSI `installer\scripts\build-installer.ps1 -Language tr-TR` ile
derlenir.

## 12. Uzak koltuk: "Kendi ekranı kullanılmadı" ya da mod / HDR değişmiyor

Uzak koltuklar Pulsar sanal ekranını (mod ve HDR denetimiyle) yalnızca **Ayarlar > Sanal
ekran yöntemi** "Pulsar sanal ekran sürücüsü" olduğunda kullanır. Pulsar Seat o zaman kendi
Uzak Masaüstü protokol sağlayıcısını kaydeder (`PulsarSeat-Idd` adlı bir dinleyici;
standart `RDP-Tcp` dinleyicisi değiştirilmez). Bu yol kullanılamazsa koltuk yine Uzak
Masaüstü ekranında açılır ve koltuk sayfası nedenini "Kendi ekranı kullanılmadı" altında
gösterir:

- *kayıtlı; Uzak Masaüstü hizmetini ya da bilgisayarı yeniden başlatın*: Windows yeni bir
  dinleyiciyi yalnızca Uzak Masaüstü hizmeti başlarken yükler. Koltukları durdurup
  bilgisayarı yeniden başlatın.
- *sağlayıcı dosyası eksik ya da başka bir işlemci için*: kurulumu onarın.
- *Pulsar ekran sürücüsü yüklü değil*: sürücü henüz imzalı değil (Windows Server WHQL imzalı
  sürücü ister); koltuk Uzak Masaüstü ekranında çalışmaya devam eder.

Uzak Masaüstü ekranında mod istemciye uyar; "Şimdiki ekran modu" "bu ekran mod
değiştiremez" yanıtını verir.

Ayarlar ne derse desin sağlayıcıyı kapalı tutmak için (örneğin kayıttan sonra Uzak
Masaüstü hizmeti sorun çıkarırsa):

```bat
reg add "HKLM\SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters" /v WrdsDisabled /t REG_DWORD /d 1 /f
sc stop PulsarSeat
sc start PulsarSeat
```

Hizmet başlarken dinleyicisini ve COM kaydını kaldırır. Geri almak için değeri silip
hizmeti yeniden başlatın. Pulsar Seat kaldırıldığında ikisi de silinir.
