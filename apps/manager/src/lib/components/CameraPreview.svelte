<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Webcam indication (spec 10 §3.5.1, ARM64 notes: WebView getUserMedia): a live preview
     of one camera, chosen by matching the device name to the WebView's video inputs. The
     stream stops when the window closes. Untested on hardware. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { t } from '#lib/i18n.ts';

  interface Props {
    name: string;
    onClose: () => void;
  }
  let { name, onClose }: Props = $props();
  let video: HTMLVideoElement | undefined = $state();
  let phase = $state<'starting' | 'live' | 'denied' | 'unsupported'>('starting');
  let fallback = $state(false);
  let stream: MediaStream | null = null;

  /** Best match of a device friendly name among the media device labels. */
  function pick(list: MediaDeviceInfo[]): MediaDeviceInfo | undefined {
    const n = name.toLowerCase();
    return list.find((d) => d.label.toLowerCase() === n) ?? list.find((d) => d.label && (d.label.toLowerCase().includes(n) || n.includes(d.label.toLowerCase())));
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
        let s = await md.getUserMedia({ video: true, audio: false });
        const cams = (await md.enumerateDevices()).filter((d) => d.kind === 'videoinput');
        const want = pick(cams);
        const cur = s.getVideoTracks()[0]?.getSettings().deviceId;
        if (want && want.deviceId !== cur) {
          s.getTracks().forEach((x) => x.stop());
          s = await md.getUserMedia({ video: { deviceId: { exact: want.deviceId } }, audio: false });
        }
        fallback = !want;
        if (stopped) {
          s.getTracks().forEach((x) => x.stop());
          return;
        }
        stream = s;
        if (video) video.srcObject = s;
        phase = 'live';
      } catch {
        phase = 'denied';
      }
    })();
    return () => {
      stopped = true;
      stream?.getTracks().forEach((x) => x.stop());
      stream = null;
    };
  });
</script>

<Modal title={t.camera.title(name)} wide {onClose}>
  {#if phase === 'starting'}<p class="hint" role="status">{t.camera.starting}</p>{/if}
  {#if phase === 'denied'}<p class="error" role="alert">{t.camera.denied}</p>{/if}
  {#if phase === 'unsupported'}<p class="error" role="alert">{t.camera.unsupported}</p>{/if}
  {#if fallback}<p class="hint">{t.camera.notFound}</p>{/if}
  <!-- svelte-ignore a11y_media_has_caption -->
  <video bind:this={video} autoplay playsinline muted class:hidden={phase !== 'live'}></video>
  {#snippet buttons()}
    <button type="button" onclick={onClose}>{t.close}</button>
  {/snippet}
</Modal>

<style>
  video {
    width: 100%;
    max-height: 55vh;
    background: #000;
    border-radius: 6px;
  }
  .hidden {
    display: none;
  }
</style>
