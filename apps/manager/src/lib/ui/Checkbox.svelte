<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Checkbox: a native 18px input with a 1.5px edge; checked = indigo fill + white check.
     With `label` it is a labelled row (label 500 + optional help); `tight` drops the padding
     (table cells, then pass `ariaLabel` instead of a visible label). -->
<script lang="ts">
  interface Props {
    checked?: boolean;
    label?: string;
    help?: string;
    ariaLabel?: string;
    tight?: boolean;
    disabled?: boolean;
    id?: string;
    onchange?: (checked: boolean) => void;
  }
  let { checked = $bindable(false), label, help, ariaLabel, tight = false, disabled = false, id, onchange }: Props = $props();
</script>

<label class="check" class:tight>
  <input
    type="checkbox"
    {id}
    {disabled}
    aria-label={ariaLabel}
    bind:checked
    onchange={(e) => onchange?.(e.currentTarget.checked)}
  />
  {#if label}
    <span class="t"><b>{label}</b>{#if help}<span>{help}</span>{/if}</span>
  {/if}
</label>

<style>
  .check {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    padding: 6px 0;
    cursor: pointer;
  }
  .check.tight {
    padding: 0;
    display: inline-flex;
  }
  input {
    appearance: none;
    margin: 1px 0 0;
    padding: 0;
    width: 18px;
    height: 18px;
    flex: none;
    border: var(--bw-input) solid var(--border-input);
    border-radius: var(--r-xs);
    background: var(--surface);
    display: grid;
    place-items: center;
    cursor: pointer;
  }
  input:checked {
    background: var(--accent);
    border-color: var(--accent);
  }
  input:checked::after {
    content: '';
    width: 10px;
    height: 6px;
    border: calc(2 * var(--bw)) solid var(--text-on-accent);
    border-top: 0;
    border-right: 0;
    rotate: -45deg;
    translate: 0 -1px;
  }
  input:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  input:disabled {
    cursor: not-allowed;
    border-color: var(--border);
  }
  .t {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .t b {
    font-weight: 500;
  }
  .t > span {
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
    max-width: var(--measure);
  }
</style>
