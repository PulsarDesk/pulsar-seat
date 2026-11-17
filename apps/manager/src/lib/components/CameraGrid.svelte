<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seat-level camera indication (spec/local 10 §3.5.1): the live picture of every camera at
     once (system menu), or of the cameras a seat uses (seat menu). One WebView stream per
     camera, all stopped when the window closes. Untested on hardware. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from '#lib/i18n.ts';
  import { gridInputs } from '#lib/cameras.ts';
  import { Button, Modal, Notice, Spinner } from '#lib/ui/index.ts';

  interface Props {
    title: string;
    /** Camera names to show (a seat's); null = every camera. */
    names: string[] | null;
    onClose: () => void;
  }
  let { title, names, onClose }: Props = $props();

  interface Tile {
    id: string;
    label: string;
    stream: MediaStream | null;
    failed: boolean;
  }
  let tiles = $state<Tile[]>([]);
  let phase = $state<'starting' | 'live' | 'denied' | 'unsupported'>('starting');
  const streams: MediaStream[] = [];

  function attach(node: HTMLVideoElement, s: MediaStream | null) {
    node.srcObject = s;
    return { update: (n: MediaStream | null) => (node.srcObject = n) };
  }

  onMount(() => {
    const md = globalThis.navigator?.mediaDevices;
    if (!md?.getUserMedia) {
      phase = 'unsupported';
      return;
    }
    let stopped = false;
    (async () => {
      try {
        // Labels are only visible after a permission grant: open any camera first.
        const first = await md.getUserMedia({ video: true, audio: false });
        first.getTracks().forEach((x) => x.stop());
        const inputs = (await md.enumerateDevices()).filter((d) => d.kind === 'videoinput');
        const pick = gridInputs(inputs, names);
        tiles = pick.map((d) => ({ id: d.deviceId, label: d.label, stream: null, failed: false }));
        phase = 'live';
        for (const [i, d] of pick.entries()) {
          try {
            const s = await md.getUserMedia({ video: { deviceId: { exact: d.deviceId } }, audio: false });
            if (stopped) {
              s.getTracks().forEach((x) => x.stop());
              return;
            }
            streams.push(s);
            tiles[i].stream = s;
          } catch {
            tiles[i].failed = true;
          }
        }
      } catch {
        phase = 'denied';
      }
    })();
    return () => {
      stopped = true;
      for (const s of streams) s.getTracks().forEach((x) => x.stop());
      streams.length = 0;
    };
  });
</script>

<Modal {title} size="lg" {onClose}>
  {#if phase === 'denied'}<Notice kind="danger" text={t.camera.denied} />{/if}
  {#if phase === 'unsupported'}<Notice kind="danger" text={t.camera.unsupported} />{/if}
  {#if phase === 'starting'}
    <span class="wait" role="status"><Spinner />{t.camera.starting}</span>
  {:else if phase === 'live' && tiles.length === 0}
    <p class="help">{t.camera.none}</p>
  {:else if phase === 'live'}
    <ul class="grid" aria-label={title}>
      {#each tiles as tile (tile.id)}
        <li class="tile">
          <div class="media">
            {#if tile.failed}
              <span class="err">{t.camera.busy}</span>
            {:else if !tile.stream}
              <span class="wait"><Spinner /></span>
            {:else}
              <!-- svelte-ignore a11y_media_has_caption -->
              <video use:attach={tile.stream} autoplay playsinline muted></video>
            {/if}
          </div>
          <span class="name">{tile.label}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#snippet foot()}
    <Button label={t.close} onclick={onClose} />
  {/snippet}
</Modal>

<style>
  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: var(--sp-3);
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .media {
    aspect-ratio: 16 / 9;
    border-radius: var(--r);
    background: var(--media-bg);
    color: var(--media-ink);
    display: grid;
    place-items: center;
    overflow: hidden;
  }
  .name {
    font-size: var(--fs-help);
    line-height: var(--lh-help);
    color: var(--text-muted);
  }
  .wait {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .err {
    font-size: var(--fs-help);
    padding: var(--sp-2);
    text-align: center;
  }
  video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    background: var(--media-bg);
  }
</style>
