// SPDX-License-Identifier: GPL-3.0-only
// GPU warnings for the seat page (spec 40 §5): no hardware adapter, hybrid systems, an
// adapter without a known hardware video encoder.

import type { RenderAdapter } from './types.ts';

/** PCI vendors whose current GPUs carry a hardware video encoder stream servers use
 *  (NVIDIA NVENC, AMD AMF, Intel Quick Sync). HYPOTHESIS for other vendors: unknown. */
const ENCODER_VENDORS = [0x10de, 0x1002, 0x8086];
const INTEL = 0x8086;

export type GpuWarning = { kind: 'no_hardware' } | { kind: 'hybrid' } | { kind: 'no_encoder'; name: string };

export function gpuWarnings(adapters: RenderAdapter[], chosenId: string | null): GpuWarning[] {
  if (adapters.length === 0) return [];
  const hw = adapters.filter((a) => a.hardware);
  if (hw.length === 0) return [{ kind: 'no_hardware' }];
  const out: GpuWarning[] = [];
  const vendors = new Set(hw.map((a) => a.vendor_id ?? 0));
  const hybrid = hw.length > 1 && vendors.has(INTEL) && [...vendors].some((v) => v !== INTEL);
  if (hybrid && !chosenId) out.push({ kind: 'hybrid' });
  const chosen = chosenId
    ? hw.find((a) => a.pnp_instance_id === chosenId)
    : [...hw].sort((a, b) => b.dedicated_video_memory - a.dedicated_video_memory)[0];
  if (chosen && chosen.vendor_id !== undefined && !ENCODER_VENDORS.includes(chosen.vendor_id)) {
    out.push({ kind: 'no_encoder', name: chosen.description });
  }
  return out;
}
