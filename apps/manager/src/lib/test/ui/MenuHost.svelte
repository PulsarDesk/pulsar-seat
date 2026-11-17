<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Test fixture: a Menu with groups, a hint, a submenu and a danger item. -->
<script lang="ts">
  import Menu from '../../ui/Menu.svelte';
  import type { MenuEntry } from '../../ui/types.ts';

  let { onPick = (_: string) => {} }: { onPick?: (label: string) => void } = $props();
  const items: MenuEntry[] = [
    { label: 'Start this seat', action: () => onPick('start') },
    { label: 'Sign in again', action: () => onPick('relogon') },
    { separator: true },
    { group: 'Settings' },
    { label: 'Rename…', hint: 'Seat 2', action: () => onPick('rename') },
    { label: 'Windows settings', submenu: [{ label: 'Device Manager', action: () => onPick('devmgmt') }] },
    { separator: true },
    { label: 'Delete seat…', danger: true, action: () => onPick('delete') }
  ];
</script>

<button type="button">Before</button>
<Menu {items} label="Guest: seat actions" />
