<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Modal on a native <dialog> opened with showModal(): focus trap, Escape, inert page. Focus
     returns to the opener when it closes (the caller unmounts it). Widths: sm 440, md 560,
     lg 760. Head: h2 + one muted line + close button (no icon disc). Foot: no divider; an
     optional muted reason on the left (`footStart`), then the buttons, right-aligned, in one
     order: the primary, other secondaries, Cancel / Close last (all Button kind="secondary"
     except the primary; `danger` for a destructive confirm). `foot` receives `reasonId`, the
     id of the footStart text, for a disabled primary's `reasonId`.
     initialFocus: 'first' = first field, else the primary; 'cancel' = the last foot button
     (destructive confirms); 'primary' = the first foot button. -->
<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import { t } from '#lib/i18n.ts';
  import IconButton from './IconButton.svelte';

  interface Props {
    title: string;
    /** One muted line under the title. */
    sub?: string;
    size?: 'sm' | 'md' | 'lg';
    /** role="alertdialog" (destructive or blocking questions). */
    alert?: boolean;
    initialFocus?: 'first' | 'cancel' | 'primary';
    /** Escape, the close button and a click on the backdrop call this. */
    onClose: () => void;
    closeLabel?: string;
    showClose?: boolean;
    children?: Snippet;
    body?: Snippet;
    foot?: Snippet<[{ reasonId: string }]>;
    footStart?: Snippet;
  }
  let {
    title,
    sub,
    size = 'md',
    alert = false,
    initialFocus = 'first',
    onClose,
    closeLabel,
    showClose = true,
    children,
    body,
    foot,
    footStart
  }: Props = $props();

  const uid = `dlg-${Math.random().toString(36).slice(2, 9)}`;
  const reasonId = `${uid}-why`;
  let dlg: HTMLDialogElement | undefined = $state();
  let footEl: HTMLDivElement | undefined = $state();
  let bodyEl: HTMLDivElement | undefined = $state();

  const FOCUSABLE =
    'button:not([disabled]), [href], input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  function focusables(): HTMLElement[] {
    return [...(dlg?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])].filter((el) => !el.closest('[hidden], [inert]'));
  }

  onMount(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const d = dlg;
    if (d) {
      if (typeof d.showModal === 'function' && !d.open) d.showModal();
      else d.setAttribute('open', '');
    }
    const footButtons = [...(footEl?.querySelectorAll<HTMLElement>('button') ?? [])];
    let target: HTMLElement | undefined;
    if (initialFocus === 'cancel') target = footButtons[footButtons.length - 1];
    else if (initialFocus === 'primary') target = footButtons[0];
    else target = bodyEl?.querySelector<HTMLElement>('input:not([disabled]), select:not([disabled]), textarea:not([disabled])') ?? footButtons[0];
    (target ?? focusables()[0])?.focus();
    return () => {
      if (d?.open && typeof d.close === 'function') d.close();
      if (opener?.isConnected) opener.focus();
    };
  });

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== 'Tab') return;
    const list = focusables();
    if (!list.length) return;
    const first = list[0];
    const last = list[list.length - 1];
    const at = document.activeElement;
    if (e.shiftKey && (at === first || !dlg?.contains(at))) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (at === last || !dlg?.contains(at))) {
      e.preventDefault();
      first.focus();
    }
  }

  function backdrop(e: MouseEvent) {
    // A click on the <dialog> itself (not its content) is a click on the backdrop.
    if (e.target === dlg && !alert) onClose();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events -->
<dialog
  bind:this={dlg}
  class="modal {size}"
  role={alert ? 'alertdialog' : undefined}
  aria-modal="true"
  aria-labelledby="{uid}-t"
  aria-describedby={sub ? `${uid}-d` : undefined}
  oncancel={(e) => {
    e.preventDefault();
    onClose();
  }}
  onkeydown={key}
  onclick={backdrop}
>
  <div class="frame">
    <div class="head">
      <div class="titles">
        <h2 id="{uid}-t">{title}</h2>
        {#if sub}<p id="{uid}-d">{sub}</p>{/if}
      </div>
      {#if showClose}<IconButton icon="x" label={closeLabel ?? t.close} tooltip={false} onclick={onClose} />{/if}
    </div>
    <div class="body" bind:this={bodyEl}>
      {#if body}{@render body()}{:else if children}{@render children()}{/if}
    </div>
    {#if foot || footStart}
      <div class="foot" bind:this={footEl}>
        {#if footStart}<span class="start" id={reasonId}>{@render footStart()}</span>{/if}
        {#if foot}{@render foot({ reasonId })}{/if}
      </div>
    {/if}
  </div>
</dialog>

<style>
  .modal {
    width: min(var(--dialog-md), calc(100vw - 48px));
    max-width: none;
    max-height: calc(100vh - 48px);
    margin: auto;
    padding: 0;
    border: var(--bw) solid var(--border);
    border-radius: var(--r-lg);
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }
  .modal.sm {
    width: min(var(--dialog-sm), calc(100vw - 48px));
  }
  .modal.lg {
    width: min(var(--dialog-lg), calc(100vw - 48px));
  }
  .modal::backdrop {
    background: var(--scrim);
  }
  .frame {
    display: flex;
    flex-direction: column;
    max-height: calc(100vh - 48px);
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-5) var(--sp-5) 0;
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
    min-width: 0;
  }
  .titles p {
    margin: 0;
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  .head > :global(.icon-btn) {
    margin: -4px -6px 0 0;
  }
  .body {
    padding: var(--sp-4) var(--sp-5) var(--sp-5);
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .body > :global(p) {
    max-width: var(--measure);
  }
  .body :global(.row) {
    padding: var(--sp-3) var(--sp-4);
    min-height: 48px;
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-5) var(--sp-5);
    flex-wrap: wrap;
  }
  .start {
    margin-right: auto;
    color: var(--text-muted);
    font-size: var(--fs-help);
    line-height: var(--lh-help);
  }
  @media (prefers-reduced-motion: no-preference) {
    .modal[open] {
      animation: dlg-in var(--dur-slow) var(--ease-out);
    }
  }
  @keyframes dlg-in {
    from {
      opacity: 0;
      translate: 0 8px;
    }
  }
</style>
