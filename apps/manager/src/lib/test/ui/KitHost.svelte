<!-- SPDX-License-Identifier: GPL-3.0-only -->
<!-- Test fixture: small components that need snippets or bindings. -->
<script lang="ts">
  import Button from '../../ui/Button.svelte';
  import ChoiceList from '../../ui/ChoiceList.svelte';
  import Field from '../../ui/Field.svelte';
  import LockedValue from '../../ui/LockedValue.svelte';
  import Row from '../../ui/Row.svelte';
  import Segmented from '../../ui/Segmented.svelte';
  import Switch from '../../ui/Switch.svelte';

  let on = $state(false);
  let mode = $state<'manual' | 'boot' | 'logon'>('manual');
  let theme = $state<'system' | 'light' | 'dark'>('system');
</script>

<Row label="Start automatically" help="Follows the start mode">
  {#snippet control()}<Switch bind:checked={on} label="Start automatically: Guest" />{/snippet}
</Row>
<Row label="Seat 1">
  {#snippet control()}<LockedValue text="Always starts" />{/snippet}
</Row>
<span data-testid="on">{on}</span>
<Button kind="primary" label="Pair a device" disabledReason="Start the seat first." />
<ChoiceList
  name="start"
  label="Start mode"
  bind:value={mode}
  options={[
    { value: 'manual', label: 'Manually', note: 'recommended for the first setup' },
    { value: 'boot', label: 'When Windows starts' },
    { value: 'logon', label: 'Concurrent full sessions', unavailable: 'Not available: Windows Server only' }
  ]}
/>
<span data-testid="mode">{mode}</span>
<Segmented
  label="Theme"
  bind:value={theme}
  options={[
    { value: 'system', label: 'System' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' }
  ]}
/>
<span data-testid="theme">{theme}</span>
<Field label="Name" help="Up to 64 characters." error="The name is empty.">
  {#snippet children({ id, describedby, invalid })}
    <input class="input" {id} aria-describedby={describedby} aria-invalid={invalid} />
  {/snippet}
</Field>
