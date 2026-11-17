<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Menu button + menu (WAI-ARIA APG menu button). Text only: no icons; group labels and
     separators carry the structure; a danger item uses danger-ink; a muted hint (seat number)
     or a submenu chevron sits on the right. The menu renders in the top layer (Popover API,
     falling back to position: fixed), so no scroll container clips it.
     Keyboard: Enter / Space / ArrowDown open on the first item, ArrowUp on the last; arrows
     move, Home / End jump, typing jumps by first letters, Enter / Space activate, ArrowRight
     opens a submenu and ArrowLeft closes it, Escape closes and focus returns to the opener.
     Highlight is a fill; the focus ring shows only with :focus-visible.
     Without its own trigger: pass `anchor` (an element the caller owns) and bind `open`. -->
<script lang="ts">
  import { tick } from 'svelte';
  import { activeLang } from '#lib/i18n.ts';
  import Button from './Button.svelte';
  import IconButton from './IconButton.svelte';
  import Self from './Menu.svelte';
  import type { IconName } from './icons.ts';
  import type { ButtonKind, MenuEntry } from './types.ts';

  interface Props {
    items: MenuEntry[];
    /** Accessible name of the menu ("Misafir: seat actions"). */
    label: string;
    /** Accessible name of the trigger; defaults to `label`. */
    triggerLabel?: string;
    triggerIcon?: IconName;
    /** A text trigger (Button) instead of an icon button. */
    triggerText?: string;
    triggerKind?: ButtonKind;
    triggerCompact?: boolean;
    open?: boolean;
    /** External trigger: no own button is rendered; the menu is placed at this element. */
    anchor?: HTMLElement | null;
    /** Horizontal alignment to the trigger (below placement). */
    align?: 'start' | 'end';
    /** Internal: submenus open to the right of their item. */
    placement?: 'below' | 'right';
    /** Called after the menu closed (any reason). */
    onClose?: () => void;
    /** Internal: a submenu closes the whole tree after an activation. */
    closeTree?: () => void;
  }
  let {
    items,
    label,
    triggerLabel,
    triggerIcon = 'more',
    triggerText,
    triggerKind = 'secondary',
    triggerCompact = true,
    open = $bindable(false),
    anchor = null,
    align = 'end',
    placement = 'below',
    onClose,
    closeTree
  }: Props = $props();

  let trigger: HTMLButtonElement | undefined = $state();
  let menuEl: HTMLDivElement | undefined = $state();
  let active = $state(-1);
  let subOpen = $state(-1);
  let pos = $state({ top: 0, left: 0 });
  let startAt: 'first' | 'last' = 'first';
  let typed = '';
  let typedAt = 0;
  const itemEls: HTMLButtonElement[] = $state([]);

  const canPopover = typeof HTMLElement !== 'undefined' && 'showPopover' in HTMLElement.prototype;
  const opener = $derived(anchor ?? trigger ?? null);
  const isItem = (e: MenuEntry): e is Extract<MenuEntry, { label: string }> => 'label' in e;
  const indices = $derived(items.map((e, i) => (isItem(e) ? i : -1)).filter((i) => i >= 0));

  function place() {
    const a = opener?.getBoundingClientRect();
    if (!a || !menuEl) return;
    const w = menuEl.offsetWidth;
    const h = menuEl.offsetHeight;
    const vw = globalThis.innerWidth || 1024;
    const vh = globalThis.innerHeight || 768;
    let top: number;
    let left: number;
    if (placement === 'right') {
      left = a.right + 2;
      if (left + w > vw - 8) left = Math.max(8, a.left - w - 2);
      top = a.top - 6;
    } else {
      left = align === 'end' ? a.right - w : a.left;
      top = a.bottom + 4;
      if (top + h > vh - 8) {
        if (a.top - h - 4 >= 8) top = a.top - h - 4;
        else {
          // Fits neither below nor above: open beside the trigger so it stays visible.
          const beside = align === 'end' ? a.left - w - 4 : a.right + 4;
          if (beside >= 8 && beside + w <= vw - 8) left = beside;
          top = a.top - 6;
        }
      }
    }
    pos = { top: Math.max(8, Math.min(top, vh - h - 8)), left: Math.max(8, Math.min(left, vw - w - 8)) };
  }

  function focusItem(i: number) {
    active = i;
    itemEls[i]?.focus();
  }

  $effect(() => {
    if (!open || !menuEl) return;
    const el = menuEl;
    if (canPopover && el.hasAttribute('popover')) {
      try {
        el.showPopover();
      } catch {
        // already shown
      }
    }
    place();
    // Start on an enabled item; a disabled one stays reachable with the arrow keys.
    const enabled = indices.filter((i) => !(items[i] as { disabled?: boolean }).disabled);
    const pool = enabled.length ? enabled : indices;
    const first = startAt === 'last' ? pool[pool.length - 1] : pool[0];
    if (first !== undefined) focusItem(first);
    const away = (e: PointerEvent) => {
      const target = e.target as Node | null;
      if (!target) return;
      if (el.contains(target) || opener?.contains(target)) return;
      // A submenu lives inside this menu's element, so this only fires for real outside clicks.
      close(false);
    };
    const reflow = () => place();
    const timer = setTimeout(() => document.addEventListener('pointerdown', away, true), 0);
    globalThis.addEventListener('resize', reflow);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('pointerdown', away, true);
      globalThis.removeEventListener('resize', reflow);
    };
  });

  async function show(at: 'first' | 'last' = 'first') {
    startAt = at;
    subOpen = -1;
    open = true;
    await tick();
  }

  function close(refocus: boolean) {
    if (!open) return;
    if (canPopover && menuEl?.matches?.(':popover-open')) {
      try {
        menuEl.hidePopover();
      } catch {
        // already hidden
      }
    }
    open = false;
    subOpen = -1;
    active = -1;
    if (refocus) opener?.focus();
    onClose?.();
  }

  function closeAll() {
    if (closeTree) closeTree();
    else close(true);
  }

  function activate(i: number) {
    const e = items[i];
    if (!isItem(e) || e.disabled) return;
    if (e.submenu) {
      subOpen = i;
      return;
    }
    closeAll();
    e.action?.();
  }

  function move(delta: number) {
    if (!indices.length) return;
    const at = indices.indexOf(active);
    const next = at < 0 ? (delta > 0 ? 0 : indices.length - 1) : (at + delta + indices.length) % indices.length;
    focusItem(indices[next]);
  }

  function typeahead(ch: string) {
    const now = Date.now();
    typed = now - typedAt > 600 ? ch : typed + ch;
    typedAt = now;
    const lang = activeLang();
    const want = typed.toLocaleLowerCase(lang);
    const at = Math.max(0, indices.indexOf(active));
    const order = [...indices.slice(at + (typed.length === 1 ? 1 : 0)), ...indices.slice(0, at + (typed.length === 1 ? 1 : 0))];
    const hit = order.find((i) => {
      const e = items[i];
      return isItem(e) && e.label.toLocaleLowerCase(lang).startsWith(want);
    });
    if (hit !== undefined) focusItem(hit);
  }

  function onMenuKey(e: KeyboardEvent) {
    if (subOpen >= 0) return; // the submenu handles its own keys
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        move(1);
        break;
      case 'ArrowUp':
        e.preventDefault();
        move(-1);
        break;
      case 'Home':
        e.preventDefault();
        if (indices.length) focusItem(indices[0]);
        break;
      case 'End':
        e.preventDefault();
        if (indices.length) focusItem(indices[indices.length - 1]);
        break;
      case 'ArrowRight': {
        const it = items[active];
        if (it && isItem(it) && it.submenu) {
          e.preventDefault();
          subOpen = active;
        }
        break;
      }
      case 'ArrowLeft':
        if (placement === 'right') {
          e.preventDefault();
          e.stopPropagation();
          close(true);
        }
        break;
      case 'Escape':
        e.preventDefault();
        e.stopPropagation();
        close(true);
        break;
      case 'Tab':
        e.preventDefault();
        closeAll();
        break;
      case 'Enter':
      case ' ':
        e.preventDefault();
        if (active >= 0) activate(active);
        break;
      default:
        if (e.key.length === 1 && !e.ctrlKey && !e.altKey && !e.metaKey) typeahead(e.key);
    }
  }

  function onTriggerKey(e: KeyboardEvent) {
    // Enter and Space click the button (onTriggerClick); arrows open on the first / last item.
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      void show('first');
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      void show('last');
    }
  }

  function onTriggerClick() {
    if (open) close(false);
    else void show('first');
  }
