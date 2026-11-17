<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Dev-only gallery of the component kit (src/lib/ui/), for visual review in light and dark:
     `bun run dev` then /ui (?theme=dark, ?lang=tr, ?show=modal|confirm|menu|pick).
     Production builds render nothing here (the `dev` guard); P8 deletes or keeps it. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { dev } from '$app/env';
  import { applyLocale, t } from '#lib/i18n.ts';
  import {
    Breadcrumb,
    Button,
    Checkbox,
    ChoiceList,
    ConfirmDialog,
    Disclosure,
    EmptyState,
    Field,
    Group,
    Icon,
    IconButton,
    Kbd,
    LinkRow,
    ListTable,
    LockedValue,
    Mark,
    Menu,
    Modal,
    Notice,
    Notices,
    PageHead,
    PulseRings,
    Row,
    SaveBar,
    Section,
    Segmented,
    Select,
    Spinner,
    Status,
    StatusLine,
    SubLabel,
    Switch,
    Toasts,
    Toc,
    ICON_NAMES,
    type MenuEntry,
    type ToastItem
  } from '#lib/ui/index.ts';

  let ready = $state(false);
  let show = $state<string | null>(null);
  let auto = $state(true);
  let start = $state<'manual' | 'boot' | 'logon'>('boot');
  let theme = $state<'system' | 'light' | 'dark'>('system');
  let same = $state<'allow' | 'warn' | 'deny'>('warn');
  let dontAsk = $state(false);
  let name = $state('Çocuk odası');
  let toasts = $state<ToastItem[]>([{ id: 1, text: 'Ayarlar kaydedildi.' }]);
  let menuOpen = $state(false);

  onMount(() => {
    const q = new URLSearchParams(location.search);
    if (q.get('lang') === 'tr' || q.get('lang') === 'en') applyLocale(q.get('lang') as 'tr' | 'en');
    const th = q.get('theme');
    if (th === 'dark' || th === 'light') {
      document.documentElement.dataset.theme = th;
      theme = th;
    }
    show = q.get('show');
    ready = true;
    if (show === 'menu') menuOpen = true;
  });

  const seatMenu: MenuEntry[] = [
    { label: 'Bu koltuğu başlat' },
    { label: 'Yeniden oturum aç' },
    { separator: true },
    { label: 'Adını değiştir…' },
    { label: 'Oturum hesabı…' },
    { label: 'Kiosk koltuğu…' },
    { label: 'IP adresi…' },
    { label: 'İşlemci çekirdekleri…' },
    { separator: true },
    { label: 'Koltuğu sil…', danger: true }
  ];
  const moveMenu: MenuEntry[] = [
    { group: 'Taşı' },
    { label: '1. koltuk' },
    { label: 'Misafir', hint: '2. koltuk' },
    { label: 'Çocuk odası', hint: '3. koltuk' },
    { label: 'Boştaki aygıtlara' },
    { label: 'Bütün koltuklarla paylaş' },
    { separator: true },
    { label: 'Göster (tanıt)' },
    { label: 'Resmini değiştir…' },
    { label: 'Aygıt bilgileri…' },
    { separator: true },
    { label: 'Windows ayarları', submenu: [{ label: 'Aygıt Yöneticisi' }, { label: 'Ekran ayarları' }, { label: 'Ses ayarları' }] }
  ];
</script>

<svelte:head><title>UI kit · Pulsar Seat</title></svelte:head>

