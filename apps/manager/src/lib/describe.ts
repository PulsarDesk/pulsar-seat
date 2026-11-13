// SPDX-License-Identifier: GPL-3.0-only
// Human-readable (Turkish) descriptions of devices and masks for tooltips and the device
// info dialog (spec 10 §3.2, §3.5 "connection path", "sharing state").

import { kindLabel, t } from './i18n.ts';
import * as M from './mask.ts';
import type { Device, DeviceKey, DeviceView, MaskSource, SeatMask } from './types.ts';

/** "Boşta", "Bütün koltuklar", "1. koltuk", "1., 3. koltuk". */
export function maskLabel(mask: SeatMask): string {
  if (M.isSpare(mask)) return t.canvas.spareSign;
  if (M.isShared(mask)) return t.assign.allSeats;
  const seats = M.seatsOf(mask);
  return `${seats.map((s) => `${s}.`).join(', ')} koltuk`;
}

export function sourceLabel(source: MaskSource, name: (k: DeviceKey) => string): string {
  if (source.type === 'explicit') return 'Elle atandı';
  if (source.type === 'inherited') {
    // The service also names a port (`port;loc=…`) or a whole device (`cont;id=…`).
    if (source.hub.startsWith('port;')) return t.canvas.inheritedPort(source.hub.slice('port;loc='.length));
    if (source.hub.startsWith('cont;')) return t.canvas.inheritedContainer;
    return t.canvas.inherited(name(source.hub));
  }
  return t.canvas.defaultMask;
}

const OUTPUT: Record<string, string> = {
  internal: 'yerleşik',
  hdmi: 'HDMI',
  displayport: 'DisplayPort',
  dvi: 'DVI',
  vga: 'VGA',
  miracast: 'kablosuz (Miracast)',
  indirect: 'dolaylı (USB / sanal)',
  other: 'diğer'
};

const ISOLATION: Record<string, string> = {
  descriptor: 'Erişim, aygıt güvenlik tanımıyla sınırlanıyor',
  session_letter: 'Sürücü harfi yalnızca sahibinin oturumunda görünüyor',
  minifilter: 'Depolama süzgeciyle sınırlanıyor',
  camera_consent: 'Kamera izniyle sınırlanıyor',
  shared: 'Ortak: herkes kullanabilir',
  not_supported: 'Bu aygıt bu bilgisayarda koltuğa kapatılamıyor',
  network: 'Ağ bağdaştırıcısı: her zaman ortak'
};

export function isolationLabel(code: string | undefined): string | null {
  return code ? (ISOLATION[code] ?? code) : null;
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
  const lines = [`${kindLabel(d.kind)}: ${d.name}`, `Koltuk: ${maskLabel(mask)}`];
  if (d.label) lines.push(`Konum: ${d.label}`);
  const det = d.details;
  if (det.type === 'display') {
    lines.push(`Ekran kartı: ${det.adapter_name}`);
    lines.push(`Bağlantı: ${OUTPUT[det.output_technology] ?? det.output_technology}`);
    if (det.mode) lines.push(`Çözünürlük: ${det.mode.width}×${det.mode.height}, ${(det.mode.refresh_mhz / 1000).toFixed(0)} Hz`);
  } else if (det.type === 'usb') {
    if (d.kind === 'usb_hub') {
      lines.push(`Port sayısı: ${det.port_count}`);
      const kids = all.filter((v) => v.device.parent_hub === d.key);
      if (kids.length) lines.push(`Takılı: ${kids.map((k) => k.device.name).join(', ')}`);
    }
  } else if (det.type === 'input') {
    lines.push(`Bağlantı türü: ${det.bus.toUpperCase()}${det.built_in ? ' (yerleşik)' : ''}`);
  } else if (det.type === 'storage') {
    if (det.letters.length) lines.push(`Sürücü harfleri: ${det.letters.join(', ')}`);
  }
  if (d.serial) lines.push(`Seri numarası: ${d.serial}`);
  if (showIds && d.hardware_id) lines.push(`Donanım kimliği: ${d.hardware_id}`);
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
