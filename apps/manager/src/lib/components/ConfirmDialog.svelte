<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Kept for old imports (redesign P4): the confirm dialog is ui/ConfirmDialog now; this file
     only passes its props through. Deleted in P8 once every importer uses the kit directly. -->
<script lang="ts">
  import type { ComponentProps } from 'svelte';
  import UiConfirmDialog from '#lib/ui/ConfirmDialog.svelte';

  let props: ComponentProps<typeof UiConfirmDialog> = $props();
</script>

<UiConfirmDialog {...props} />
