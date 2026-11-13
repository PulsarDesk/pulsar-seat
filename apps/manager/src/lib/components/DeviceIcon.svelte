<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Our own simple line pictures per device category (spec 10 §3.1; CLEANROOM: no third-party art).
     `active = false` draws the "not in use" variant (unplugged / switched off). `picture`
     picks a built-in alternative (`builtin:<id>`, lib/pictures.ts); `userUrl` draws a user
     file instead, scaled to the cell without keeping its aspect ratio (spec 10 §3.1). -->
<script lang="ts">
  import { parsePicture } from '#lib/pictures.ts';
  import type { DeviceKind } from '#lib/types.ts';

  interface Props {
    kind: DeviceKind;
    active?: boolean;
    size?: number;
    picture?: string;
    userUrl?: string | null;
  }
  let { kind, active = true, size = 40, picture, userUrl = null }: Props = $props();
  const ref = $derived(parsePicture(picture));
  const id = $derived(ref.type === 'builtin' ? ref.id : '');
</script>

{#if userUrl}
  <img src={userUrl} width={size} height={size} alt="" class:inactive={!active} aria-hidden="true" />
{:else}
  <svg
    width={size}
    height={size}
    viewBox="0 0 40 40"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    class:inactive={!active}
    aria-hidden="true"
    data-picture={id || kind}
  >
    {#if id === 'monitor-speakers'}
      <rect x="9" y="7" width="22" height="18" rx="2" />
      <path d="M16 33h8M20 25v8" />
      <rect x="2" y="12" width="5" height="12" rx="1" />
      <rect x="33" y="12" width="5" height="12" rx="1" />
    {:else if id === 'monitor-curved'}
      <path d="M4 9q16 4 32 0v16q-16 4-32 0z" />
      <path d="M15 34h10M20 29v5" />
    {:else if id === 'tv'}
      <rect x="3" y="8" width="34" height="21" rx="2" />
      <path d="M10 34l4-5M30 34l-4-5" />
    {:else if id === 'laptop'}
      <rect x="8" y="8" width="24" height="17" rx="1.5" />
      <path d="M3 30h34l-3-5H6z" />
    {:else if id === 'keyboard-compact'}
      <rect x="7" y="13" width="26" height="14" rx="2" />
      <path d="M11 18h2M16 18h2M21 18h2M26 18h2M14 23h12" />
    {:else if id === 'keyboard-numpad'}
      <rect x="2" y="12" width="36" height="16" rx="2" />
      <path d="M6 17h2M11 17h2M16 17h2M21 17h2M10 23h12M29 16v8M33 16v8" />
    {:else if id === 'keyboard-laptop'}
      <path d="M4 14h32l3 14H1z" />
      <path d="M8 18h2M13 18h2M18 18h2M23 18h2M28 18h2M13 23h14" />
    {:else if id === 'mouse-wireless'}
      <rect x="12" y="9" width="16" height="26" rx="8" />
      <path d="M20 9v8M14 5a8 8 0 0 1 12 0" />
    {:else if id === 'mouse-vertical'}
      <path d="M14 34c-4-6-4-20 2-26 5-4 10 2 10 10v16z" />
      <path d="M20 9v8" />
    {:else if id === 'trackball'}
      <path d="M5 30c0-10 8-16 15-16s15 6 15 16z" />
      <circle cx="20" cy="22" r="5" />
    {:else if id === 'touchpad'}
      <rect x="6" y="8" width="28" height="24" rx="3" />
      <path d="M6 26h28M20 26v6" />
      <circle cx="17" cy="16" r="1.5" fill="currentColor" />
    {:else if id === 'trackpoint'}
      <rect x="4" y="12" width="32" height="16" rx="2" />
      <circle cx="20" cy="20" r="3" fill="currentColor" />
    {:else if id === 'pen'}
      <path d="M28 5l7 7-19 19-9 2 2-9z" />
      <path d="M24 9l7 7" />
    {:else if id === 'touchscreen'}
      <rect x="5" y="6" width="30" height="22" rx="2" />
      <path d="M18 34v-9a2 2 0 0 1 4 0v4l4 1v4" />
    {:else if id === 'gamepad'}
      <path d="M8 14h24a5 5 0 0 1 5 5l1 9a3 3 0 0 1-5 2l-4-4H11l-4 4a3 3 0 0 1-5-2l1-9a5 5 0 0 1 5-5z" />
      <path d="M11 19v5M8.5 21.5h5" />
      <circle cx="28" cy="20" r="1" fill="currentColor" />
      <circle cx="31" cy="23" r="1" fill="currentColor" />
    {:else if id === 'headphones'}
      <path d="M7 24v-4a13 13 0 0 1 26 0v4" />
      <rect x="4" y="23" width="7" height="11" rx="2" />
      <rect x="29" y="23" width="7" height="11" rx="2" />
    {:else if id === 'speaker-single'}
      <rect x="11" y="4" width="18" height="32" rx="2" />
      <circle cx="20" cy="24" r="5" />
      <circle cx="20" cy="11" r="2" />
    {:else if id === 'headset'}
      <path d="M7 22v-3a13 13 0 0 1 26 0v3" />
      <rect x="4" y="21" width="6" height="10" rx="2" />
      <rect x="30" y="21" width="6" height="10" rx="2" />
      <path d="M7 31q2 5 10 5h3" />
    {:else if id === 'webcam-clip'}
      <rect x="8" y="6" width="24" height="14" rx="4" />
      <circle cx="20" cy="13" r="3.5" />
      <path d="M12 20l-2 6h20l-2-6M4 30h32" />
    {:else if kind === 'monitor'}
      <rect x="5" y="7" width="30" height="20" rx="2" />
      <path d="M15 33h10M20 27v6" />
      {#if active}<path d="M9 11h22v12H9z" fill="currentColor" opacity="0.15" stroke="none" />{/if}
    {:else if kind === 'keyboard'}
      <rect x="3" y="12" width="34" height="16" rx="2" />
      <path d="M8 17h2M13 17h2M18 17h2M23 17h2M28 17h2M8 22h2M28 22h2M13 23h12" />
    {:else if kind === 'mouse'}
      <rect x="12" y="6" width="16" height="28" rx="8" />
      <path d="M20 6v9" />
    {:else if kind === 'hid'}
      <rect x="6" y="9" width="28" height="22" rx="3" />
      <path d="M6 24h28M20 24v7" />
    {:else if kind === 'audio_render'}
      <rect x="4" y="10" width="13" height="22" rx="2" />
      <rect x="23" y="10" width="13" height="22" rx="2" />
      <circle cx="10.5" cy="24" r="3.5" />
      <circle cx="29.5" cy="24" r="3.5" />
      <circle cx="10.5" cy="15" r="1.5" />
      <circle cx="29.5" cy="15" r="1.5" />
    {:else if kind === 'audio_capture'}
      <rect x="15" y="5" width="10" height="18" rx="5" />
      <path d="M10 19a10 10 0 0 0 20 0M20 29v6M14 35h12" />
    {:else if kind === 'webcam'}
      <circle cx="20" cy="17" r="10" />
      <circle cx="20" cy="17" r="4" />
      <path d="M13 35h14M20 27v8" />
    {:else if kind === 'smart_card_reader'}
      <rect x="6" y="16" width="28" height="16" rx="2" />
      <rect x="12" y="6" width="16" height="14" rx="1" />
      <path d="M15 10h4" />
    {:else if kind === 'usb_hub'}
      <rect x="4" y="14" width="32" height="12" rx="3" />
      <path d="M10 20h2M16 20h2M22 20h2M28 20h2" />
    {:else if kind === 'storage'}
      <rect x="12" y="4" width="16" height="32" rx="3" />
      <path d="M16 4v6h8V4" />
    {:else if kind === 'printer'}
      <path d="M11 15V5h18v10" />
      <rect x="5" y="15" width="30" height="12" rx="2" />
      <path d="M11 23h18v12H11z" />
    {:else if kind === 'serial_port'}
      <path d="M6 13h28l-4 14H10z" />
      <path d="M13 18h1M19 18h1M25 18h1M16 22h1M22 22h1" />
    {:else if kind === 'network'}
      <path d="M6 15a20 20 0 0 1 28 0M11 20a13 13 0 0 1 18 0M16 25a6 6 0 0 1 8 0" />
      <circle cx="20" cy="30" r="1.5" fill="currentColor" />
    {:else}
      <rect x="8" y="8" width="24" height="24" rx="4" />
      <path d="M16 20h8M20 16v8" />
    {/if}
    {#if !active}<path d="M6 34L34 6" stroke-width="2.5" />{/if}
  </svg>
{/if}

<style>
  svg {
    display: block;
    color: var(--text);
  }
  svg.inactive {
    color: var(--muted);
  }
  img {
    display: block;
    object-fit: fill;
  }
  img.inactive {
    opacity: 0.55;
  }
</style>
