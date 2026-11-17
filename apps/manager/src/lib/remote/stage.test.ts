// SPDX-License-Identifier: GPL-3.0-only
import { afterEach, describe, expect, it, vi } from 'vitest';
import { countChanges, discardRemoteStage, remoteDirty, remoteStage, saveRemoteStage } from './stage.svelte.ts';

describe('countChanges', () => {
  it('counts changed leaves, a changed list once, and treats null and undefined alike', () => {
    expect(countChanges({ a: 1, b: { c: 2, d: 3 } }, { a: 1, b: { c: 2, d: 3 } })).toBe(0);
    expect(countChanges({ a: 2, b: { c: 2, d: 4 } }, { a: 1, b: { c: 2, d: 3 } })).toBe(2);
    expect(countChanges({ l: [1, 2, 3] }, { l: [1, 2] })).toBe(1);
    expect(countChanges({ x: null }, {})).toBe(0);
    expect(countChanges({ x: { y: 1 } }, { x: null })).toBe(1);
  });
});

describe('remote stage', () => {
  afterEach(() => {
    remoteStage.current = null;
  });

  it('is clean without a page and saves nothing', async () => {
    expect(remoteDirty()).toBe(false);
    expect(await saveRemoteStage()).toBe(true);
  });

  it('saves and discards through the page on screen; an invalid draft is not saved', async () => {
    const save = vi.fn(async () => true);
    const discard = vi.fn();
    remoteStage.current = { count: 2, save, discard };
    expect(remoteDirty()).toBe(true);
    expect(await saveRemoteStage()).toBe(true);
    expect(save).toHaveBeenCalledOnce();
    discardRemoteStage();
    expect(discard).toHaveBeenCalledOnce();
    remoteStage.current = { count: 1, invalid: 'fix it', save, discard };
    expect(await saveRemoteStage()).toBe(false);
    expect(save).toHaveBeenCalledOnce();
  });
});
