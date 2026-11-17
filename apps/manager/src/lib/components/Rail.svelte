<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The rail (DESIGN.md "Navigation"): the brand, three places (Seats, Remote seats,
     Settings; counts as plain muted numbers), then Status at the foot as a plain item with an
     8px dot (green = service running, amber = reconnecting, red = unreachable; the state is
     also visually hidden text) and the version line. No sub-items, no service card, no menu.
     232px; 188px from 900 to 1100px; below 900px 44px icon items with the label in the
     tooltip and the accessibility tree, counts as badges, the dot stays. -->
<script lang="ts">
  import { t } from '#lib/i18n.ts';
  import type { Tab } from '#lib/prefs.ts';
  import { Icon, Mark, type IconName } from '#lib/ui/index.ts';

  interface Props {
    current: Tab;
    /** Local seats (the seats board count) and remote seats; `null` hides the count. */
    seatCount: number | null;
    remoteCount: number | null;
    service: 'ok' | 'link' | 'down';
    version: string | null;
    /** The seat this manager runs on (shown under the version). */
    ownSeat: number | null;
    onNavigate: (tab: Tab) => void;
  }
  let { current, seatCount, remoteCount, service, version, ownSeat, onNavigate }: Props = $props();

  const places: { tab: Tab; label: string; icon: IconName; count: () => number | null }[] = [
    { tab: 'seats', label: t.nav.seats, icon: 'seats', count: () => seatCount },
    { tab: 'remote', label: t.nav.remote, icon: 'cast', count: () => remoteCount },
    { tab: 'settings', label: t.nav.settings, icon: 'sliders', count: () => null }
  ];
  const here = (tab: Tab) => current === tab || (tab === 'settings' && current === 'advanced');
  const serviceText = $derived(service === 'ok' ? t.nav.statusOk : service === 'down' ? t.nav.statusDown : t.nav.statusLink);

  function go(e: MouseEvent, tab: Tab) {
    // Plain left click: navigate in place (the unsaved-changes guard runs in App).
    if (e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    onNavigate(tab);
  }
</script>

<aside class="rail" aria-label={t.nav.app}>
  <a class="brand" href="#seats" aria-label={t.appTitle} onclick={(e) => go(e, 'seats')}>
    <Mark size={26} /><span>Pulsar Seat</span>
  </a>
  <nav class="nav" aria-label={t.nav.sections}>
    {#each places as p (p.tab)}
      {@const n = p.count()}
      <a href="#{p.tab}" title={p.label} aria-current={here(p.tab) ? 'page' : undefined} onclick={(e) => go(e, p.tab)}>
        <Icon name={p.icon} />
        <span class="label">{p.label}</span>
        {#if n !== null && n > 0}<span class="count" aria-label={t.nav.count(n)}>{n}</span>{/if}
      </a>
    {/each}
  </nav>
  <nav class="nav foot" aria-label={t.nav.status}>
    <a href="#status" title={t.nav.status} aria-current={current === 'status' ? 'page' : undefined} onclick={(e) => go(e, 'status')}>
      <Icon name="activity" />
      <span class="label">{t.nav.status}</span><span class="sr-only">{serviceText}</span>
      <span class="dot {service}" aria-hidden="true"></span>
    </a>
    {#if version || ownSeat}
      <p class="version">
        {#if version}{t.nav.version(version)}{/if}
        {#if ownSeat}<br />{t.title.onSeat(ownSeat)}{/if}
      </p>
    {/if}
  </nav>
</aside>

<style>
  .rail {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    padding: var(--sp-5) var(--sp-3) var(--sp-4);
    background: var(--surface-2);
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 var(--sp-2);
    color: var(--text);
    text-decoration: none;
  }
  .brand span {
    font-family: var(--font-display);
    font-weight: 600;
    font-size: var(--fs-h2);
    line-height: 1;
    letter-spacing: -0.02em;
    white-space: nowrap;
  }
  .nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav a {
    position: relative;
    display: flex;
    align-items: center;
    gap: 11px;
    min-height: 40px;
    padding: 0 var(--sp-3);
    border-radius: var(--r-sm);
    color: var(--text-muted);
    font-weight: 500;
    text-decoration: none;
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .nav a:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .nav a[aria-current='page'] {
    background: var(--accent-soft);
    color: var(--accent-ink);
    font-weight: 600;
  }
  .label {
    min-width: 0;
  }
  .count {
    margin-left: auto;
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    color: var(--text-muted);
    font-weight: 500;
  }
  .dot {
    margin-left: auto;
    width: 8px;
    height: 8px;
    border-radius: var(--r-pill);
    flex: none;
    background: var(--ok);
  }
  .dot.link {
    background: var(--warn);
  }
  .dot.down {
    background: var(--danger);
  }
  .foot {
    margin-top: auto;
  }
  /* Contrast themes drop the dot's background: keep its colour (the text still says it). */
  @media (forced-colors: active) {
    .dot {
      forced-color-adjust: none;
      border: 1px solid CanvasText;
    }
  }

  .version {
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    color: var(--text-faint);
    padding: var(--sp-2) var(--sp-3) 0;
  }

  @media (max-width: 1100px) {
    .rail {
      padding-inline: var(--sp-2);
    }
  }
  @media (max-width: 899px) {
    .rail {
      align-items: center;
    }
    .brand {
      padding: 0;
    }
    .brand span,
    .label,
    .version {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip-path: inset(50%);
      white-space: nowrap;
    }
    .nav a {
      width: 44px;
      height: 44px;
      padding: 0;
      justify-content: center;
    }
    .count {
      position: absolute;
      top: 2px;
      right: 2px;
      min-width: 16px;
      padding: 0 4px;
      border-radius: var(--r-pill);
      background: var(--surface-3);
      color: var(--text);
      text-align: center;
    }
    .dot {
      position: absolute;
      top: 8px;
      right: 8px;
    }
  }
</style>
