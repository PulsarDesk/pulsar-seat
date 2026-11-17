<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- One Lucide icon (icons.ts), one stroke weight (1.75; 2 at 14px, 1.5 at 40px). Always
     aria-hidden: the visible text or the button's aria-label names the thing, never the icon. -->
<script lang="ts">
  import { ICONS, type IconName } from './icons.ts';

  interface Props {
    name: IconName;
    /** 14 (inline meta), 16 (buttons, rows), 18 (default), 40 (empty state). */
    size?: 14 | 16 | 18 | 40;
    /** Colour role: `ok`, `warn`, `danger` (status icons only); default is the text colour. */
    tone?: 'ok' | 'warn' | 'danger';
    spin?: boolean;
    class?: string;
  }
  let { name, size = 18, tone, spin = false, class: cls = '' }: Props = $props();
  const stroke = $derived(size === 14 ? 2 : size === 40 ? 1.5 : 1.75);
</script>

<svg
  class="icon {cls}"
  class:xs={size === 14}
  class:sm={size === 16}
  class:xl={size === 40}
  class:ok={tone === 'ok'}
  class:warn={tone === 'warn'}
  class:danger={tone === 'danger'}
  class:spin
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  {#each ICONS[name] as [tag, attrs], i (i)}
    <svelte:element this={tag} {...attrs} />
  {/each}
</svg>

<style>
  .icon {
    flex: none;
    display: inline-block;
    vertical-align: middle;
  }
</style>
