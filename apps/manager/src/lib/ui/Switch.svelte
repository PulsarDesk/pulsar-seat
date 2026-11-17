<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Switch: role=switch + aria-checked, 40x24, accent when on, the knob slides in 220ms.
     There is no disabled look: a setting that cannot change renders LockedValue instead.
     The accessible name includes the object ("Start automatically: Misafir"). -->
<script lang="ts">
  interface Props {
    checked?: boolean;
    /** Accessible name; or use labelledby. */
    label?: string;
    labelledby?: string;
    describedby?: string;
    id?: string;
    onchange?: (checked: boolean) => void;
  }
  let { checked = $bindable(false), label, labelledby, describedby, id, onchange }: Props = $props();

  function toggle() {
    checked = !checked;
    onchange?.(checked);
  }
</script>

<button
  type="button"
  role="switch"
  class="switch"
  {id}
  aria-checked={checked ? 'true' : 'false'}
  aria-label={label}
  aria-labelledby={labelledby}
  aria-describedby={describedby}
  onclick={toggle}
></button>

<style>
  .switch {
    position: relative;
    width: 40px;
    height: 24px;
    flex: none;
    margin: 0;
    padding: 0;
    border: 0;
    border-radius: var(--r-pill);
    background: var(--border-input);
    cursor: pointer;
    transition: background var(--dur) var(--ease);
  }
  .switch::after {
    content: '';
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: var(--r-pill);
    background: var(--knob);
    box-shadow: var(--shadow-xs);
    transition: translate var(--dur) var(--ease);
  }
  .switch[aria-checked='true'] {
    background: var(--accent);
  }
  .switch[aria-checked='true']::after {
    translate: 16px 0;
  }
  /* Windows contrast themes drop backgrounds and shadows: draw the track with a border and
     keep the knob's own system colours, so on / off stays visible (review 2026-11-17). */
  @media (forced-colors: active) {
    .switch {
      border: 1px solid ButtonText;
    }
    .switch::after {
      forced-color-adjust: none;
      top: 2px;
      left: 2px;
      background: ButtonText;
      box-shadow: none;
    }
    .switch[aria-checked='true'] {
      forced-color-adjust: none;
      background: Highlight;
      border-color: Highlight;
    }
    .switch[aria-checked='true']::after {
      background: HighlightText;
    }
  }
</style>
