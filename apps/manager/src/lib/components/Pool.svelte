<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- The unassigned devices ("system" container, spec 10 §3): an h2 and one help line above an
     auto-fill grid of plain device rows. No panel and no tiles; a dashed accent outline only
     while a drag is over it. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '#lib/i18n.ts';
  import { DropZone, type OnDrop } from './drag.svelte.ts';

  interface Props {
    onDrop: OnDrop;
    children: Snippet;
  }
  let { onDrop, children }: Props = $props();

  const zone = new DropZone(
    () => 'system',
    () => onDrop
  );
</script>

<section
  class="pool"
  class:drop={zone.active}
  aria-label={t.canvas.system}
  ondragenter={zone.enter}
  ondragover={zone.over}
  ondragleave={zone.leave}
  ondrop={zone.drop}
>
  <div class="section-head">
    <div class="t">
      <h2>{t.canvas.system}</h2>
      <p class="help">{t.canvas.systemHint}</p>
    </div>
  </div>
  <ul class="list">
    {@render children()}
  </ul>
</section>

<style>
  .pool {
    margin-bottom: var(--sp-6);
    border-radius: var(--r);
  }
  .pool.drop {
    outline: 2px dashed var(--accent);
    outline-offset: 4px;
  }
  .list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 2px var(--sp-3);
    list-style: none;
    min-height: var(--h-device);
  }
</style>
