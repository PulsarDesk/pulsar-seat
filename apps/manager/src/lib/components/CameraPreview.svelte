<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Webcam indication (spec 10 §3.5.1, ARM64 notes: WebView getUserMedia): a live preview
     of one camera, chosen by matching the device name to the WebView's video inputs. The
     stream stops when the window closes. Untested on hardware. Redesign 59-dlg-camera: the
     picture sits on the media tokens (`--media-bg` / `--media-ink`) in both themes. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from '#lib/i18n.ts';
  import { matchCamera } from '#lib/cameras.ts';
  import { Button, Modal, Notice, Spinner } from '#lib/ui/index.ts';

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
  const pick = (list: MediaDeviceInfo[]) => matchCamera(list, name);

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

<Modal title={t.camera.title(name)} size="md" {onClose}>
  {#if phase === 'denied'}<Notice kind="danger" text={t.camera.denied} />{/if}
  {#if phase === 'unsupported'}<Notice kind="danger" text={t.camera.unsupported} />{/if}
  {#if fallback}<p class="help">{t.camera.notFound}</p>{/if}
  <div class="media">
    {#if phase === 'starting'}<span class="wait" role="status"><Spinner />{t.camera.starting}</span>{/if}
    <!-- svelte-ignore a11y_media_has_caption -->
    <video bind:this={video} autoplay playsinline muted class:hidden={phase !== 'live'}></video>
  </div>
  {#snippet foot()}
    <Button label={t.close} onclick={onClose} />
  {/snippet}
</Modal>

<style>
  .media {
    aspect-ratio: 16 / 9;
    border-radius: var(--r);
    background: var(--media-bg);
    color: var(--media-ink);
    display: grid;
    place-items: center;
    overflow: hidden;
  }
  .wait {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    background: var(--media-bg);
  }
  .hidden {
    display: none;
  }
</style>
