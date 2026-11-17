<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Save-time conflicts (spec 10 §3.4; redesign 55-dlg-conflict): one row per conflict with a
     bare alert icon, then "Fix" (closes, back to editing) or "Save anyway" (force). -->
<script lang="ts">
  import { Button, Group, Icon, Modal, Row } from '#lib/ui/index.ts';
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

<Modal title={t.conflicts.title} size="md" alert initialFocus="primary" onClose={onCancel}>
  <p>{t.conflicts.body}</p>
  <Group>
    {#each issues as i, n (n)}
      <Row label={issueLabel(i, deviceName)}>
        {#snippet lead()}<Icon name="alert" tone="warn" />{/snippet}
      </Row>
    {/each}
  </Group>
  {#snippet foot()}
    <Button kind="primary" label={t.conflicts.fix} onclick={onCancel} />
    <Button label={t.conflicts.saveAnyway} onclick={onForce} />
  {/snippet}
</Modal>
