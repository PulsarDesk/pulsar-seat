<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The one looping animation: a loader icon. With `label` it is a status line
     (role=status, the text carries the meaning; reduced motion stops the spin). -->
<script lang="ts">
  import Icon from './Icon.svelte';

  let { label, size = 16 }: { label?: string; size?: 14 | 16 | 18 | 40 } = $props();
</script>

{#if label}
  <p class="wait" role="status"><Icon name="loader" {size} spin />{label}</p>
{:else}
  <Icon name="loader" {size} spin />
{/if}

<style>
  .wait {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
    margin: 0;
    color: var(--text-muted);
  }
</style>
