// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/svelte';
import { applyLocale } from '../i18n.ts';
import Avatar from '../ui/Avatar.svelte';
import { avatarSrc, initial, picturesToLoad } from './avatar.ts';

describe('avatar', () => {
  it('builds a data URL only for known image types and clean base64', () => {
    expect(avatarSrc({ mime: 'image/png', data: 'iVBORw==' })).toBe('data:image/png;base64,iVBORw==');
    expect(avatarSrc({ mime: 'image/svg+xml', data: 'PHN2Zy8+' })).toBeNull();
    expect(avatarSrc({ mime: 'image/png', data: '' })).toBeNull();
    expect(avatarSrc({ mime: 'image/png', data: 'a"onerror' })).toBeNull();
    expect(avatarSrc(null)).toBeNull();
  });

  it('uses the first letter in the upper case of the UI language', () => {
    expect(initial('istasyon')).toBe('I');
    applyLocale('tr');
    expect(initial('istasyon')).toBe('İ');
    applyLocale('en');
    expect(initial('  salon')).toBe('S');
    expect(initial('')).toBe('?');
  });

  it('asks once per user, and again for a renamed user', () => {
    expect(picturesToLoad(['ali', 'ali', '', 'veli'], {})).toEqual(['ali', 'veli']);
    expect(picturesToLoad(['ali', 'veli'], { ali: null })).toEqual(['veli']);
    // A renamed seat user is asked for again.
    expect(picturesToLoad(['ayse'], { ali: null, veli: null })).toEqual(['ayse']);
  });

  it('renders the picture clipped, or the generic initial', () => {
    const a = render(Avatar, { src: 'data:image/png;base64,iVBORw==', initial: 'S' });
    expect(a.container.querySelector('img')?.getAttribute('src')).toBe('data:image/png;base64,iVBORw==');
    a.unmount();
    const b = render(Avatar, { src: null, initial: 'S' });
    expect(b.container.querySelector('img')).toBeNull();
    expect(b.container.textContent).toContain('S');
  });
});
