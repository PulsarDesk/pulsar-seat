<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The Pulsar mark: concentric pulse rings in the `mark` colour (rail brand, wizard). -->
<script lang="ts">
  let { size = 26 }: { size?: number } = $props();
</script>

<svg class="mark" width={size} height={size} viewBox="0 0 52 52" fill="none" aria-hidden="true" focusable="false">
  <circle cx="26" cy="26" r="24" stroke="currentColor" stroke-width="1.5" opacity=".2" />
  <circle cx="26" cy="26" r="16" stroke="currentColor" stroke-width="1.75" opacity=".45" />
  <circle cx="26" cy="26" r="8.5" stroke="currentColor" stroke-width="2" />
  <circle cx="26" cy="26" r="3.5" fill="currentColor" />
</svg>

<style>
  .mark {
    flex: none;
    color: var(--mark);
  }
</style>
