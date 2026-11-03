// A stand-in page for running translate.js in JavaScriptCore in the tests:
// a small tree of elements and text nodes with only the DOM members the
// script captures. crossbow.page() builds a document; crossbow.texts() reads
// every text node in it, in order, for byte-for-byte comparisons.
var crossbow = { reports: [], selectors: [], writes: 0, root: null, body: null, scrollX: 0, scrollY: 0, width: 800, calls: { styles: 0, boxes: 0, ranges: 0, texts: 0 } };
// String helpers kept from before any test patches built-ins: the browser's
// own code, which this stands in for, never sees such patches.
var own = {
  slice: Function.prototype.call.bind(String.prototype.slice),
  indexOf: Function.prototype.call.bind(String.prototype.indexOf),
  split: Function.prototype.call.bind(String.prototype.split),
  trim: Function.prototype.call.bind(String.prototype.trim),
  lower: Function.prototype.call.bind(String.prototype.toLowerCase),
};
var HTML_NS = 'http://www.w3.org/1999/xhtml';
var SVG_NS = 'http://www.w3.org/2000/svg';

function define(owner, name, getter, setter) {
  Object.defineProperty(owner, name, { get: getter, set: setter, configurable: true, enumerable: true });
}

function Node() {}
define(Node.prototype, 'firstChild', function () { return this.kids.length > 0 ? this.kids[0] : null; });
define(Node.prototype, 'nextSibling', function () {
  if (!this.up) return null;
  var at = this.up.kids.indexOf(this);
  return at + 1 < this.up.kids.length ? this.up.kids[at + 1] : null;
});
define(Node.prototype, 'parentNode', function () { return this.up || null; });
define(Node.prototype, 'nodeType', function () { return this.type; });
define(Node.prototype, 'nodeValue', function () {
  if (this.type !== 3) return null;
  crossbow.calls.texts += 1;
  return this.value;
}, function (value) {
  if (this.type !== 3) return;
  this.value = String(value);
  crossbow.writes += 1;
});
function Element() {}
Element.prototype = Object.create(Node.prototype);
define(Element.prototype, 'tagName', function () { return this.tag; });
define(Element.prototype, 'namespaceURI', function () { return this.ns; });
Element.prototype.getAttribute = function (name) { return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null; };
// Computed styles: an element's style attribute ("name: value; ..."), then
// for inherited properties the nearest ancestor that declares them, then
// the initial value. -webkit-text-fill-color follows color, as Blink's does.
var INITIAL = {
  display: 'block', visibility: 'visible', opacity: '1', 'overflow-x': 'visible', 'overflow-y': 'visible', clip: 'auto', 'clip-path': 'none',
  filter: 'none', 'mask-image': 'none', '-webkit-mask-image': 'none', 'mix-blend-mode': 'normal', '-webkit-mask-box-image': 'none',
  '-webkit-mask-box-image-source': 'none', 'mask-border': 'none', 'mask-border-source': 'none', '-webkit-text-security': 'none',
  'font-size': '16px', color: 'rgb(0, 0, 0)', direction: 'ltr', 'writing-mode': 'horizontal-tb', 'text-indent': '0px', transform: 'none',
};
var INHERITED = { visibility: true, '-webkit-text-security': true, 'font-size': true, color: true, '-webkit-text-fill-color': true, direction: true, 'writing-mode': true, 'text-indent': true };
function declared(element) {
  var found = {};
  var parts = own.split(element.attrs.style || '', ';');
  for (var i = 0; i < parts.length; i += 1) {
    var at = own.indexOf(parts[i], ':');
    if (at < 0) continue;
    var name = own.trim(own.slice(parts[i], 0, at)), value = own.trim(own.slice(parts[i], at + 1));
    if (name === 'overflow') { found['overflow-x'] = value; found['overflow-y'] = value; } else found[name] = value;
  }
  return found;
}
function computed(element, name) {
  var own = declared(element)[name];
  if (own !== undefined) return own;
  var parent = element.up && element.up.type === 11 ? element.up.hostElement : element.up;
  if (INHERITED[name] && parent && parent.type === 1) return computed(parent, name);
  if (name === '-webkit-text-fill-color') return computed(element, 'color');
  return INITIAL[name] !== undefined ? INITIAL[name] : '';
}
// The slot a shadow host's child is drawn in, from an open or closed shadow
// root; assignedSlot below gives it only from an open one, as Blink does.
function slotOf(node) {
  var host = node.up;
  if (!host || !host.shadow) return null;
  var name = node.type === 1 ? (node.attrs.slot || '') : '';
  return crossbow.find(host.shadow, function (kid) { return kid.tag === 'SLOT' && (kid.attrs.name || '') === name; });
}
function unslotted(node) { return !!(node.up && node.up.shadow) && slotOf(node) === null; }
// checkVisibility ignores visibility and opacity unless explicitly requested,
// and display: contents has no box even though its children can be visible.
// A shadow host's child outside every slot has no box either.
Element.prototype.checkVisibility = function (options) {
  var display = computed(this, 'display');
  if (display === 'none' || display === 'contents' || unslotted(this)) return false;
  if (options && options.visibilityProperty && computed(this, 'visibility') === 'hidden') return false;
  if (options && options.opacityProperty && computed(this, 'opacity') === '0') return false;
  return true;
};
function CSSStyleDeclaration(element) { this.element = element; }
CSSStyleDeclaration.prototype.getPropertyValue = function (name) { return computed(this.element, name); };
function getComputedStyle(element) {
  crossbow.calls.styles += 1;
  return new CSSStyleDeclaration(element);
}
// Boxes: an element's box attribute, "left top width height" in CSS pixels
// from the viewport's corner, else a line at the top.
function DOMRectReadOnly(box) { this.box = box; }
['left', 'top', 'width', 'height'].forEach(function (name, i) { define(DOMRectReadOnly.prototype, name, function () { return this.box[i]; }); });
define(DOMRectReadOnly.prototype, 'right', function () { return this.box[0] + this.box[2]; });
define(DOMRectReadOnly.prototype, 'bottom', function () { return this.box[1] + this.box[3]; });
function boxOf(element) {
  var box = own.split(element.attrs.box || '0 0 800 20', ' ');
  return [Number(box[0]), Number(box[1]), Number(box[2]), Number(box[3])];
}
Element.prototype.getBoundingClientRect = function () {
  crossbow.calls.boxes += 1;
  return new DOMRectReadOnly(boxOf(this));
};
// The root element's client width is the viewport's.
define(Element.prototype, 'clientWidth', function () { return this === crossbow.root ? crossbow.width : boxOf(this)[2]; });
define(globalThis, 'scrollX', function () { return crossbow.scrollX; });
define(globalThis, 'scrollY', function () { return crossbow.scrollY; });
// A range over one text node: one box when the text is laid out, none for
// a shadow host's text outside every slot.
function DOMRectList(count) { this.count = count; }
define(DOMRectList.prototype, 'length', function () { return this.count; });
function Range() { this.node = null; }
Range.prototype.selectNodeContents = function (node) { this.node = node; };
Range.prototype.getClientRects = function () {
  crossbow.calls.ranges += 1;
  return new DOMRectList(this.node && !unslotted(this.node) ? 1 : 0);
};
// A text node's box: its parent's, moved by the text-indent its first line
// takes, and nothing at all under a scale(0) transform or outside every slot.
Range.prototype.getBoundingClientRect = function () {
  crossbow.calls.ranges += 1;
  var node = this.node;
  if (!node || unslotted(node) || !node.up || node.up.type !== 1) return new DOMRectReadOnly([0, 0, 0, 0]);
  var box = boxOf(node.up);
  for (var element = node.up; element && element.type === 1; element = element.up) {
    if (own.indexOf(declared(element).transform || '', 'scale(0)') >= 0) return new DOMRectReadOnly([box[0], box[1], 0, 0]);
  }
  var indent = Number(own.slice(computed(node.up, 'text-indent'), 0, -2)) || 0;
  return new DOMRectReadOnly([box[0] + indent, box[1], box[2], box[3]]);
};
function HTMLElement() {}
HTMLElement.prototype = Object.create(Element.prototype);
define(HTMLElement.prototype, 'isContentEditable', function () {
  for (var node = this; node && node.type === 1; node = node.up) {
    var value = node.attrs.contenteditable;
    if (value === '' || value === 'true') return true;
    if (value === 'false') return false;
  }
  return false;
});
// Form fields, each with its form owner as the HTML standard gives it: the
// form its form attribute names, else its nearest form ancestor.
function HTMLInputElement() {}
HTMLInputElement.prototype = Object.create(HTMLElement.prototype);
function HTMLSelectElement() {}
HTMLSelectElement.prototype = Object.create(HTMLElement.prototype);
function HTMLTextAreaElement() {}
HTMLTextAreaElement.prototype = Object.create(HTMLElement.prototype);
function NodeList(items) { this.items = items; }
NodeList.prototype.item = function (index) { return index < this.items.length ? this.items[index] : null; };
define(NodeList.prototype, 'length', function () { return this.items.length; });
[HTMLInputElement, HTMLSelectElement, HTMLTextAreaElement].forEach(function (Field) {
  define(Field.prototype, 'form', function () {
    var field = this;
    if (Object.prototype.hasOwnProperty.call(field.attrs, 'form')) {
      var named = crossbow.find(crossbow.root, function (node) { return node.type === 1 && node.attrs.id === field.attrs.form; });
      return named && named.tag === 'FORM' ? named : null;
    }
    for (var node = field.up; node && node.type === 1; node = node.up) if (node.tag === 'FORM') return node;
    return null;
  });
  // Its label ancestors, and the labels whose for names its id.
  define(Field.prototype, 'labels', function () {
    var field = this, found = [];
    crossbow.each(crossbow.root, function (node) {
      if (node.tag === 'LABEL' && ((field.attrs.id && node.attrs['for'] === field.attrs.id) || (!node.attrs['for'] && node.contains(field)))) found.push(node);
    });
    return new NodeList(found);
  });
});
define(HTMLInputElement.prototype, 'type', function () { return own.lower(this.attrs.type || 'text'); });
// The fields, or with '*' every element, under `from` in document order,
// never inside a shadow root: as in the DOM, its own query finds those. An
// element is anything with a tag: here an input's type getter hides the
// node type its constructor gave it.
function fieldsUnder(from, selector) {
  crossbow.selectors.push(selector);
  var found = [];
  crossbow.each(from, function (node) { if (selector === '*' ? node.tag !== undefined : node.tag === 'INPUT' || node.tag === 'SELECT' || node.tag === 'TEXTAREA') found.push(node); });
  return new NodeList(found);
}
Element.prototype.querySelectorAll = function (selector) { return fieldsUnder(this, selector); };
// An open shadow root; a closed one is null, as in the DOM.
define(Element.prototype, 'shadowRoot', function () { return this.shadow && this.shadow.mode === 'open' ? this.shadow : null; });
Node.prototype.contains = function (other) {
  for (var node = other; node; node = node.up) if (node === this) return true;
  return false;
};
// An open shadow root's slots are returned; a closed one's are not.
function assignedSlot() { var host = this.up; return host && host.shadow && host.shadow.mode === 'open' ? slotOf(this) : null; }
define(Element.prototype, 'assignedSlot', assignedSlot);
function DocumentFragment() {}
DocumentFragment.prototype = Object.create(Node.prototype);
DocumentFragment.prototype.querySelectorAll = function (selector) { return fieldsUnder(this, selector); };
function ShadowRoot() { this.type = 11; }
ShadowRoot.prototype = Object.create(DocumentFragment.prototype);
define(ShadowRoot.prototype, 'host', function () { return this.hostElement; });
function SVGElement() {}
SVGElement.prototype = Object.create(Element.prototype);
function Text(value) { this.type = 3; this.value = value; this.kids = []; }
Text.prototype = Object.create(Node.prototype);
define(Text.prototype, 'assignedSlot', assignedSlot);
function Document() {}
Document.prototype = Object.create(Node.prototype);
define(Document.prototype, 'documentElement', function () { return crossbow.root; });
define(Document.prototype, 'body', function () { return crossbow.body; });
Document.prototype.querySelectorAll = function (selector) { return fieldsUnder(crossbow.root, selector); };
// The first element in the document, outside every shadow root, with that id.
Document.prototype.getElementById = function (id) {
  if (crossbow.root.attrs.id === id) return crossbow.root;
  return crossbow.find(crossbow.root, function (node) { return node.type === 1 && node.attrs.id === id; });
};
Document.prototype.createRange = function () { return new Range(); };
var document = new Document();

