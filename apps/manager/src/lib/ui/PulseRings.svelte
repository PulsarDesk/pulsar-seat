<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- A seat number inside neutral pulse rings: the monitor face (48px) and the wizard's
     per-monitor page (220px). Identity comes from the number, never from a colour. -->
<script lang="ts">
  let { number, size = 48 }: { number: number | string; size?: number } = $props();
</script>

<svg class="rings" width={size} height={size} viewBox="0 0 52 52" fill="none" aria-hidden="true" focusable="false">
  <circle cx="26" cy="26" r="24.5" stroke="currentColor" stroke-width="1.25" opacity=".28" />
  <circle cx="26" cy="26" r="18" stroke="currentColor" stroke-width="1.5" opacity=".6" />
  <circle cx="26" cy="26" r="12" stroke="currentColor" stroke-width="2" />
  <text x="26" y="32" text-anchor="middle">{number}</text>
</svg>

<style>
  .rings {
    display: block;
    flex: none;
    color: var(--text-muted);
  }
  text {
    font-family: var(--font-display);
    font-weight: 600;
    font-size: var(--fs-h3);
    fill: var(--text);
  }
</style>
