<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Old "Network addresses" tab, kept only until the app shell (redesign P2) stops routing
     it: the content moved into the Advanced page (NetworkSection, P4). Deleted in P8. -->
<script lang="ts">
  import NetworkSection from './NetworkSection.svelte';
  import type { ManagerStore } from '#lib/store.svelte.ts';
  import type { SeatNumber } from '#lib/types.ts';

  interface Props {
    store: ManagerStore;
    initialSeat?: SeatNumber | null;
  }
  let { store, initialSeat = null }: Props = $props();
</script>

<div class="page">
  <NetworkSection {store} {initialSeat} readOnly={!store.isAdmin} />
</div>