</script>

{#if !anchor && placement === 'below'}
  {#if triggerText}
    <Button
      bind:el={trigger}
      kind={triggerKind}
      compact={triggerCompact}
      label={triggerText}
      aria-label={triggerLabel}
      aria-haspopup="menu"
      aria-expanded={open ? 'true' : 'false'}
      onclick={onTriggerClick}
      onkeydown={onTriggerKey}
    />
  {:else}
    <IconButton
      bind:el={trigger}
      icon={triggerIcon}
      label={triggerLabel ?? label}
      aria-haspopup="menu"
      aria-expanded={open ? 'true' : 'false'}
      onclick={onTriggerClick}
      onkeydown={onTriggerKey}
    />
  {/if}
{/if}

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="menu"
    role="menu"
    aria-label={label}
    tabindex="-1"
    popover={canPopover ? 'manual' : undefined}
    bind:this={menuEl}
    style:top="{pos.top}px"
    style:left="{pos.left}px"
    onkeydown={onMenuKey}
  >
    {#each items as e, i (i)}
      {#if 'separator' in e}
        <div role="separator" class="sep"></div>
      {:else if 'group' in e}
        <div class="mlabel" role="presentation">{e.group}</div>
      {:else}
        <button
          type="button"
          role="menuitem"
          tabindex="-1"
          bind:this={itemEls[i]}
          class:danger={e.danger}
          class:focus={active === i}
          aria-disabled={e.disabled ? 'true' : undefined}
          aria-haspopup={e.submenu ? 'menu' : undefined}
          aria-expanded={e.submenu ? (subOpen === i ? 'true' : 'false') : undefined}
          onclick={() => activate(i)}
          onfocus={() => (active = i)}
          onpointerenter={() => {
            active = i;
            if (subOpen >= 0 && subOpen !== i) subOpen = -1;
          }}
          ><span class="text">{e.label}</span>{#if e.hint}<span class="hint">{e.hint}</span>{/if}{#if e.submenu}<span class="sub-chev" aria-hidden="true"></span>{/if}</button
        >
        {#if e.submenu && subOpen === i}
          <Self
            items={e.submenu}
            label={e.label}
            anchor={itemEls[i]}
            placement="right"
            open={true}
            closeTree={closeAll}
            onClose={() => {
              subOpen = -1;
            }}
          />
        {/if}
      {/if}
    {/each}
  </div>
{/if}

<style>
  .menu {
    position: fixed;
    inset: auto;
    z-index: var(--z-menu);
    margin: 0;
    min-width: 232px;
    max-width: 340px;
    padding: 6px;
    overflow: visible;
    border-radius: var(--r);
    background: var(--surface);
    color: var(--text);
    border: var(--bw) solid var(--border);
    box-shadow: var(--shadow-lg);
  }
  .mlabel {
    padding: 8px 10px 4px;
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    font-weight: 600;
    color: var(--text-muted);
  }
  [role='menuitem'] {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    min-height: 36px;
    margin: 0;
    padding: 0 10px;
    border: 0;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--text);
    text-align: left;
    cursor: pointer;
    font-weight: 500;
    padding-block: 6px;
  }
  [role='menuitem']:hover,
  [role='menuitem'].focus {
    background: var(--surface-3);
  }
  [role='menuitem']:focus-visible {
    background: var(--surface-3);
    outline: 2px solid var(--focus);
    outline-offset: -2px;
  }
  [role='menuitem'].danger {
    color: var(--danger-ink);
  }
  [role='menuitem'][aria-disabled='true'] {
    color: var(--text-faint);
    cursor: not-allowed;
  }
  .text {
    flex: 1 1 auto;
    min-width: 0;
  }
  .hint {
    margin-left: auto;
    display: inline-flex;
    flex: none;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: var(--fs-meta);
    line-height: var(--lh-meta);
    font-weight: 400;
  }
  /* Submenu chevron: drawn with borders, so a menu item never holds an icon. */
  .sub-chev {
    margin-left: auto;
    width: 7px;
    height: 7px;
    border-right: 1.5px solid var(--text-muted);
    border-bottom: 1.5px solid var(--text-muted);
    rotate: -45deg;
    flex: none;
  }
  .sep {
    height: 1px;
    margin: 6px 4px;
    background: var(--border);
  }
  @media (prefers-reduced-motion: no-preference) {
    .menu {
      animation: menu-in var(--dur) var(--ease-out);
    }
  }
  @keyframes menu-in {
    from {
      opacity: 0;
      translate: 0 -4px;
    }
  }
</style>
