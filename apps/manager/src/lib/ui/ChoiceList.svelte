<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- ChoiceList: radio rows inside a Group (start mode, session method, account mode, IP rule).
     The radio is the only selection signal: no row tint, no "in use" chip. An unavailable
     option has no control: an 18px gap, a muted label, and its reason as the help line. -->
<script lang="ts" generics="V extends string">
  import type { ChoiceOption } from './types.ts';

  interface Props {
    name: string;
    options: ChoiceOption<V>[];
    value: V | undefined;
    /** Accessible name of the radiogroup (or use labelledby). */
    label?: string;
    labelledby?: string;
    disabled?: boolean;
    /** Inside a dialog: tighter rows. */
    dense?: boolean;
    onchange?: (value: V) => void;
  }
  let { name, options, value = $bindable(), label, labelledby, disabled = false, dense = false, onchange }: Props = $props();

  function pick(v: V) {
    value = v;
    onchange?.(v);
  }
</script>

<div class="opts" role="radiogroup" aria-label={label} aria-labelledby={labelledby}>
  <div class="group">
    {#each options as o (o.value)}
      {#if o.unavailable}
        <div class="choice unavailable" class:dense>
          <span class="gap" aria-hidden="true"></span>
          <span class="t">
            <b>{o.label}{#if o.note}<span class="note">{o.note}</span>{/if}</b>
            <span>{o.unavailable}{#if o.help}<br />{o.help}{/if}</span>
          </span>
        </div>
      {:else}
        <label class="choice" class:dense>
          <input type="radio" {name} value={o.value} checked={value === o.value} {disabled} onchange={() => pick(o.value)} />
          <span class="t">
            <b>{o.label}{#if o.note}<span class="note">{o.note}</span>{/if}</b>
            {#if o.help}<span>{o.help}</span>{/if}
          </span>
        </label>
      {/if}
    {/each}
  </div>
</div>

<style>
  .opts {
    display: flex;
    flex-direction: column;
  }
  .choice {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    padding: var(--sp-3) var(--sp-5);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease);
  }
  .choice.dense {
    padding: var(--sp-3) var(--sp-4);
  }
  .choice:hover {
    background: var(--surface-2);
  }
  input {
    appearance: none;
    margin: 2px 0 0;
    padding: 0;
    width: 18px;
    height: 18px;
    flex: none;
    border: var(--bw-input) solid var(--border-input);
    border-radius: var(--r-pill);
    background: var(--surface);
    cursor: pointer;
  }
  input:checked {
    border-color: var(--accent);
    background: var(--accent);
    box-shadow: inset 0 0 0 3.5px var(--surface);
  }
  input:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  /* Contrast themes drop the background and inset shadow that mark a checked radio. */
  @media (forced-colors: active) {
    input {
      forced-color-adjust: none;
      border-color: ButtonText;
      background: Canvas;
    }
    input:checked {
      border-color: Highlight;
      background: Highlight;
      box-shadow: inset 0 0 0 3.5px Canvas;
    }
  }
  input:disabled {
    cursor: not-allowed;
  }
  .t {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .t b {
    font-weight: 600;
    display: flex;
    gap: 8px;
    align-items: baseline;
    flex-wrap: wrap;
  }
  .t > span {
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
    max-width: var(--measure);
  }
  .unavailable {
    cursor: default;
  }
  .unavailable:hover {
    background: transparent;
  }
  .gap {
    width: 18px;
    flex: none;
  }
  .unavailable b {
    color: var(--text-muted);
    font-weight: 500;
  }
</style>
