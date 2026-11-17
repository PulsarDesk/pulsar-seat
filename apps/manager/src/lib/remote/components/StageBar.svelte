<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The save bar of a remote page (staged save, redesign P6). The page owns the draft and hands
     its stage here; this reports it to `remoteStage`, which the app's unsaved-changes guard
     and docked save bar read. Inside the app shell (`provideStageDock`) the shell docks the
     shared ui/SaveBar under the scroll area; without a shell (page tests) the bar renders
     here, sticky at the end of the page. -->
<script lang="ts">
  import { SaveBar } from '#lib/ui/index.ts';
  import { hasStageDock, remoteStage, type RemoteStage } from '#lib/remote/stage.svelte.ts';

  let { stage }: { stage: RemoteStage } = $props();
  let busy = $state(false);
  const docked = hasStageDock();

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

{#if !docked && stage.count > 0}
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
