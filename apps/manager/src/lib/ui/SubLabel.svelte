<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- A label inside a section ("Session method", "Other methods"): body size, 600, 24px above. -->
<script lang="ts">
  let { text, id }: { text: string; id?: string } = $props();
</script>

<h3 class="sub" {id}>{text}</h3>
