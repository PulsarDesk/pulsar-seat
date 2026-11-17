<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Gamepads that SDL games of one seat ignore (`SeatConfig.pad_ignore_devices`, the
     SDL_GAMECONTROLLER_IGNORE_DEVICES list): 0xVVVV/0xPPPP ids, one per line or separated by
     commas. Was a column of the old seat table; now a seat-menu dialog (redesign P4). -->
<script lang="ts">
  import { Button, Field, Modal } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';
  import { isVidPid, parsePadIgnoreList } from '#lib/model.ts';

  interface Props {
    seatName: string;
    ids: string[];
    onSave: (ids: string[]) => void;
    onCancel: () => void;
  }
  let { seatName, ids, onSave, onCancel }: Props = $props();
  // svelte-ignore state_referenced_locally
  let text = $state(ids.join('\n'));
  const all = $derived(parsePadIgnoreList(text));
  const parsed = $derived(all.filter(isVidPid));
  const bad = $derived(all.filter((x) => !isVidPid(x)));
</script>

<Modal title={t.settingsPage.padIgnoreTitle(seatName)} size="sm" onClose={onCancel}>
  <Field label={t.settingsPage.padIgnoreLabel} help={t.basic.padIgnoreHint} error={bad.length ? t.settingsPage.padIgnoreBad(bad.join(', ')) : undefined}>
    {#snippet children({ id, describedby, invalid })}
      <textarea
        class="textarea mono"
        {id}
        rows="4"
        spellcheck="false"
        placeholder={t.basic.padIgnorePlaceholder}
        aria-describedby={describedby}
        aria-invalid={invalid}
        data-testid="pad-ignore"
        bind:value={text}
      ></textarea>
    {/snippet}
  </Field>
  {#snippet foot()}
    <Button kind="primary" label={t.save} onclick={() => onSave(parsed)} />
    <Button label={t.cancel} onclick={onCancel} />
  {/snippet}
</Modal>
