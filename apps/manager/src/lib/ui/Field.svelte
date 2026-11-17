<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Field: a visible label above the control, help below, an inline error with an icon.
     Placeholders never stand in for labels. The control snippet receives the ids to use:
       <Field label="Name">{#snippet children({ id, describedby, invalid })}
         <input class="input" {id} aria-describedby={describedby} aria-invalid={invalid} />
       {/snippet}</Field>
     Use `group` (no for=) when the control is a radiogroup or segmented control. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  interface Props {
    label: string;
    help?: string;
    error?: string;
    id?: string;
    /** The control is a group (radios, segmented): the label is plain text, not <label for>. */
    group?: boolean;
    children: Snippet<[{ id: string; labelId: string; describedby: string | undefined; invalid: 'true' | undefined }]>;
  }
  let { label, help, error, id, group = false, children }: Props = $props();
  const uid = Math.random().toString(36).slice(2, 9);
  const cid = $derived(id ?? `f-${uid}`);
  const labelId = `fl-${uid}`;
  const helpId = `fh-${uid}`;
  const errId = `fe-${uid}`;
  const describedby = $derived([help ? helpId : '', error ? errId : ''].filter(Boolean).join(' ') || undefined);
</script>

<div class="ui-field">
  {#if group}<span class="flabel" id={labelId}>{label}</span>{:else}<label class="flabel" id={labelId} for={cid}>{label}</label>{/if}
  {@render children({ id: cid, labelId, describedby, invalid: error ? 'true' : undefined })}
  {#if help}<p class="help" id={helpId}>{help}</p>{/if}
  {#if error}<p class="err" id={errId}><Icon name="alert-circle" size={14} />{error}</p>{/if}
</div>

<style>
  .ui-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    margin: 0;
  }
  .flabel {
    font-weight: 600;
  }
  .help {
    margin-top: -2px;
  }
  .err {
    display: flex;
    gap: 6px;
    align-items: center;
    margin: 0;
    color: var(--danger-ink);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
</style>
