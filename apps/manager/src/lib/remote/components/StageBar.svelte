<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The save bar of a remote page (staged save, redesign P6). The page owns the draft and hands
     its stage here; the bar is the shared ui/SaveBar, docked at the bottom of the remote view
     (sticky at the end of the scroll area, after the page content, so it never covers the
     last row). It also reports the stage to `remoteStage`, which the app's unsaved-changes
     guard reads. -->
<script lang="ts">
  import { SaveBar } from '#lib/ui/index.ts';
  import { remoteStage, type RemoteStage } from '#lib/remote/stage.svelte.ts';

  let { stage }: { stage: RemoteStage } = $props();
  let busy = $state(false);

  $effect(() => {
    remoteStage.current = stage;
  });
  $effect(() => () => {
    remoteStage.current = null;
  });

  async function save() {
    if (busy || stage.invalid) return;
    busy = true;
    try {
      await stage.save();
    } finally {
      busy = false;
    }
  }
</script>

{#if stage.count > 0}
  <div class="dock">
    <SaveBar count={stage.count} detail={stage.detail} saveDisabledReason={stage.invalid} {busy} shortcut onSave={save} onUndo={() => stage.discard()} />
  </div>
{/if}

<style>
  .dock {
    position: sticky;
    bottom: 0;
    z-index: var(--z-sticky);
  }
</style>
