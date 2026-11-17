<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- QR code drawn as SVG from the pure TS encoder (#lib/qr.ts); dark on white in both
     themes so phone cameras read it (`--qr-ink` on `--qr-paper`). No network. -->
<script lang="ts">
  import { encodeQr, qrPath } from '#lib/remote/qr.ts';

  interface Props {
    text: string;
    label: string;
    size?: number;
  }
  let { text, label, size = 168 }: Props = $props();
  const qr = $derived.by(() => {
    try {
      return encodeQr(text);
    } catch {
      return null;
    }
  });
</script>

{#if qr}
  <svg
    class="qr"
    role="img"
    aria-label={label}
    width={size}
    height={size}
    viewBox="0 0 {qr.size + 8} {qr.size + 8}"
    shape-rendering="crispEdges"
  >
    <rect width={qr.size + 8} height={qr.size + 8} class="paper" />
    <path d={qrPath(qr)} class="ink" />
  </svg>
{/if}

<style>
  .qr {
    display: block;
  }
  .paper {
    fill: var(--qr-paper);
  }
  .ink {
    fill: var(--qr-ink);
  }
</style>
