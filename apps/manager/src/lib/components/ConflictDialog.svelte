<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Save-time conflicts (spec 10 §3.4): list them, then save anyway (force) or cancel. -->
<script lang="ts">
  import Modal from './Modal.svelte';
  import { issueLabel, t } from '#lib/i18n.ts';
  import type { Issue } from '#lib/types.ts';

  interface Props {
    issues: Issue[];
    deviceName: (key: string) => string;
    onForce: () => void;
    onCancel: () => void;
  }
  let { issues, deviceName, onForce, onCancel }: Props = $props();
</script>

<Modal title={t.conflicts.title} alert onClose={onCancel}>
  <p>{t.conflicts.body}</p>
  <ul>
    {#each issues as i, n (n)}<li>{issueLabel(i, deviceName)}</li>{/each}
  </ul>
  {#snippet buttons()}
    <button type="button" class="primary danger-btn" onclick={onForce}>{t.conflicts.saveAnyway}</button>
    <button type="button" onclick={onCancel}>{t.cancel}</button>
  {/snippet}
</Modal>
