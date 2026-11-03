// Crossbow's element hiding, run as a document's script context is created,
// before the page's own scripts. It keeps the browser's CSSStyleSheet,
// replaceSync, the adoptedStyleSheets accessors, Array.prototype.concat and
// Reflect.apply, so a page that replaces them later changes nothing, and
// returns the one function the renderer calls with a sheet. It defines no
// global and runs no matching.
(() => {
  'use strict';
  const apply = Reflect.apply;
  const Sheet = CSSStyleSheet;
  const replaceSync = Sheet.prototype.replaceSync;
  const adopted = Object.getOwnPropertyDescriptor(Document.prototype, 'adoptedStyleSheets');
  const concat = Array.prototype.concat;
  const doc = document;
  return (css) => {
    const sheet = new Sheet();
    apply(replaceSync, sheet, [css]);
    apply(adopted.set, doc, [apply(concat, apply(adopted.get, doc, []), [[sheet]])]);
  };
})()
