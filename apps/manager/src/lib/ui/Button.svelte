<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Button (DESIGN.md "Buttons"). Kinds: primary (one per page head + Save), secondary (every
     other real action, also Cancel / Close), quiet (low-stakes tertiary), danger (confirm inside
     a destructive dialog only), danger-quiet (destructive entry points on pages).
     Disabled = aria-disabled (stays focusable) + a visible reason linked by aria-describedby:
     `disabledReason` renders the reason after the button, or point `reasonId` at text the
     caller shows (a dialog footer's left text). Labels never end in "…" (dev builds warn). -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { HTMLButtonAttributes } from 'svelte/elements';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons.ts';
  import type { ButtonKind } from './types.ts';

  interface Props extends Omit<HTMLButtonAttributes, 'type' | 'disabled' | 'children'> {
    kind?: ButtonKind;
    /** 32px instead of 36px: inside rows, status lines, notices. */
    compact?: boolean;
    icon?: IconName;
    /** Text label; or pass children. */
    label?: string;
    type?: 'button' | 'submit';
    /** Not available now: aria-disabled, clicks ignored. */
    disabled?: boolean;
    /** Why it is disabled; shown next to the button and linked by aria-describedby. */
    disabledReason?: string;
    /** Id of visible reason text the caller renders (dialog footer start); used instead of disabledReason's own text. */
    reasonId?: string;
    /** Busy: aria-disabled + spinner icon. */
    busy?: boolean;
    el?: HTMLButtonElement;
    children?: Snippet;
  }
  let {
    kind = 'secondary',
    compact = false,
    icon,
    label,
    type = 'button',
    disabled = false,
    disabledReason,
    reasonId,
    busy = false,
    el = $bindable(),
    children,
    onclick,
    class: cls = '',
    ...rest
  }: Props = $props();

  const uid = `btn-why-${Math.random().toString(36).slice(2, 9)}`;
  const off = $derived(disabled || busy || !!disabledReason);
  const describedBy = $derived(
    [rest['aria-describedby'], off ? (reasonId ?? (disabledReason ? uid : undefined)) : undefined].filter(Boolean).join(' ') || undefined
  );

  function click(e: MouseEvent & { currentTarget: EventTarget & HTMLButtonElement }) {
    if (off) {
      e.preventDefault();
      return;
    }
    onclick?.(e);
  }

  $effect(() => {
    if (!import.meta.env.DEV || !el) return;
    const text = el.textContent?.trim() ?? '';
    if (text.endsWith('…') || text.endsWith('...')) {
      console.warn(`[ui/Button] "${text}": button labels never end in an ellipsis (DESIGN.md, the Ellipsis Rule).`);
    }
  });
</script>

<button
  bind:this={el}
  {...rest}
  {type}
  class="btn {kind} {cls}"
  class:compact
  aria-disabled={off ? 'true' : undefined}
  aria-busy={busy ? 'true' : undefined}
  aria-describedby={describedBy}
  onclick={click}
>
  {#if busy}<Icon name="loader" size={16} spin />{:else if icon}<Icon name={icon} size={16} />{/if}
  {#if children}{@render children()}{:else}{label}{/if}
</button>
{#if off && disabledReason && !reasonId}<span class="reason" id={uid}>{disabledReason}</span>{/if}

<style>
  .btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    white-space: nowrap;
    flex: none;
    min-height: var(--h-control);
    margin: 0;
    padding: 6px 14px;
    max-width: 100%;
    border: var(--bw) solid transparent;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text);
    font-family: var(--font-sans);
    font-weight: 600;
    font-size: var(--fs-body);
    line-height: 1.25;
    text-align: center;
    text-decoration: none;
    cursor: pointer;
    transition:
      background var(--dur-fast) var(--ease),
      border-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .btn:active {
    translate: 0 1px;
  }
  .primary {
    background: var(--accent);
    color: var(--text-on-accent);
  }
  .primary:hover {
    background: var(--accent-hover);
  }
  .primary:active {
    background: var(--accent-press);
  }
  .secondary {
    background: var(--surface);
    border-color: var(--border-strong);
  }
  .secondary:hover {
    background: var(--surface-2);
    border-color: var(--border-input);
  }
  .quiet {
    color: var(--text-muted);
  }
  .quiet:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .danger {
    background: var(--danger-fill);
    color: var(--text-on-danger);
  }
  .danger:hover {
    background: var(--danger-fill-hover);
  }
  .danger-quiet {
    color: var(--danger-ink);
  }
  .danger-quiet:hover {
    background: var(--danger-soft);
  }
  .compact {
    min-height: var(--h-compact);
    padding: 4px 12px;
  }
  .btn[aria-disabled='true'],
  .btn[aria-disabled='true']:hover {
    background: var(--surface-3);
    color: var(--text-faint);
    border-color: transparent;
    cursor: not-allowed;
    translate: none;
  }
  .quiet[aria-disabled='true'],
  .quiet[aria-disabled='true']:hover {
    background: transparent;
  }
  .reason {
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
</style>
