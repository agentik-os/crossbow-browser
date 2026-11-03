// Crossbow's page translator. The renderer evaluates this in every http(s)
// top frame as soon as the frame's script context exists and before any page
// script runs. As in media.js, every built-in it uses is captured here, first,
// and later called only through these references, without iterators or
// regular expressions, so a page that patches built-ins afterwards can at most
// feed its own values to report(), which only this closure holds. Its lists
// are null-prototype objects, so a setter a page puts on Array.prototype never
// sees a record or a report. Nothing here logs a value, and text from the
// browser is only ever written as a text node's value: it is never parsed as
// HTML.
//
// It reads text nodes in document order that the user can see (see `look`),
// never inside script, style,
// code, pre, keyboard or sample text, form fields (textarea, input, select),
// editable content, translate="no" or class="notranslate", or near a
// sensitive field: its form or, for a field outside its form, its group, its
// labels, what it and the elements up to that form or group name by
// aria-labelledby or aria-describedby (see `scopesOf`). A field is one when
// it is a password input, its autocomplete value, read as the HTML standard
// reads it, names a password, a one-time code or a card detail, or the
// vault's card words name it; so is a frame in a form, named for a card part
// or named as payment processors name theirs, an editor named for a card
// part or holding a card number, and a shadow host whose open shadow trees
// hold one. A closed
// shadow tree can't be seen from here. Each node's text is sent trimmed;
// its leading and trailing white space stays in place. A document whose root
// or body says translate="no" or class="notranslate" is not read at all: a
// probe or a collect only reports that it declined.
//
// report(...strings) sends one of:
//   "probe", page, then what the document did before it: whether an earlier
//     probe reported a sample ("1" or "0"), the nodes and the UTF-16 units
//     its collects read, and the first pass of the translation it shows, or
//     0; then lang: the root element's lang attribute, and sample: the first
//     visible text, at most the asked number of UTF-16 units.
//   "segments", page, pass, truncated ("1" or "0"), then an id and text for
//     each node, ids counting from 1.
//   "applied", page, pass, written, skipped
//   "restored", page, pass, restored
//   "declined", page: the document asks not to be translated.
//   "hello", page, then what the document did before it, as for a probe:
//     nothing read. By the number, the browser tells a document the
//     back/forward cache kept from a new one; by what it did before, one it
//     no longer keeps a record of goes on from there, as the script's own
//     state comes back with the document.
//
// The returned function runs one command from the browser, every part a
// string: hello, probe(sample), collect, apply(pass, id, text, id, text...)
// and restore(pass). A collect starts a pass, and the first after the page
// loads or after a restore starts a translation. Within a translation, a
// collect reads only text nodes it hasn't read yet, or whose text the page
// changed since, so a same-document change sends only what is new. Over all
// its collects, the document reads at most MAX_SEGMENTS nodes and MAX_CHARS
// UTF-16 units, a node counting each time it is sent, a later translation's
// included; the browser holds it to the same total. Apply writes a node of
// the pass only while it still holds the text it was read with and is still
// in the document, outside an editor, a sensitive form and every other place
// it would not have read. Restore, for any pass of the translation, ends it: it puts the
// original back wherever a node still holds what apply wrote; text the page
// changed is left alone.
(function (report) {
  'use strict';
  const apply = Reflect.apply;
  const gopd = Object.getOwnPropertyDescriptor;
  const get = (owner, name) => { const d = gopd(owner, name); return d === undefined ? undefined : d.get; };
  const set = (owner, name) => { const d = gopd(owner, name); return d === undefined ? undefined : d.set; };
  const doc = document;
  const documentElement = get(Document.prototype, 'documentElement');
  const body = get(Document.prototype, 'body');
  const firstChild = get(Node.prototype, 'firstChild');
  const nextSibling = get(Node.prototype, 'nextSibling');
  const parentNode = get(Node.prototype, 'parentNode');
  const nodeType = get(Node.prototype, 'nodeType');
  const nodeValue = get(Node.prototype, 'nodeValue');
  const setNodeValue = set(Node.prototype, 'nodeValue');
  const tagName = get(Element.prototype, 'tagName');
  const namespace = get(Element.prototype, 'namespaceURI');
  const getAttribute = Element.prototype.getAttribute;
  const queryAll = Document.prototype.querySelectorAll;
  const queryFragment = DocumentFragment.prototype.querySelectorAll;
  const byId = Document.prototype.getElementById;
  const shadowRootOf = get(Element.prototype, 'shadowRoot');
  const itemAt = NodeList.prototype.item;
  const countOf = get(NodeList.prototype, 'length');
  const contains = Node.prototype.contains;
  const hasInstance = Function.prototype[Symbol.hasInstance];
  const InputCtor = HTMLInputElement;
  const SelectCtor = HTMLSelectElement;
  const AreaCtor = HTMLTextAreaElement;
  const inputType = get(InputCtor.prototype, 'type');
  // A field's form owner and labels, by its own type's getters.
  const formOf = [get(InputCtor.prototype, 'form'), get(SelectCtor.prototype, 'form'), get(AreaCtor.prototype, 'form')];
  const labelsOf = [get(InputCtor.prototype, 'labels'), get(SelectCtor.prototype, 'labels'), get(AreaCtor.prototype, 'labels')];
  const Marks = WeakSet;
  const marked = WeakSet.prototype.has;
  const mark = WeakSet.prototype.add;
  const visible = Element.prototype.checkVisibility;
  const boundingRect = Element.prototype.getBoundingClientRect;
  const rectAt = (name) => get(DOMRectReadOnly.prototype, name);
  const rectWidth = rectAt('width');
  const rectHeight = rectAt('height');
  const rectLeft = rectAt('left');
  const rectRight = rectAt('right');
  const rectBottom = rectAt('bottom');
  const clientWidth = get(Element.prototype, 'clientWidth');
  const createRange = Document.prototype.createRange;
  const selectNodeContents = Range.prototype.selectNodeContents;
  const rangeBox = Range.prototype.getBoundingClientRect;
  const assignedSlot = get(Element.prototype, 'assignedSlot');
  const textSlot = get(Text.prototype, 'assignedSlot');
  const shadowHost = typeof ShadowRoot === 'function' ? get(ShadowRoot.prototype, 'host') : undefined;
  const win = globalThis;
  // Window's attributes live on the global itself.
  const scrollXOf = get(win, 'scrollX');
  const scrollYOf = get(win, 'scrollY');
  const styleOf = getComputedStyle;
  const property = CSSStyleDeclaration.prototype.getPropertyValue;
  const editable = get(HTMLElement.prototype, 'isContentEditable');
  const charCodeAt = String.prototype.charCodeAt;
  const slice = String.prototype.slice;
  const toLowerCase = String.prototype.toLowerCase;
  const includes = String.prototype.includes;
  const indexOf = String.prototype.indexOf;
  const lastIndexOf = String.prototype.lastIndexOf;
  const text = String;
  const toNumber = Number;
  const isInteger = Number.isInteger;
  const floor = Math.floor;
  const random = Math.random;
  const create = Object.create;
  const Records = WeakMap;
  const recordOf = WeakMap.prototype.get;
  const keep = WeakMap.prototype.set;
  // Each text node the document has read, and its record. A page can't reach it.
  const read = new WeakMap();
  const HTML = 'http://www.w3.org/1999/xhtml';
  const MAX_SEGMENTS = 2000;
  const MAX_CHARS = 100000;
  const MAX_SEGMENT_CHARS = 2000;
  const MAX_SAMPLE = 2000;
  const MAX_LANG = 64;
  // Both spellings Chromium has used for these options. Unlike the vault,
  // not contentVisibilityAuto: a section content-visibility: auto skips
  // because it is out of view is shown once scrolled to.
  const VISIBILITY = { __proto__: null, visibilityProperty: true, opacityProperty: true, checkVisibilityCSS: true, checkOpacity: true };
  // As the vault's rule for fields (login.js `visible`) reads them: an
  // element and its ancestors together at least this opaque, and no
  // property here off its initial value, which hides nothing.
  const MIN_OPACITY = 0.5;
  const HIDING = ['clip', 'auto', 'clip-path', 'none', 'filter', 'none', 'mask-image', 'none', '-webkit-mask-image', 'none', 'mix-blend-mode', 'normal', '-webkit-mask-box-image', 'none', '-webkit-mask-box-image-source', 'none', 'mask-border', 'none', 'mask-border-source', 'none'];
  // A box that clips what overflows it hides its text when it is smaller
  // than this, in CSS pixels, either way.
  const MIN_SIDE = 8;
  // Text drawn less tall than this, in CSS pixels, can't be read.
  const MIN_TEXT_HEIGHT = 4;
  // Without every built-in the rule needs, nothing is read: unknown means hidden.
  const able = visible !== undefined && boundingRect !== undefined && rectWidth !== undefined && rectHeight !== undefined && rectLeft !== undefined && rectRight !== undefined
    && rectBottom !== undefined && clientWidth !== undefined && createRange !== undefined && selectNodeContents !== undefined && rangeBox !== undefined
    && assignedSlot !== undefined && textSlot !== undefined && shadowHost !== undefined && scrollXOf !== undefined && scrollYOf !== undefined
    && shadowRootOf !== undefined && queryFragment !== undefined && byId !== undefined;
  // More autocomplete tokens than any valid value has: then the field counts
  // as sensitive.
  const MAX_TOKENS = 16;
  // A field's role on a card, and the words that give one, as the vault
  // reads them for Fill Card (login.js), character for character; here only
  // whether a field has a role counts.
  const NONE = -1;
  const CARD_NAME = 0, CARD_GIVEN = 1, CARD_FAMILY = 2, CARD_NUMBER = 3, CARD_EXP = 4, CARD_MONTH = 5, CARD_YEAR = 6, CSC = 7;
  const CARD_WORDS = [
    [CSC, ['cvc', 'cvv', 'csc', 'cvn', 'security code', 'card code', 'verification']],
    [NONE, ['zip', 'postal', 'coupon', 'promo', 'gift', 'email', 'phone', 'birth', 'dob']],
    [CARD_MONTH, ['exp month', 'expiry month', 'expiration month', 'expmonth', 'exp mm', 'month']],
    [CARD_YEAR, ['exp year', 'expiry year', 'expiration year', 'expyear', 'exp yy', 'year']],
    [CARD_EXP, ['expiry', 'expiration', 'exp date', 'expdate', 'expires', 'cc exp', 'ccexp', 'valid thru', 'valid through', 'mm yy']],
    [CARD_NUMBER, ['card number', 'cardnumber', 'card no', 'ccnumber', 'cc number', 'credit card', 'debit card', 'number on card']],
    [CARD_NAME, ['name on card', 'nameoncard', 'cardholder', 'card holder', 'holder name', 'ccname', 'cc name']],
  ];
  // At most this many characters of a label are read.
  const MAX_LABEL = 200;
  // Words payment processors name their card frames with, in a frame's
  // title, name, id, aria-label or address, read as `wordRole` reads labels.
  // Any one makes a frame hold a secret, wherever it is: a frame named so
  // for another reason keeps the text around it unread too.
  const PAYMENT_WORDS = [[CARD_NUMBER, ['card', 'payment', 'checkout', 'billing', 'stripe', 'braintree', 'adyen', 'paypal', 'klarna', 'squareup', 'worldpay', 'cvc', 'cvv', 'iban', 'securefield', 'secure field', 'hostedfield', 'hosted field']]];
  const PAYMENT_NAMES = ['title', 'name', 'id', 'aria-label', 'src'];
  const REFERENCES = ['aria-labelledby', 'aria-describedby'];
  // A sensitive field's form or group holding no text reaches up at most
  // this many ancestors for the nearest that does.
  const CLIMB = 4;
  // At most this many nodes are visited to find whether an element holds
  // text, or to read an editor or a label; past them it counts as holding
  // some.
  const MAX_VISITS = 1024;
  const CONTACT = create(null);
  const contact = ['home', 'work', 'mobile', 'fax', 'pager'];
  for (let i = 0; i < contact.length; i += 1) CONTACT[contact[i]] = true;
  const SKIPPED = create(null);
  const skipped = ['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'CODE', 'PRE', 'KBD', 'SAMP', 'VAR', 'TEXTAREA', 'INPUT', 'SELECT', 'OPTION', 'OPTGROUP', 'IFRAME', 'OBJECT', 'CANVAS', 'VIDEO', 'AUDIO'];
  for (let i = 0; i < skipped.length; i += 1) SKIPPED[skipped[i]] = true;
  // -- Everything below calls only the references captured above. --

  const page = text(floor(random() * 9007199254740990) + 1);
  let pass = 0;
  // The first pass in the translation shown now, or 0.
  let first = 0;
  // Whether a probe has reported a sample, which the browser may have sent on.
  let sampled = false;
  // A list a page can't reach: writing past its end defines an element,
  // never calling a setter a page put on Array.prototype, and
  // Reflect.apply reads it as an array-like.
  const list = () => {
    const made = create(null);
    made.length = 0;
    return made;
  };
  const append = (to, value) => {
    to[to.length] = value;
    to.length += 1;
  };
  // The pass's records, index id - 1, and the translation's, each once:
  // { node, original, lead, core, trail, written, first }.
  let segments = list();
  let shown = list();
  // What the document has read, over all its collects.
  let spentNodes = 0;
  let spentUnits = 0;
  // Elements near a sensitive field, found again for each command.
  let scopes = new Marks();
  // How each element between a slot and its host shows what passes through
  // it (`through`), found once in each walk.
  let chains = new Records();
  // Whether the document can't scroll past its top edge, its left edge and
  // its right edge, the viewport's width, its scroll offsets, and whether
  // the root or the body clips what overflows sideways, read again for each
  // walk.
  let topEdge = true;
  let leftEdge = true;
  let rightEdge = false;
  let width = 0;
  let offsetX = 0;
  let offsetY = 0;
  let sealed = false;

  // White space as Rust's char::is_whitespace reads it, which the browser
  // trims each item with: an item is never only white space to it.
  const space = (code) => (code >= 9 && code <= 13) || code === 32 || code === 0x85 || code === 160 || code === 0x1680 || (code >= 0x2000 && code <= 0x200a) || code === 0x2028 || code === 0x2029 || code === 0x202f || code === 0x205f || code === 0x3000;
  // Digits, ASCII punctuation and symbols alone aren't worth translating.
  const plain = (code) => space(code) || (code >= 0x21 && code <= 0x40) || (code >= 0x5b && code <= 0x60) || (code >= 0x7b && code <= 0x7e);

  const split = (value) => {
    let start = 0;
    let end = value.length;
    while (start < end && space(apply(charCodeAt, value, [start]))) start += 1;
    while (end > start && space(apply(charCodeAt, value, [end - 1]))) end -= 1;
    let words = false;
    for (let i = start; i < end && !words; i += 1) words = !plain(apply(charCodeAt, value, [i]));
    if (!words) return null;
    return { lead: apply(slice, value, [0, start]), core: apply(slice, value, [start, end]), trail: apply(slice, value, [end]) };
  };

  const hasWord = (value, word) => {
    let from = 0;
    for (;;) {
      const at = apply(indexOf, value, [word, from]);
      if (at < 0) return false;
      const after = at + word.length;
      if ((at === 0 || space(apply(charCodeAt, value, [at - 1]))) && (after === value.length || space(apply(charCodeAt, value, [after])))) return true;
      from = after;
    }
  };

  const is = (C, value) => apply(hasInstance, C, [value]);
  const blank = (code) => code === 32 || code === 9 || code === 10 || code === 12 || code === 13;

  // An autocomplete value's tokens, lowercased, in a list a page can't
  // reach; null for more than MAX_TOKENS.
  const tokensOf = (value) => {
    const lowered = apply(toLowerCase, value, []);
    const tokens = create(null);
    let count = 0;
    let start = -1;
    for (let i = 0; i <= lowered.length; i += 1) {
      if (i < lowered.length && !blank(apply(charCodeAt, lowered, [i]))) {
        if (start < 0) start = i;
        continue;
      }
      if (start < 0) continue;
      if (count === MAX_TOKENS) return null;
      tokens[count] = apply(slice, lowered, [start, i]);
      count += 1;
      start = -1;
    }
    tokens.count = count;
    return tokens;
  };

  // The autofill field name, as the HTML standard reads the tokens: an
  // optional section-*, an optional shipping or billing, an optional home,
  // work, mobile, fax or pager, the field name, then an optional webauthn.
  // Null when they aren't that shape.
  const fieldName = (tokens) => {
    let end = tokens.count;
    if (end > 0 && tokens[end - 1] === 'webauthn') end -= 1;
    if (end === 0) return null;
    let at = end - 2;
    if (at >= 0 && CONTACT[tokens[at]] === true) at -= 1;
    if (at >= 0 && (tokens[at] === 'shipping' || tokens[at] === 'billing')) at -= 1;
    if (at >= 0 && apply(slice, tokens[at], [0, 8]) === 'section-') at -= 1;
    return at < 0 ? tokens[end - 1] : null;
  };

  // A card detail, a password or a one-time code.
  const secret = (token) => apply(slice, token, [0, 3]) === 'cc-' || apply(indexOf, token, ['password']) >= 0 || apply(indexOf, token, ['one-time-code']) >= 0;

  // The node after `node` in document order inside `within`, without its
  // children unless `enter`; null past the end.
  const following = (node, within, enter) => {
    const child = enter ? apply(firstChild, node, []) : null;
    if (child !== null) return child;
    for (let at = node; at !== null && at !== within; at = apply(parentNode, at, [])) {
      const sibling = apply(nextSibling, at, []);
      if (sibling !== null) return sibling;
    }
    return null;
  };

  // The text inside `element`, as its whole text begins: its text nodes'
  // values in document order, until it has `limit` UTF-16 units or
  // MAX_VISITS nodes were visited. Read only to decide, never sent.
  const textOf = (element, limit) => {
    let value = '';
    let node = apply(firstChild, element, []);
    for (let visits = 0; node !== null && visits < MAX_VISITS && value.length < limit; visits += 1) {
      if (apply(nodeType, node, []) === 3) value += apply(nodeValue, node, []);
      node = following(node, element, true);
    }
    return value;
  };

  // Whether `element` holds a written word outside `skip` (null, or an
  // element inside it already found to hold none), a field, an editor, other
  // markup and every element whose text is never read. Past MAX_VISITS
  // nodes it counts as holding some.
  const holdsText = (element, skip) => {
    let node = apply(firstChild, element, []);
    for (let visits = 0; node !== null; visits += 1) {
      if (visits === MAX_VISITS) return true;
      const type = apply(nodeType, node, []);
      if (type === 3 && split(apply(nodeValue, node, [])) !== null) return true;
      const enter = type === 1 && node !== skip && apply(namespace, node, []) === HTML && SKIPPED[apply(tagName, node, [])] !== true && apply(editable, node, []) !== true;
      node = following(node, element, enter);
    }
    return false;
  };

  // Whether `text` is a card number: 13 to 19 digits, spaces and dashes
  // around them allowed, passing the Luhn check.
  const cardNumber = (text) => {
    let digits = 0;
    let sum = 0;
    for (let i = text.length - 1; i >= 0; i -= 1) {
      const code = apply(charCodeAt, text, [i]);
      if (space(code) || code === 45) continue;
      if (code < 48 || code > 57) return false;
      let digit = code - 48;
      if (digits % 2 === 1) digit = digit * 2 > 9 ? digit * 2 - 9 : digit * 2;
      sum += digit;
      digits += 1;
    }
    return digits >= 13 && digits <= 19 && sum % 10 === 0;
  };

  // The role `table` gives a text by its words, lowercased with every other
  // character a space; null for none.
  const wordRole = (text, table) => {
    if (typeof text !== 'string' || text === '') return null;
    const lower = apply(toLowerCase, text, []);
    let plain = ' ';
    for (let i = 0; i < lower.length && i < MAX_LABEL; i++) {
      const c = lower[i];
      plain += (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ? c : ' ';
    }
    for (let i = 0; i < table.length; i++) {
      const words = table[i][1];
      for (let j = 0; j < words.length; j++) {
        if (apply(includes, plain, [words[j]])) return table[i][0];
      }
    }
    return null;
  };

  // The role `table` gives by its words to an element's labels (a field's, by
  // `kind`; none for -1), then its aria-label, its placeholder, its name and
  // its id, the first that has any, as the vault's labelRole reads them.
  const labelRole = (element, kind, table) => {
    const labels = kind < 0 ? null : apply(labelsOf[kind], element, []);
    const count = labels === null || labels === undefined ? 0 : apply(countOf, labels, []);
    for (let i = 0; i < count && i < 4; i += 1) {
      const role = wordRole(textOf(apply(itemAt, labels, [i]), MAX_LABEL), table);
      if (role !== null) return role;
    }
    const names = ['aria-label', 'placeholder', 'name', 'id'];
    for (let i = 0; i < names.length; i += 1) {
      const role = wordRole(apply(getAttribute, element, [names[i]]), table);
      if (role !== null) return role;
    }
    return null;
  };

  // Whether the vault's card words name `element`: a field the vault could
  // fill (a text, telephone, number or month input, or a select; an email
  // field never), or, with `kind` -1, a frame or an editor, whose title then
  // counts after its names.
  const cardNamed = (element, kind) => {
    if (kind === 0) {
      const type = apply(inputType, element, []);
      if (type !== 'text' && type !== 'tel' && type !== 'number' && type !== 'month') return false;
    } else if (kind !== 1 && kind !== -1) return false;
    let role = labelRole(element, kind, CARD_WORDS);
    if (role === null && kind === -1) role = wordRole(apply(getAttribute, element, ['title']), CARD_WORDS);
    return role !== null && role !== NONE;
  };

  // Whether a field holds a secret: a password input, an autocomplete value
  // naming a password, a one-time code or a card detail (a value that isn't
  // the standard's shape counts when any token names one), or the vault's
  // card words naming it, whatever its autocomplete says.
  const sensitive = (field, kind) => {
    if (kind === 0 && apply(inputType, field, []) === 'password') return true;
    const value = apply(getAttribute, field, ['autocomplete']);
    if (value !== null) {
      const tokens = tokensOf(value);
      if (tokens === null) return true;
      const name = fieldName(tokens);
      if (name !== null && secret(name)) return true;
      for (let i = 0; name === null && i < tokens.count; i += 1) {
        if (secret(tokens[i])) return true;
      }
    }
    return cardNamed(field, kind);
  };

  // Whether `element` is a frame.
  const framed = (element) => apply(namespace, element, []) === HTML && (apply(tagName, element, []) === 'IFRAME' || apply(tagName, element, []) === 'FRAME');

  // Whether `element` is an editor's own element: marked contenteditable,
  // editable, inside nothing editable.
  const editorHost = (element) => {
    if (apply(namespace, element, []) !== HTML || apply(getAttribute, element, ['contenteditable']) === null || apply(editable, element, []) !== true) return false;
    const up = apply(parentNode, element, []);
    return up === null || apply(nodeType, up, []) !== 1 || apply(namespace, up, []) !== HTML || apply(editable, up, []) !== true;
  };

  // The form `element` is in, in its own tree: its nearest form ancestor.
  const formAbove = (element) => {
    let node = apply(parentNode, element, []);
    for (let depth = 0; node !== null && depth < 512; depth += 1) {
      if (apply(nodeType, node, []) !== 1) return null;
      if (apply(namespace, node, []) === HTML && apply(tagName, node, []) === 'FORM') return node;
      node = apply(parentNode, node, []);
    }
    return null;
  };

  // Whether a payment processor's words name a frame.
  const paymentFrame = (frame) => {
    for (let i = 0; i < PAYMENT_NAMES.length; i += 1) {
      if (wordRole(apply(getAttribute, frame, [PAYMENT_NAMES[i]]), PAYMENT_WORDS) !== null) return true;
    }
    return false;
  };

  // Whether a frame or an editor holds a secret: a frame in a form, as a
  // payment processor's is, named for a card part or named as a payment
  // processor names its frames; an editor named for a card part or holding
  // a card number.
  const secretHost = (element, frame) => frame ? formAbove(element) !== null || cardNamed(element, -1) || paymentFrame(element) : cardNamed(element, -1) || cardNumber(textOf(element, 64));

  // Whether the open shadow trees under `host`, however deep, hold a field,
  // a frame or an editor: null for none, else whether one holds a secret.
  const shadowHolds = (host) => {
    const roots = list();
    append(roots, apply(shadowRootOf, host, []));
    let any = null;
    for (let r = 0; r < roots.length; r += 1) {
      const all = apply(queryFragment, roots[r], ['*']);
      const count = apply(countOf, all, []);
      for (let i = 0; i < count; i += 1) {
        const element = apply(itemAt, all, [i]);
        const kind = is(InputCtor, element) ? 0 : is(SelectCtor, element) ? 1 : is(AreaCtor, element) ? 2 : -1;
        const frame = kind < 0 && framed(element);
        if (kind >= 0 || frame || editorHost(element)) {
          if (kind >= 0 ? sensitive(element, kind) : secretHost(element, frame)) return true;
          any = false;
        }
        const root = apply(shadowRootOf, element, []);
        if (root !== null) append(roots, root);
      }
    }
    return any;
  };

  // Every id in the open shadow trees under `host`, however deep, into `ids`.
  const shadowIds = (host, ids) => {
    const roots = list();
    append(roots, apply(shadowRootOf, host, []));
    for (let r = 0; r < roots.length; r += 1) {
      const all = apply(queryFragment, roots[r], ['*']);
      const count = apply(countOf, all, []);
      for (let i = 0; i < count; i += 1) {
        const element = apply(itemAt, all, [i]);
        const id = apply(getAttribute, element, ['id']);
        if (id !== null) ids[id] = true;
        const root = apply(shadowRootOf, element, []);
        if (root !== null) append(roots, root);
      }
    }
  };

  // Mark what `element` names by id in aria-labelledby and
  // aria-describedby: every id, however many, one lookup each.
  const refer = (element, found) => {
    for (let i = 0; i < REFERENCES.length; i += 1) {
      const value = apply(getAttribute, element, [REFERENCES[i]]);
      if (value === null) continue;
      let start = -1;
      for (let at = 0; at <= value.length; at += 1) {
        if (at < value.length && !blank(apply(charCodeAt, value, [at]))) {
          if (start < 0) start = at;
          continue;
        }
        if (start < 0) continue;
        const named = apply(byId, doc, [apply(slice, value, [start, at])]);
        if (named !== null) apply(mark, found, [named]);
        start = -1;
      }
    }
  };

  // For a sensitive item: mark what each element from its parent up to
  // `top`, its form or group as left unread, names by aria-labelledby or
  // aria-describedby, as a fieldset or a role=group is named, and each
  // label among them. Up to the body when `top` isn't above it.
  const around = (element, top, main, found) => {
    let node = apply(parentNode, element, []);
    for (let depth = 0; node !== null && node !== main && depth < 512; depth += 1) {
      if (apply(nodeType, node, []) !== 1) return;
      refer(node, found);
      if (apply(namespace, node, []) === HTML && apply(tagName, node, []) === 'LABEL') apply(mark, found, [node]);
      if (node === top) return;
      node = apply(parentNode, node, []);
    }
  };

  // What to leave unread for a sensitive field whose form or group is
  // `start`: `start` if it is the body or holds text, else the nearest
  // among its CLIMB ancestors below the body that holds some; with none,
  // `start`. Reaching the body would leave the whole page unread.
  const climb = (start, main) => {
    let from = null;
    let node = start;
    for (let level = 0; level <= CLIMB; level += 1) {
      if (node === main) return level === 0 ? main : start;
      if (node === null || apply(nodeType, node, []) !== 1) return start;
      if (holdsText(node, from)) return node;
      from = node;
      node = apply(parentNode, node, []);
    }
    return start;
  };

  // The nearest element holding both `field` and `other`.
  const common = (field, other) => {
    let node = apply(parentNode, field, []);
    for (let depth = 0; node !== null && depth < 512; depth += 1) {
      if (apply(nodeType, node, []) !== 1) return null;
      if (apply(contains, node, [other]) === true) return node;
      node = apply(parentNode, node, []);
    }
    return null;
  };

  // The elements whose text is never read or written, found with one query
  // for every element the document holds. Each field, each frame and editor
  // holding a secret, and each shadow host whose open shadow trees hold a
  // field, a frame or an editor (standing for them), is an item; one
  // holding a secret is sensitive. For a sensitive item: its form owner (a
  // field's own: the form its form attribute names, else its form
  // ancestor; for any other item, its form ancestor) or, for an item
  // outside its form owner or without one, its group; a field's labels and
  // every label whose for names the item's id or, for a shadow host, an id
  // in its open shadow trees; what the item, its form owner and the
  // elements between the item and that form or group name by
  // aria-labelledby or aria-describedby, and each label among them. A
  // group is the nearest element below the body holding the item and the
  // item beside it in document order among such loose items; without one,
  // the item's parent, which is the body itself for an item placed directly
  // in the page, so then nothing in the page is read. A form owner or group
  // that holds no text itself reaches up to the nearest ancestor that does
  // (`climb`), as a lone field's label sits in a box beside its own.
  const scopesOf = () => {
    const found = new Marks();
    // Without them nothing is read anyway (`walk`).
    if (!able) return found;
    const main = apply(body, doc, []);
    const all = apply(queryAll, doc, ['*']);
    const count = apply(countOf, all, []);
    const loose = create(null);
    let looseCount = 0;
    // Labels naming an element by for; sensitive items' ids, and every id
    // in a sensitive host's open shadow trees.
    const fors = list();
    const ids = create(null);
    for (let i = 0; i < count; i += 1) {
      const element = apply(itemAt, all, [i]);
      const kind = is(InputCtor, element) ? 0 : is(SelectCtor, element) ? 1 : is(AreaCtor, element) ? 2 : -1;
      let owner = null;
      let held = false;
      if (kind >= 0) {
        owner = apply(formOf[kind], element, []);
        held = sensitive(element, kind);
      } else {
        if (apply(namespace, element, []) === HTML && apply(tagName, element, []) === 'LABEL' && apply(getAttribute, element, ['for']) !== null) append(fors, element);
        const frame = framed(element);
        held = (frame || editorHost(element)) && secretHost(element, frame);
        const inside = apply(shadowRootOf, element, []) === null ? null : shadowHolds(element);
        if (!held && inside === null) continue;
        held = held || inside === true;
        owner = formAbove(element);
      }
      if (held) {
        if (owner !== null) {
          const top = climb(owner, main);
          apply(mark, found, [top]);
          refer(owner, found);
          if (apply(contains, owner, [element]) === true) around(element, top, main, found);
        }
        refer(element, found);
        const labels = kind < 0 ? null : apply(labelsOf[kind], element, []);
        const labelCount = labels === null ? 0 : apply(countOf, labels, []);
        for (let j = 0; j < labelCount; j += 1) apply(mark, found, [apply(itemAt, labels, [j])]);
        const id = apply(getAttribute, element, ['id']);
        if (id !== null) ids[id] = true;
        if (kind < 0 && apply(shadowRootOf, element, []) !== null) shadowIds(element, ids);
      }
      if (owner === null || apply(contains, owner, [element]) !== true) {
        loose[looseCount] = { __proto__: null, field: element, held: held };
        looseCount += 1;
      }
    }
    for (let i = 0; i < looseCount; i += 1) {
      if (!loose[i].held) continue;
      const field = loose[i].field;
      let group = null;
      for (let k = i - 1; k <= i + 1; k += 2) {
        if (k < 0 || k >= looseCount) continue;
        const shared = common(field, loose[k].field);
        const below = shared !== null && shared !== main && (main === null || apply(contains, main, [shared]) === true);
        if (below && (group === null || apply(contains, group, [shared]) === true)) group = shared;
      }
      if (group === null) group = apply(parentNode, field, []);
      if (group === null) continue;
      const top = climb(group, main);
      apply(mark, found, [top]);
      around(field, top, main, found);
    }
    for (let i = 0; i < fors.length; i += 1) {
      if (ids[apply(getAttribute, fors[i], ['for'])] === true) apply(mark, found, [fors[i]]);
    }
    return found;
  };

  // Whether `element` asks for its text to be left alone: translate="no" or
  // class="notranslate".
  const optedOut = (element) => {
    const translate = apply(getAttribute, element, ['translate']);
    if (translate !== null && apply(toLowerCase, translate, []) === 'no') return true;
    const classes = apply(getAttribute, element, ['class']);
    return classes !== null && hasWord(apply(toLowerCase, classes, []), 'notranslate');
  };

  // Whether the whole document does, on its root or its body.
  const declined = () => {
    const root = apply(documentElement, doc, []);
    const main = apply(body, doc, []);
    return (root !== null && optedOut(root)) || (main !== null && optedOut(main));
  };

  // Whether text inside `element` may be translated at all, shown or not.
  const allowed = (element) => {
    if (apply(namespace, element, []) !== HTML) return false;
    const tag = apply(tagName, element, []);
    if (SKIPPED[tag] === true) return false;
    if (optedOut(element)) return false;
    if (apply(editable, element, []) === true) return false;
    return !apply(marked, scopes, [element]);
  };

  // A computed colour's alpha: 0 for transparent, the value after the last
  // comma in rgba() or after the slash in any other function, else 1. An
  // opaque rgb() ending in a zero blue is 1.
  const alphaOf = (colour) => {
    if (colour === 'transparent') return 0;
    let at = apply(lastIndexOf, colour, ['/']);
    if (at < 0 && apply(slice, colour, [0, 5]) === 'rgba(') at = apply(lastIndexOf, colour, [',']);
    if (at < 0) return 1;
    const end = apply(lastIndexOf, colour, [')']);
    const value = apply(slice, colour, [at + 1, end > at ? end : colour.length]);
    const alpha = apply(slice, value, [-1]) === '%' ? toNumber(apply(slice, value, [0, -1])) / 100 : toNumber(value);
    return alpha >= 0 ? alpha : 1;
  };

  // Whether a box lies wholly where the user can't scroll: above the top,
  // or before the left edge when the document can't scroll there, or past
  // the right edge when it can't scroll there either (right to left, or the
  // viewport clips what overflows sideways).
  const past = (box) => (topEdge && apply(rectBottom, box, []) + offsetY < 0) || (leftEdge && apply(rectRight, box, []) + offsetX < 0)
    || (rightEdge && apply(rectLeft, box, []) + offsetX >= width);

  // How `element` shows the text inside it, given the opacity it is drawn
  // with (`inherited`, its ancestors' product): null when it hides it, else
  // its own product and whether text directly in it paints. Hidden when not
  // allowed, or, as the vault decides for fields: not shown by display,
  // visibility, opacity or content-visibility: hidden (checkVisibility),
  // under MIN_OPACITY with its ancestors, or any HIDING property set. Then,
  // for text: a box that clips its overflow and is under MIN_SIDE either
  // way, or that lies wholly where the user can't scroll (`past`). The
  // vault's viewport and hit tests don't apply: text below the fold or under
  // a sticky header is still shown. Text paints unless masked by
  // -webkit-text-security, sized 0, or in a colour or fill whose alpha, with
  // the opacity it is drawn with, is under MIN_OPACITY. An element with
  // display: contents has no box: only its visibility and what its text
  // inherits count. `viewport` is true for the root and the body, whose
  // overflow is the viewport's and clips nothing, though clipping sideways
  // seals the right edge.
  const look = (element, inherited, viewport) => {
    if (!allowed(element)) return null;
    const style = apply(styleOf, win, [element]);
    const contents = apply(property, style, ['display']) === 'contents';
    if (apply(visible, element, [VISIBILITY]) !== true) {
      if (!contents) return null;
      const visibility = apply(property, style, ['visibility']);
      if (visibility === 'hidden' || visibility === 'collapse') return null;
    }
    let opacity = inherited;
    if (!contents) {
      const own = toNumber(apply(property, style, ['opacity']));
      if (!(own >= 0)) return null;
      opacity *= own;
      if (!(opacity >= MIN_OPACITY)) return null;
      for (let i = 0; i < HIDING.length; i += 2) {
        const value = apply(property, style, [HIDING[i]]);
        if (value !== '' && value !== HIDING[i + 1]) return null;
      }
      const box = apply(boundingRect, element, []);
      const sideways = apply(property, style, ['overflow-x']);
      if (viewport === true && (sideways === 'hidden' || sideways === 'clip')) sealed = true;
      const clips = viewport !== true && (sideways !== 'visible' || apply(property, style, ['overflow-y']) !== 'visible');
      if (clips && !(apply(rectWidth, box, []) >= MIN_SIDE && apply(rectHeight, box, []) >= MIN_SIDE)) return null;
      if (past(box)) return null;
    }
    const security = apply(property, style, ['-webkit-text-security']);
    const paints = (security === '' || security === 'none') && apply(property, style, ['font-size']) !== '0px'
      && alphaOf(apply(property, style, ['color'])) * opacity >= MIN_OPACITY && alphaOf(apply(property, style, ['-webkit-text-fill-color'])) * opacity >= MIN_OPACITY;
    return { __proto__: null, opacity: opacity, paints: paints };
  };

  // How a slotted node is shown through the elements between its slot and
  // `host` in an open shadow tree, given the host's opacity: null when one
  // hides it, else the opacity through them all and whether text the slot
  // passes on paints, with that opacity. A closed shadow tree's slots aren't
  // returned, so its wrappers are never met, as for the vault. Each element
  // on the way is looked at once in a walk, from the host down, and kept in
  // `chains`: every child a host has is drawn with the host's opacity, and
  // an element between a slot and a host leads to that host alone, so its
  // answer holds for every node slotted through it.
  const through = (slot, host, opacity) => {
    // Up from the slot to the host, or to an element already answered.
    const climbed = list();
    let node = slot;
    let known;
    for (let depth = 0; ; depth += 1) {
      if (node === host) {
        known = { __proto__: null, opacity: opacity, paints: null };
        break;
      }
      if (depth === 512 || node === null || apply(nodeType, node, []) !== 1) return null;
      known = apply(recordOf, chains, [node]);
      if (known !== undefined) break;
      append(climbed, node);
      const next = apply(assignedSlot, node, []);
      let up = next !== null && next !== undefined ? next : apply(parentNode, node, []);
      if (up !== null && apply(nodeType, up, []) === 11) up = apply(shadowHost, up, []);
      node = up;
    }
    // Back down: each element is looked at with the opacity above it.
    for (let i = climbed.length - 1; i >= 0; i -= 1) {
      const wrapper = known === null ? null : look(climbed[i], known.opacity);
      known = wrapper === null ? null : { __proto__: null, opacity: wrapper.opacity, paints: wrapper.paints };
      apply(keep, chains, [climbed[i], known]);
    }
    return known === null || known.paints === null ? null : known;
  };

  // Whether a text node is drawn where the user can see it: laid out at all
  // (a shadow host's text outside every slot, open or closed, has no box),
  // at least MIN_TEXT_HEIGHT tall and a pixel wide, so not scaled away, and
  // not wholly where the user can't scroll, where text-indent can push it
  // while its element stays on the page.
  const placed = (range, node) => {
    apply(selectNodeContents, range, [node]);
    const box = apply(rangeBox, range, []);
    return apply(rectHeight, box, []) >= MIN_TEXT_HEIGHT && apply(rectWidth, box, []) >= 1 && !past(box);
  };

  // Recheck before writing: the page can have moved an unchanged text node
  // into an editor or a password form while the answer was on its way. Text
  // that was merely hidden since is still written.
  const writable = (node) => {
    const root = apply(documentElement, doc, []);
    for (let parent = apply(parentNode, node, []); parent !== null; parent = apply(parentNode, parent, [])) {
      if (apply(nodeType, parent, []) !== 1 || !allowed(parent)) return false;
      if (parent === root) return true;
    }
    return false;
  };

  // Every text node the user can see under the body, in document order,
  // until `take` returns false. The root element is checked as any other
  // ancestor. Each element's style and box are read once, on the way down.
  const walk = (take) => {
    if (!able) return;
    const top = apply(documentElement, doc, []);
    let root = apply(body, doc, []);
    if (root === null) root = top;
    if (root === null) return;
    // A horizontal page starts at its top; left to right, also at its left;
    // right to left, at its right, which it never scrolls past, nor does a
    // page whose root or body clips what overflows sideways. Vertical pages
    // are read wherever their boxes lie.
    const flow = top === null ? null : apply(styleOf, win, [top]);
    const mode = flow === null ? '' : apply(property, flow, ['writing-mode']);
    const rtl = flow !== null && apply(property, flow, ['direction']) === 'rtl';
    topEdge = mode === '' || mode === 'horizontal-tb';
    leftEdge = topEdge && !rtl;
    rightEdge = false;
    sealed = false;
    chains = new Records();
    width = top === null ? 0 : apply(clientWidth, top, []);
    offsetX = apply(scrollXOf, win, []);
    offsetY = apply(scrollYOf, win, []);
    let opacity = 1;
    if (root !== top && top !== null) {
      const above = look(top, 1, true);
      if (above === null) return;
      opacity = above.opacity;
    }
    // How each element whose children are walked shows them, from the root down.
    const path = create(null);
    let depth = 0;
    path[0] = look(root, opacity, true);
    if (path[0] === null) return;
    rightEdge = topEdge && (rtl || sealed);
    const range = apply(createRange, doc, []);
    let node = apply(firstChild, root, []);
    while (node !== null) {
      const type = apply(nodeType, node, []);
      const parent = path[depth];
      if (type === 3) {
        const slot = apply(textSlot, node, []);
        const drawn = slot === null || slot === undefined ? parent : through(slot, apply(parentNode, node, []), parent.opacity);
        if (drawn !== null && drawn.paints && placed(range, node) && !take(node)) return;
      } else if (type === 1) {
        const slot = apply(assignedSlot, node, []);
        const drawn = slot === null || slot === undefined ? parent : through(slot, apply(parentNode, node, []), parent.opacity);
        const inner = drawn === null ? null : look(node, drawn.opacity);
        const child = inner === null ? null : apply(firstChild, node, []);
        if (child !== null) {
          depth += 1;
          path[depth] = inner;
          node = child;
          continue;
        }
      }
      while (node !== root) {
        const sibling = apply(nextSibling, node, []);
        if (sibling !== null) { node = sibling; break; }
        node = apply(parentNode, node, []);
        if (node === null) return;
        depth -= 1;
      }
      if (node === root) return;
    }
  };

  const probe = (limit) => {
    if (declined()) { apply(report, undefined, ['declined', page]); return; }
    scopes = scopesOf();
    const root = apply(documentElement, doc, []);
    let lang = root === null ? null : apply(getAttribute, root, ['lang']);
    lang = lang === null ? '' : apply(slice, lang, [0, MAX_LANG]);
    let sample = '';
    walk((node) => {
      const part = split(apply(nodeValue, node, []));
      if (part === null) return true;
      sample = sample === '' ? part.core : sample + '\n' + part.core;
      return sample.length < limit;
    });
    const before = sampled ? '1' : '0';
    if (sample !== '') sampled = true;
    apply(report, undefined, ['probe', page, before, text(spentNodes), text(spentUnits), text(first), lang, apply(slice, sample, [0, limit])]);
  };

  // End the translation: every original back where its node still holds
  // the translation.
  const restore = () => {
    let restored = 0;
    for (let i = 0; i < shown.length; i += 1) {
      const record = shown[i];
      if (record.written === null) continue;
      if (apply(nodeValue, record.node, []) === record.written) {
        apply(setNodeValue, record.node, [record.original]);
        restored += 1;
      }
      record.written = null;
    }
    first = 0;
    segments = list();
    shown = list();
    return restored;
  };

  const collect = () => {
    // A translation under way stays, and Show Original still ends it.
    if (declined()) { apply(report, undefined, ['declined', page]); return; }
    scopes = scopesOf();
    pass += 1;
    if (first === 0) first = pass;
    segments = list();
    const args = list();
    append(args, 'segments');
    append(args, page);
    append(args, text(pass));
    append(args, '0');
    let truncated = false;
    walk((node) => {
      const value = apply(nodeValue, node, []);
      let record = apply(recordOf, read, [node]);
      // Read in this translation, and translated since or left as it was.
      if (record !== undefined && record.first === first && (value === record.original || value === record.written)) return true;
      const part = split(value);
      if (part === null) return true;
      if (part.core.length > MAX_SEGMENT_CHARS) { truncated = true; return true; }
      const units = part.core.length;
      if (spentNodes + 1 > MAX_SEGMENTS || spentUnits + units > MAX_CHARS) { truncated = true; return false; }
      spentNodes += 1;
      spentUnits += units;
      if (record === undefined) {
        // Every field defined here, so no setter a page added is ever called.
        record = { __proto__: null, node: node, original: value, lead: '', core: '', trail: '', written: null, first: 0 };
        apply(keep, read, [node, record]);
      }
      if (record.first !== first) append(shown, record);
      record.original = value;
      record.lead = part.lead;
      record.core = part.core;
      record.trail = part.trail;
      record.written = null;
      record.first = first;
      append(segments, record);
      append(args, text(segments.length));
      append(args, part.core);
      return true;
    });
    if (truncated) args[3] = '1';
    apply(report, undefined, args);
  };

  const write = (args) => {
    scopes = scopesOf();
    let written = 0;
    let missed = 0;
    for (let i = 2; i + 1 < args.length; i += 2) {
      const id = toNumber(args[i]);
      const value = args[i + 1];
      const segment = isInteger(id) && id >= 1 && id <= segments.length ? segments[id - 1] : undefined;
      if (segment === undefined || typeof value !== 'string' || segment.written !== null || apply(nodeValue, segment.node, []) !== segment.original || !writable(segment.node)) {
        missed += 1;
        continue;
      }
      const translated = segment.lead + value + segment.trail;
      apply(setNodeValue, segment.node, [translated]);
      segment.written = translated;
      written += 1;
    }
    apply(report, undefined, ['applied', page, text(pass), text(written), text(missed)]);
  };

  return function () {
    const args = arguments;
    const name = args[0];
    if (name === 'hello') {
      apply(report, undefined, ['hello', page, sampled ? '1' : '0', text(spentNodes), text(spentUnits), text(first)]);
    } else if (name === 'probe') {
      const limit = toNumber(args[1]);
      if (isInteger(limit) && limit > 0 && limit <= MAX_SAMPLE) probe(limit);
    } else if (name === 'collect') {
      collect();
    } else if (name === 'apply') {
      if (args[1] === text(pass)) write(args);
    } else if (name === 'restore') {
      // Any pass in the translation: the browser may not have seen the
      // latest collect yet. An apply arriving later finds nothing to write.
      // With nothing shown, any earlier pass is answered with 0 restored:
      // the browser asks again when a document comes back from the
      // back/forward cache before the answer reached it.
      const asked = toNumber(args[1]);
      if (isInteger(asked) && asked >= 1 && asked <= pass && (first === 0 || asked >= first)) {
        const restored = restore();
        apply(report, undefined, ['restored', page, text(asked), text(restored)]);
      }
    }
  };
})
