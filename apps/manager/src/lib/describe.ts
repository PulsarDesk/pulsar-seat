// SPDX-License-Identifier: GPL-3.0-only
// Human-readable descriptions of devices and masks for tooltips and the device
// info dialog (spec 10 §3.2, §3.5 "connection path", "sharing state").

import { kindLabel, t } from './i18n.ts';
import * as M from './mask.ts';
import type { Device, DeviceKey, DeviceView, MaskSource, SeatMask } from './types.ts';

/** "Spare", "All seats", "Seat 1", "Seats 1, 3" (copy in `t.describe`). */
export function maskLabel(mask: SeatMask): string {
  if (M.isSpare(mask)) return t.canvas.spareSign;
  if (M.isShared(mask)) return t.assign.allSeats;
  const seats = M.seatsOf(mask);
  return t.describe.seatsOf(seats);
}

export function sourceLabel(source: MaskSource, name: (k: DeviceKey) => string): string {
  if (source.type === 'explicit') return t.describe.explicit;
  if (source.type === 'inherited') {
    // The service also names a port (`port;loc=…`) or a whole device (`cont;id=…`).
    if (source.hub.startsWith('port;')) return t.canvas.inheritedPort(source.hub.slice('port;loc='.length));
    if (source.hub.startsWith('cont;')) return t.canvas.inheritedContainer;
    return t.canvas.inherited(name(source.hub));
  }
  return t.canvas.defaultMask;
}

export function isolationLabel(code: string | undefined): string | null {
  return code ? (t.describe.isolation[code] ?? code) : null;
}

/** Chain of hubs from the device up to the root, nearest first. */
export function hubChain(d: Device, byKey: Map<DeviceKey, Device>): Device[] {
  const out: Device[] = [];
  let k = d.parent_hub;
  for (let i = 0; k && i < 32; i++) {
    const hub = byKey.get(k);
    if (!hub) break;
    out.push(hub);
    k = hub.parent_hub;
  }
  return out;
}

/** Lines for the tooltip and the info dialog. */
export function deviceLines(view: DeviceView, mask: SeatMask, all: DeviceView[], showIds = false): string[] {
  const d = view.device;
  const lines = [`${kindLabel(d.kind)}: ${d.name}`, t.describe.seat(maskLabel(mask))];
  if (d.label) lines.push(t.describe.location(d.label));
  const det = d.details;
  if (det.type === 'display') {
    lines.push(t.describe.adapter(det.adapter_name));
    lines.push(t.describe.connection(t.describe.output[det.output_technology] ?? det.output_technology));
    if (det.mode) lines.push(t.describe.resolution(det.mode.width, det.mode.height, (det.mode.refresh_mhz / 1000).toFixed(0)));
  } else if (det.type === 'usb') {
    if (d.kind === 'usb_hub') {
      lines.push(t.describe.ports(det.port_count));
      const kids = all.filter((v) => v.device.parent_hub === d.key);
      if (kids.length) lines.push(t.describe.plugged(kids.map((k) => k.device.name).join(', ')));
    }
  } else if (det.type === 'input') {
    lines.push(t.describe.bus(det.bus.toUpperCase(), !!det.built_in));
  } else if (det.type === 'storage') {
    if (det.letters.length) lines.push(t.describe.letters(det.letters.join(', ')));
  }
  if (d.serial) lines.push(t.describe.serial(d.serial));
  if (showIds && d.hardware_id) lines.push(t.describe.hardwareId(d.hardware_id));
  if (!d.present) lines.push(t.canvas.unplugged);
  return lines;
}

/** Hex VID:PID for the label when ids are shown. */
export function idSuffix(d: Device): string | null {
  const det = d.details;
  if (det.type === 'input' || det.type === 'usb') {
    const h = (n: number) => n.toString(16).padStart(4, '0').toUpperCase();
    return `${h(det.vendor_id)}:${h(det.product_id)}`;
  }
  return d.hardware_id ?? null;
}
