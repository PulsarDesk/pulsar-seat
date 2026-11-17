<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Avatar: an account picture clipped to a circle, or a generic initial when there is none.
     Decorative (aria-hidden): the row or heading next to it carries the name. -->
<script lang="ts">
  interface Props {
    /** `data:` URL of the picture; null = the initial. */
    src: string | null;
    /** One letter for the fallback. */
    initial: string;
    size?: number;
  }
  let { src, initial, size = 32 }: Props = $props();
</script>

{#if src}
  <img class="avatar" {src} alt="" aria-hidden="true" width={size} height={size} style:width="{size}px" style:height="{size}px" />
{:else}
  <span class="avatar initial" aria-hidden="true" style:width="{size}px" style:height="{size}px">{initial}</span>
{/if}

<style>
  .avatar {
    flex: none;
    border-radius: 50%;
    object-fit: cover;
  }
  .initial {
    display: inline-grid;
    place-items: center;
    background: var(--surface-3);
    color: var(--text);
    border: 1px solid var(--border);
    font-weight: 600;
    font-size: var(--fs-help);
  }
</style>
