<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Seat avatar (spec 10 §2.1): the seat user's Windows account picture clipped to a
     circle, read through the service; the seat's initial when there is none. -->
<script lang="ts">
  import type { UserPicture } from '#lib/remote/types.ts';
  import { avatarSrc, initial } from '#lib/remote/avatar.ts';

  interface Props {
    name: string;
    picture?: UserPicture | null;
    size?: number;
  }
  let { name, picture = null, size = 28 }: Props = $props();
  const src = $derived(avatarSrc(picture));
</script>

{#if src}
  <img class="avatar" {src} alt="" width={size} height={size} style:width="{size}px" style:height="{size}px" />
{:else}
  <span class="avatar initial" aria-hidden="true" style:width="{size}px" style:height="{size}px">{initial(name)}</span>
{/if}

<style>
  .avatar {
    border-radius: 50%;
    flex: none;
    object-fit: cover;
  }

  .initial {
    display: grid;
    place-items: center;
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
</style>
