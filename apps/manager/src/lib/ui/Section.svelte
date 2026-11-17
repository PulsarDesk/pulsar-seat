<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Section: an h2, one help line under it (never beside it), then the content (usually one
     Group). 32px between sections. `id` makes it a Toc target. -->
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    help?: string;
    id?: string;
    /** Section-end action (a compact Button), aligned with the heading. */
    end?: Snippet;
    children: Snippet;
  }
  let { title, help, id, end, children }: Props = $props();
  const uid = `s-${Math.random().toString(36).slice(2, 9)}`;
  const hid = $derived(`${id ?? uid}-h`);
</script>

<section class="section" {id} aria-labelledby={hid}>
  <div class="section-head">
    <div class="t">
      <h2 id={hid}>{title}</h2>
      {#if help}<p class="help">{help}</p>{/if}
    </div>
    {#if end}<div class="end">{@render end()}</div>{/if}
  </div>
  {@render children()}
</section>

<style>
  .section {
    scroll-margin-top: var(--sp-5);
  }
</style>
