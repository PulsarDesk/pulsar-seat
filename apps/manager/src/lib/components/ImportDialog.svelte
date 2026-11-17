<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Optional importer (spec 10 "Pulsar Seat approach"; redesign 71-dlg-import): the service
     maps another product's device-assignment export to our config; the result only fills the
     draft. -->
<script lang="ts">
  import { Button, Field, Modal } from '#lib/ui/index.ts';
  import { t } from '#lib/i18n.ts';

  interface Props {
    onImport: (json: string) => Promise<boolean>;
    onClose: () => void;
  }
  let { onImport, onClose }: Props = $props();
  let text = $state('');
  let busy = $state(false);
  let parseError = $state(false);
  let file: HTMLInputElement | undefined = $state();

  async function pick(e: Event) {
    const f = (e.currentTarget as HTMLInputElement).files?.[0];
    if (f) text = await f.text();
  }

  async function run() {
    try {
      JSON.parse(text);
      parseError = false;
    } catch {
      parseError = true;
      return;
    }
    busy = true;
    const ok = await onImport(text);
    busy = false;
    if (ok) onClose();
  }
</script>

<Modal title={t.import.title} size="md" onClose={onClose}>
  <p class="help">{t.import.body}</p>
  <div>
    <input bind:this={file} class="sr-only" type="file" accept=".json,application/json" tabindex="-1" aria-label={t.import.file} onchange={pick} />
    <Button label={t.import.file} icon="file" onclick={() => file?.click()} />
  </div>
  <Field label={t.import.content} error={parseError ? t.errorJson : undefined}>
    {#snippet children({ id, describedby, invalid })}
      <textarea class="textarea mono" {id} rows="6" placeholder={'{ … }'} aria-describedby={describedby} aria-invalid={invalid} bind:value={text}></textarea>
    {/snippet}
  </Field>
  {#snippet foot()}
    <Button kind="primary" label={t.import.run} disabled={text.trim() === ''} {busy} onclick={run} />
    <Button label={t.cancel} onclick={onClose} />
  {/snippet}
</Modal>