{#if !dev}
  <p class="page">Not available in this build.</p>
{:else if ready}
  <div class="page gallery">
    <Breadcrumb items={[{ label: 'Ayarlar', href: '#' }, { label: 'Bileşenler' }]} />
    <PageHead title="Bileşenler">
      {#snippet status()}
        <StatusLine kind="ok" status="Pulsar Seat açık" summary="2 yerel koltuk çalışıyor, Çocuk odası başlatılamıyor">
          {#snippet action()}<Button compact label="Koltukları başlat" />{/snippet}
        </StatusLine>
      {/snippet}
      {#snippet actions()}
        <Button kind="primary" icon="plus" label="Koltuk ekle" />
        <Menu items={moveMenu} label="USB klavye: taşı ya da bilgi" bind:open={menuOpen} />
      {/snippet}
    </PageHead>

    <Notices
      items={[
        { kind: 'warn', text: 'Takılı aygıtlar kayıtlı yapılandırmadan farklı: 1 aygıt takılı değil, 1 aygıt yeni.', action: { label: 'Karşılaştır', onclick: () => {} } },
        { kind: 'info', text: 'Windows güncellendi. Koltuk ayarlarını gözden geçirip gerekirse yeniden kaydedin.', action: { label: 'Tamam', onclick: () => {} } }
      ]}
    />

    <div class="with-toc">
      <Toc
        items={[
          { id: 'buttons', label: 'Düğmeler' },
          { id: 'status', label: 'Durum' },
          { id: 'rows', label: 'Satırlar' },
          { id: 'choices', label: 'Seçimler' },
          { id: 'fields', label: 'Alanlar' },
          { id: 'notices', label: 'Bildirimler' },
          { id: 'misc', label: 'Diğer' }
        ]}
      />
      <div>
        <Section id="buttons" title="Düğmeler" help="Birincil, ikincil, sessiz, tehlikeli ve tehlikeli-sessiz; normal ve sıkı boy.">
          <div class="btnrow">
            <Button kind="primary" label="Kaydet" />
            <Button label="Geri al" />
            <Button kind="quiet" label="Hepsini sıfırla" />
            <Button kind="danger" label="Koltuğu sil" />
            <Button kind="danger-quiet" label="Sıfırla" />
            <Button compact label="Yeniden başlat" />
            <Button compact kind="primary" label="Aç ve başlat" />
            <Button kind="primary" label="PIN’i gönder" disabledReason="İstek gelince PIN’i yazabilirsiniz." />
            <Button busy label="Kaydediliyor" />
          </div>
          <div class="btnrow after">
            <IconButton icon="more" label="Misafir: koltuk işlemleri" aria-haspopup="menu" />
            <IconButton icon="copy" label="Adresi kopyala" />
            <IconButton icon="x" label="Kapat" />
            <IconButton icon="trash" label="Kaldır: satır 1" />
            <Menu items={seatMenu} label="Çocuk odası: koltuk işlemleri" />
            <Menu items={seatMenu} label="Koltuklar: diğer işlemler" triggerText="Diğer" />
          </div>
        </Section>

        <Section id="status" title="Durum" help="Nokta ve kelime; renk yalnızca uyarı ve hatada.">
          <div class="btnrow wide">
            <Status kind="ok" text="Çalışıyor" />
            <Status kind="off" text="Kapalı" />
            <Status kind="warn" text="Başlatılamıyor" />
            <Status kind="danger" text="Kilitli" />
            <Status kind="ok" plainOk text="Hazır" />
          </div>
          <div class="after"><StatusLine kind="off" status="Pulsar Seat kapalı" summary="Koltuklar ayarlandı. Açınca başlatma biçimine göre çalışırlar.">
            {#snippet action()}<Button compact kind="primary" label="Aç ve başlat" />{/snippet}
          </StatusLine></div>
        </Section>

        <Section id="rows" title="Koltuklar" help="Kendiliğinden başlayan koltuklar yukarıdaki başlatma biçimini izler.">
          <Group>
            <Row label="1. koltuk" help="Oturum açma: Ayşe · 2 başlangıç programı">
              {#snippet control()}
                <LockedValue text="Her zaman açılır" />
                <Button compact label="Hesap" aria-label="Oturum hesabı: 1. koltuk" />
                <Menu items={seatMenu} label="1. koltuk: diğer koltuk ayarları" />
              {/snippet}
            </Row>
            <Row label="Misafir" help="Oturum açma ekranı · Kiosk: 3 uygulama">
              {#snippet control()}
                <span class="value" aria-hidden="true">Kendiliğinden başlar</span>
                <Switch bind:checked={auto} label="Kendiliğinden başlasın: Misafir" />
                <Button compact label="Hesap" aria-label="Oturum hesabı: Misafir" />
                <Menu items={seatMenu} label="Misafir: diğer koltuk ayarları" />
              {/snippet}
            </Row>
            <Row label="Aynı hesap birden fazla koltukta" help="Aynı hesabı iki koltukta açmak dosya ve ayarları bozabilir.">
              {#snippet control()}
                <Select
                  label="Aynı hesap birden fazla koltukta"
                  bind:value={same}
                  options={[
                    { value: 'allow', label: 'İzin ver' },
                    { value: 'warn', label: 'İzin ver ama uyar' },
                    { value: 'deny', label: 'İzin verme' }
                  ]}
                />
              {/snippet}
            </Row>
            <Row label="Tema">
              {#snippet control()}
                <Segmented
                  label="Tema"
                  bind:value={theme}
                  onchange={(v) => (v === 'system' ? delete document.documentElement.dataset.theme : (document.documentElement.dataset.theme = v))}
                  options={[
                    { value: 'system', label: 'Sistem' },
                    { value: 'light', label: 'Açık' },
                    { value: 'dark', label: 'Koyu' }
                  ]}
                />
              {/snippet}
            </Row>
            <LinkRow label="Gelişmiş ayarlar" help="Çalışma yöntemleri, zamanlamalar, ağ adresleri, işlemci çekirdekleri." href="#rows" />
            <LinkRow label="Salon" help="Pulsar ile akış · yayında: Salon TV" href="#rows">
              {#snippet value()}<Status kind="ok" text="Çalışıyor" />{/snippet}
            </LinkRow>
            <Row label="Giriş tam yalıtılmıyor" help="Diğer koltukların tuşları 1. koltuğun öndeki penceresine de ulaşabilir.">
              {#snippet lead()}<Icon name="alert" size={16} tone="warn" />{/snippet}
            </Row>
          </Group>
        </Section>

        <Section id="choices" title="Başlatma" help="Pulsar Seat açıkken ek koltuklar ne zaman başlasın?">
          <ChoiceList
            name="start"
            label="Başlatma biçimi"
            bind:value={start}
            options={[
              { value: 'manual', label: 'Elle', note: 'ilk kurulumda önerilir', help: 'Koltuklar yalnızca “Koltukları başlat” düğmesiyle ya da bildirim alanından başlar. En güvenlisi.' },
              { value: 'boot', label: 'Windows açılınca', help: 'Bilgisayar açılırken bütün koltuk ekranlarının açık olması gerekir.' },
              { value: 'logon', label: 'Eşzamanlı tam oturumlar', unavailable: 'Kullanılamaz: yalnızca Windows Server x64 ve Uzak Masaüstü Oturumu Ana Bilgisayarı rolüyle.' }
            ]}
          />
          <SubLabel text="Diğer yöntemler" />
          <Group>
            <Row label="Sınıf düzeyinde varsayılan erişim tanımları" help="Gerçek donanımda denenene kadar kapalı tutuluyor.">
              {#snippet control()}<LockedValue text="Henüz kullanılamaz" />{/snippet}
            </Row>
            <Row label="Koltuk başlatma zaman aşımı" help="30 ile 120 saniye arası.">
              {#snippet control()}<input class="input narrow" value="60" inputmode="numeric" aria-label="Koltuk başlatma zaman aşımı, saniye" /><span class="unit">sn</span>{/snippet}
            </Row>
            <Row label="Klavye/fare geçiş kısayolu" help="Bu kısayola basan klavye bir sonraki koltuğa geçer.">
              {#snippet control()}<Kbd keys="Ctrl+F12" /><Button compact label="Değiştir" aria-label="Geçiş kısayolunu değiştir" />{/snippet}
            </Row>
          </Group>
        </Section>

        <Section id="fields" title="Alanlar">
          <Group>
            <Row label="Program listesi" help="Bütün koltuklar için ortak." stack>
              {#snippet control()}
                <ListTable
                  columns={[
                    { key: 'on', label: 'Etkin', width: '56px' },
                    { key: 'title', label: 'Başlık' },
                    { key: 'path', label: 'Yol' },
                    { key: 'x', label: 'Kaldır', srOnly: true, end: true }
                  ]}
                >
                  <tr>
                    <td><Checkbox tight checked ariaLabel="Etkin: Oyun istemcisi" /></td>
                    <td>Oyun istemcisi</td>
                    <td class="mono">C:\Oyunlar\istemci.exe</td>
                    <td class="end"><IconButton icon="trash" label="Kaldır: Oyun istemcisi" /></td>
                  </tr>
                  <tr>
                    <td><Checkbox tight checked={false} ariaLabel="Etkin: LAN oyunları" /></td>
                    <td>LAN oyunları</td>
                    <td class="mono">D:\LAN\</td>
                    <td class="end"><IconButton icon="trash" label="Kaldır: LAN oyunları" /></td>
                  </tr>
                </ListTable>
                <div class="btnrow"><Button compact icon="plus" label="Program ekle" /><Button compact kind="quiet" icon="folder" label="Klasördeki programları ekle" /></div>
              {/snippet}
            </Row>
          </Group>
          <div class="fields two after">
            <Field label="Ad" help="En fazla 64 karakter.">
              {#snippet children({ id, describedby })}<input class="input" {id} aria-describedby={describedby} bind:value={name} />{/snippet}
            </Field>
            <Field label="Parola (tekrar)" error="Parolalar aynı değil.">
              {#snippet children({ id, describedby, invalid })}<input class="input" type="password" {id} aria-describedby={describedby} aria-invalid={invalid} value="secret" />{/snippet}
            </Field>
          </div>
          <div class="after">
            <Checkbox label="Kullanıcı adlarını gizle" help="Önerilen." bind:checked={dontAsk} />
            <Disclosure title="Sınırlar" help="Kiosk koltuğu tam bir kilit değildir">
              <p class="help">Bütün koltuklar aynı Windows oturumunu ve panoyu paylaşır. Ctrl+Alt+Del ve Win+L bütün koltukları güvenlik ekranına geçirir.</p>
            </Disclosure>
          </div>
        </Section>

        <Section id="notices" title="Bildirimler">
          <div class="stack">
            <Notice kind="info" text="Dokunarak bul: bulmak istediğiniz klavyede bir tuşa basın ya da fareyi oynatın.">
              {#snippet actions()}<Button compact label="Aramayı bitir" />{/snippet}
            </Notice>
            <Notice kind="warn" text="Bu düzende diğer koltukların klavye girişleri 1. koltuğun öndeki penceresine de ulaşabilir." />
            <Notice kind="danger" text="Hizmetle bağlantı koptu, yeniden bağlanılıyor…" />
            <Notice kind="ok" text="Rapor kaydedildi." />
            <Notice kind="neutral" icon="lock" text="Koltuk çalışırken ayarları değiştirilemez. Değiştirmek için koltuğu durdurun." />
          </div>
        </Section>

        <Section id="misc" title="Diğer" help="Simgeler, tuşlar, halkalar, bekleme, boş durum.">
          <div class="btnrow wide">
            <Mark size={26} />
            <PulseRings number={2} />
            <Kbd keys="Shift+Esc" />
            <Kbd keys="Enter" size="lg" />
            <Spinner label="Cihazdan eşleştirme isteği bekleniyor…" />
          </div>
          <div class="icons after">
            {#each ICON_NAMES as n (n)}<span title={n}><Icon name={n} /></span>{/each}
          </div>
          <EmptyState icon="alert-circle" danger title="Hizmete ulaşılamıyor" body="Pulsar Seat hizmeti çalışmıyor ya da bir güvenlik yazılımı onu durdurmuş olabilir.">
            {#snippet actions()}<Button kind="primary" icon="play" label="Hizmeti başlat" /><Button label="Tanı raporu" />{/snippet}
          </EmptyState>
        </Section>
      </div>
    </div>
  </div>

  <div class="dock">
    <SaveBar count={1} detail="Kulaklık mikrofonu Misafir koltuğuna taşındı. Kaydedince Misafir koltuğu yeniden başlar." onSave={() => {}} onUndo={() => {}} />
  </div>
  <Toasts items={toasts} raised onDismiss={(id) => (toasts = toasts.filter((x) => x.id !== id))} duration={600000} />

  {#if show === 'modal'}
    <Modal title="USB kulaklık: koltuk ataması" sub="Bu aygıt birden fazla koltukta kullanılabilir." onClose={() => (show = null)}>
      <Field label="Ad" help="En fazla 64 karakter.">
        {#snippet children({ id, describedby })}<input class="input" {id} aria-describedby={describedby} bind:value={name} />{/snippet}
      </Field>
      {#snippet foot()}
        <Button kind="primary" label={t.save} onclick={() => (show = null)} />
        <Button label={t.cancel} onclick={() => (show = null)} />
      {/snippet}
    </Modal>
  {:else if show === 'confirm'}
    <ConfirmDialog
      title="Çocuk odası silinsin mi?"
      body="Bu koltuğun aygıtları boşa çıkarılır. Değişiklik kaydedilince geçerli olur."
      confirmLabel="Koltuğu sil"
      cancelLabel="Vazgeç"
      danger
      onConfirm={() => (show = null)}
      onCancel={() => (show = null)}
    />
  {:else if show === 'pick'}
    <Modal title="Salon için cihaz eşleştir" sub="Bir kez eşleşen cihaz sonra doğrudan bağlanır." size="lg" onClose={() => (show = null)}>
      <Spinner label="Cihazdan eşleştirme isteği bekleniyor…" />
      {#snippet footStart()}İstek gelince PIN’i yazabilirsiniz.{/snippet}
      {#snippet foot({ reasonId })}
        <Button kind="primary" label="PIN’i gönder" disabled {reasonId} />
        <Button label="Kapat" onclick={() => (show = null)} />
      {/snippet}
    </Modal>
  {/if}
{/if}

<style>
  .gallery {
    padding-bottom: calc(var(--savebar-h) + var(--sp-8));
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .icons {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-3);
    color: var(--text-muted);
    margin-bottom: var(--sp-5);
  }
  .dock {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
  }
</style>
