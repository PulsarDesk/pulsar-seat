<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Sticky in-page contents (Advanced settings): a nav of in-page links with
     aria-current="location" on the section in view. Hidden below 1100px, where the section
     headings carry the page. Place it in a `.with-toc` grid next to the sections. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from '#lib/i18n.ts';
  import type { TocItem } from './types.ts';

  interface Props {
    items: TocItem[];
    current?: string;
    label?: string;
  }
  let { items, current = $bindable(items[0]?.id), label }: Props = $props();

  onMount(() => {
    if (typeof IntersectionObserver === 'undefined') return;
    const seen = new Map<string, boolean>();
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) seen.set(e.target.id, e.isIntersecting);
        const first = items.find((i) => seen.get(i.id));
        if (first) current = first.id;
      },
      { rootMargin: '0px 0px -60% 0px' }
    );
    for (const i of items) {
      const el = document.getElementById(i.id);
      if (el) io.observe(el);
    }
    return () => io.disconnect();
  });

  function go(e: MouseEvent, id: string) {
    const el = document.getElementById(id);
    if (!el) return;
    e.preventDefault();
    current = id;
    const smooth = !globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    el.scrollIntoView?.({ behavior: smooth ? 'smooth' : 'auto', block: 'start' });
    // Move keyboard focus and screen readers to the chosen section (the default fragment
    // navigation, cancelled above, would have done that; WCAG 2.4.3, review 2026-11-17).
    const target = (el.querySelector('h2, h1') as HTMLElement | null) ?? el;
    if (!target.hasAttribute('tabindex')) target.setAttribute('tabindex', '-1');
    target.focus({ preventScroll: true });
  }
</script>

<nav class="toc" aria-label={label ?? t.nav.onThisPage}>
  {#each items as i (i.id)}
    <a href="#{i.id}" aria-current={current === i.id ? 'location' : undefined} onclick={(e) => go(e, i.id)}>{i.label}</a>
  {/each}
</nav>

<style>
  .toc {
    position: sticky;
    top: var(--sp-5);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  a {
    display: inline-flex;
    align-items: center;
    min-height: var(--h-compact);
    padding: 6px var(--sp-3);
    border-radius: var(--r-xs);
    color: var(--text-muted);
    text-decoration: none;
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  a:hover {
    color: var(--text);
    background: var(--surface-3);
  }
  a[aria-current] {
    color: var(--text);
    background: var(--surface-3);
    font-weight: 600;
  }
  @media (max-width: 1100px) {
    .toc {
      display: none;
    }
  }
</style>
