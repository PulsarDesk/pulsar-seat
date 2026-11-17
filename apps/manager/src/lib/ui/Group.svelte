<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Group: the one container level on settings pages (surface, hairline, md radius, no shadow;
     children divided by hairlines). Groups never nest. `plain`: dividers only (wizard). -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { HTMLAttributes } from 'svelte/elements';

  interface Props extends HTMLAttributes<HTMLDivElement> {
    plain?: boolean;
    children: Snippet;
  }
  let { plain = false, children, class: cls = '', ...rest }: Props = $props();
</script>

<div {...rest} class="group {cls}" class:plain>{@render children()}</div>
