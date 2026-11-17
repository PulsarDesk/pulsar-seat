// SPDX-License-Identifier: GPL-3.0-only
import '@testing-library/jest-dom/vitest';

// jsdom has <dialog> but no showModal / close: a minimal stand-in that sets `open`, makes the
// rest of the page inert (as the top layer does) and fires `close`. Only for tests.
const proto = (globalThis as { HTMLDialogElement?: { prototype: HTMLDialogElement } }).HTMLDialogElement?.prototype;
if (proto && typeof proto.showModal !== 'function') {
  const inerted = new WeakMap<HTMLDialogElement, Element[]>();
  proto.showModal = function (this: HTMLDialogElement) {
    this.setAttribute('open', '');
    const marked: Element[] = [];
    let node: Element | null = this;
    while (node && node.parentElement) {
      for (const sib of Array.from(node.parentElement.children)) {
        if (sib !== node && !sib.hasAttribute('inert')) {
          sib.setAttribute('inert', '');
          marked.push(sib);
        }
      }
      node = node.parentElement;
      if (node === document.body) break;
    }
    inerted.set(this, marked);
  };
  proto.show = function (this: HTMLDialogElement) {
    this.setAttribute('open', '');
  };
  proto.close = function (this: HTMLDialogElement, returnValue?: string) {
    if (!this.hasAttribute('open')) return;
    this.removeAttribute('open');
    for (const el of inerted.get(this) ?? []) el.removeAttribute('inert');
    inerted.delete(this);
    if (returnValue !== undefined) this.returnValue = returnValue;
    this.dispatchEvent(new Event('close'));
  };
}
