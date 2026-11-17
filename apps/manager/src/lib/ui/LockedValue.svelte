<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- A value the user cannot change here: plain muted text, never a disabled control
     ("Always starts", "Not available yet", "Not assigned", "Saved"). -->
<script lang="ts">
  let { text, mono = false, id }: { text: string; mono?: boolean; id?: string } = $props();
</script>

<span class="value" class:mono {id}>{text}</span>

<style>
  .value {
    color: var(--text-muted);
  }
  .mono {
    color: var(--text);
    font-size: var(--fs-help);
  }
</style>
