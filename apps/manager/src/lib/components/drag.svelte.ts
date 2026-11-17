// SPDX-License-Identifier: GPL-3.0-only
// HTML5 drag and drop of devices on the Seats board (spec 10 §3.4): the payload type and the
// drop-target bookkeeping shared by the pool and the seat cards.

import type { DropTarget } from '#lib/model.ts';
import type { DeviceKey } from '#lib/types.ts';

export const DRAG_TYPE = 'application/x-pulsar-device';

export interface DragPayload {
  key: DeviceKey;
  from: DropTarget;
}

/** The dragged device, or null when the drop is not one of ours. */
export function readDrag(e: DragEvent): DragPayload | null {
  const raw = e.dataTransfer?.getData(DRAG_TYPE);
  if (!raw) return null;
  try {
    const p = JSON.parse(raw) as Partial<DragPayload>;
    if (typeof p.key !== 'string' || (p.from !== 'system' && typeof p.from !== 'number')) return null;
    return { key: p.key, from: p.from };
  } catch {
    return null;
  }
}

export type OnDrop = (key: DeviceKey, from: DropTarget, to: DropTarget) => void;

/**
 * Drop-target state of one container: `active` stays true while a drag is anywhere inside it
 * (enter / leave fire for every child, so they are counted). Only then does the container get
 * its dashed outline, the one dashed line of the app (DESIGN.md).
 */
export class DropZone {
  active = $state(false);
  #depth = 0;
  #target: () => DropTarget;
  #onDrop: () => OnDrop;

  /** Getters, so the zone follows the component's current props. */
  constructor(target: () => DropTarget, onDrop: () => OnDrop) {
    this.#target = target;
    this.#onDrop = onDrop;
  }

  enter = (e: DragEvent) => {
    e.preventDefault();
    this.#depth++;
    this.active = true;
  };

  over = (e: DragEvent) => {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    this.active = true;
  };

  leave = () => {
    this.#depth = Math.max(0, this.#depth - 1);
    if (this.#depth === 0) this.active = false;
  };

  drop = (e: DragEvent) => {
    e.preventDefault();
    this.#depth = 0;
    this.active = false;
    const p = readDrag(e);
    const to = this.#target();
    if (p && p.from !== to) this.#onDrop()(p.key, p.from, to);
  };
}