function adopt(parent, kids) {
  parent.kids = kids;
  kids.forEach(function (kid) { kid.up = parent; });
  return parent;
}
// An HTML element: h('P', { class: 'x' }, [t('text')]).
function h(tag, attrs, kids) {
  var element = tag === 'INPUT' ? new HTMLInputElement() : tag === 'SELECT' ? new HTMLSelectElement() : tag === 'TEXTAREA' ? new HTMLTextAreaElement() : new HTMLElement();
  element.type = 1; element.tag = tag; element.ns = HTML_NS; element.attrs = attrs || {};
  return adopt(element, kids || []);
}
function svg(kids) {
  var element = new SVGElement();
  element.type = 1; element.tag = 'svg'; element.ns = SVG_NS; element.attrs = {};
  return adopt(element, kids || []);
}
function t(value) { return new Text(value); }

crossbow.page = function (lang, kids) {
  crossbow.body = h('BODY', {}, kids);
  crossbow.root = h('HTML', lang === null ? {} : { lang: lang }, [h('HEAD', {}, [h('TITLE', {}, [t('Head title, never read')])]), crossbow.body]);
};
// A shadow root on `host`, 'open' or 'closed', holding `kids`, among them
// SLOT elements; the host's own children are drawn in the slot they name.
crossbow.shadow = function (host, mode, kids) {
  var root = adopt(new ShadowRoot(), kids);
  root.hostElement = host;
  root.mode = mode;
  host.shadow = root;
  return host;
};
// What a same-document change does to the page: new elements at the end of `parent`.
crossbow.add = function (parent, kids) {
  kids.forEach(function (kid) { parent.kids.push(kid); kid.up = parent; });
  return 'added';
};
crossbow.find = function (from, test) {
  for (var i = 0; i < from.kids.length; i += 1) {
    var kid = from.kids[i];
    if (test(kid)) return kid;
    var found = crossbow.find(kid, test);
    if (found) return found;
  }
  return null;
};
crossbow.each = function (from, visit) {
  from.kids.forEach(function (kid) { visit(kid); crossbow.each(kid, visit); });
};
// Every text node's value in document order, separated by U+001F.
crossbow.texts = function () {
  var values = [];
  crossbow.each(crossbow.root, function (node) { if (node.type === 3) values.push(node.value); });
  return values.join('\u001f');
};
crossbow.elements = function () {
  var count = 0;
  crossbow.each(crossbow.root, function (node) { if (node.type === 1) count += 1; });
  return count;
};
// Reports since the last call: parts joined by U+001F, reports by U+001E, or
// 'not strings' if any part wasn't a string.
crossbow.sent = function () {
  var all = crossbow.reports.every(function (report) { return report.every(function (part) { return typeof part === 'string'; }); });
  var lines = crossbow.reports.map(function (report) { return report.join('\u001f'); }).join('\u001e');
  crossbow.reports = [];
  return all ? lines : 'not strings';
};
