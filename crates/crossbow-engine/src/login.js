// Crossbow's login listener. The renderer evaluates this in every http(s) top
// frame, and every frame same-origin with all its ancestors, as soon as the
// frame's script context exists and before any page script runs. CEF has no
// isolated worlds, so it shares the page's world: every built-in it uses is
// captured here, first, and later called only through these references. A
// page that patches built-ins afterwards can at most feed its own values to
// the native functions, which only this closure holds, and never obtains
// them. Nothing here logs, stores or returns a value.
//
// capture(username, password): a trusted submit, with user activation, of a
// form holding a non-empty password field. followed(): a fetch or XHR request
// started after that submit finished. focused(): a field of a login form got
// focus. addressCapture(name, organization, street, city, region, postal
// code, country, phone, email): a trusted, user-activated submit of a form
// whose fields the user can see are address-shaped, with a street and a
// city or postal code; only those fields are read. addressFocused(): a field
// of an address-shaped form, or of address-shaped fields outside any form
// that share a container, got focus, or, once one had, focus moved to
// another such form or group or to anything that reaches none.
// cardFocused(): the same for a card field and card forms or groups.
// The renderer passes it only to a document that may take a card: a secure
// context at an https address, or http on this computer's loopback address,
// inside pages that are each one too, as it read them before any page
// script ran.
//
// addressFilled(outcome, filled, hidden) and cardFilled(...): what an
// address or card fill did, once the engine said which fields the user can
// see (see `gated`): "done", with the number of fields filled and of fields
// left empty because the engine didn't call them clearly visible;
// "focus-hidden", nothing filled, because the focused field isn't; or
// "unconfirmed", nothing filled, because no answer came in time or none can.
// Numbers only, never a value.
//
// In a frame of another site than the page's, which may hold a payment
// processor's card fields, the renderer passes only cardFocused() and
// cardFilled(): nothing there notes a login or address field, and only a
// card is ever filled.
//
// It returns three functions, which only the renderer holds. login(username,
// password, main) fills the focused field's login form and only it, and
// nothing when that field has no login form; with no field focused, and
// only in the main frame (`main` true), the one form with a password field
// the user can see and type into. address(main, ...values) fills, from the
// values the browser computed (see `ADDRESS_VALUES`), only from a focused
// field: its form, or outside any form its group (see `groupOf`), and
// nothing elsewhere; with no field focused, nothing, in any frame.
// card(main, ...values) fills the same way (see `cardGroupOf`), only where
// cardFocused() was passed, never a security code, and never submits: see
// `CARD_VALUES`. None ever fills a hidden, disabled, read-only, off-screen,
// filtered, masked, blended or tiny field. An address or card fill also
// waits, at most `WAIT_MS`, for the engine's own answer, and fills only the
// fields it calls clearly visible (see `gated`); a login fill doesn't wait.
// Each returns false at once when it can fill nothing; an address or card
// fill that went on to wait reports its outcome instead. Without "Offer to
// save passwords from websites", the renderer passes no capture(),
// followed() or addressCapture(): then no submit listener or request
// observer is added at all.
(function (capture, followed, focused, addressCapture, addressFocused, cardFocused, addressFilled, cardFilled) {
  'use strict';
  const apply = Reflect.apply;
  const gopd = Object.getOwnPropertyDescriptor;
  const get = (proto, name) => { const d = gopd(proto, name); return d === undefined ? undefined : d.get; };
  const hasInstance = Function.prototype[Symbol.hasInstance];
  const win = window;
  const doc = document;
  const nav = navigator;
  const perf = performance;
  const EventCtor = Event;
  const InputCtor = HTMLInputElement;
  const SelectCtor = HTMLSelectElement;
  const AreaCtor = HTMLTextAreaElement;
  const FormCtor = HTMLFormElement;
  const Observer = PerformanceObserver;
  const listen = EventTarget.prototype.addEventListener;
  const dispatch = EventTarget.prototype.dispatchEvent;
  const querySelectorAll = Element.prototype.querySelectorAll;
  const queryDocument = Document.prototype.querySelectorAll;
  const getAttribute = Element.prototype.getAttribute;
  const includes = String.prototype.includes;
  const toLowerCase = String.prototype.toLowerCase;
  const trim = String.prototype.trim;
  const slice = String.prototype.slice;
  const textContent = get(Node.prototype, 'textContent');
  const nodeAt = NodeList.prototype.item;
  const nodeCount = get(NodeList.prototype, 'length');
  const formAt = HTMLCollection.prototype.item;
  const formCount = get(HTMLCollection.prototype, 'length');
  const target = get(Event.prototype, 'target');
  const userActivation = get(Navigator.prototype, 'userActivation');
  const isActive = typeof UserActivation === 'function' ? get(UserActivation.prototype, 'isActive') : undefined;
  const activeElement = get(Document.prototype, 'activeElement');
  const docBody = get(Document.prototype, 'body');
  const forms = get(Document.prototype, 'forms');
  const inputType = get(InputCtor.prototype, 'type');
  const inputForm = get(InputCtor.prototype, 'form');
  const disabled = get(InputCtor.prototype, 'disabled');
  const readOnly = get(InputCtor.prototype, 'readOnly');
  const inputLabels = get(InputCtor.prototype, 'labels');
  const maxLength = get(InputCtor.prototype, 'maxLength');
  const selectType = get(SelectCtor.prototype, 'type');
  const selectForm = get(SelectCtor.prototype, 'form');
  const selectDisabled = get(SelectCtor.prototype, 'disabled');
  const selectLabels = get(SelectCtor.prototype, 'labels');
  const options = get(SelectCtor.prototype, 'options');
  const selectedIndex = gopd(SelectCtor.prototype, 'selectedIndex');
  const optionText = get(HTMLOptionElement.prototype, 'text');
  const optionValue = get(HTMLOptionElement.prototype, 'value');
  const areaForm = get(AreaCtor.prototype, 'form');
  const areaDisabled = get(AreaCtor.prototype, 'disabled');
  const areaReadOnly = get(AreaCtor.prototype, 'readOnly');
  const areaLabels = get(AreaCtor.prototype, 'labels');
  const areaValue = gopd(AreaCtor.prototype, 'value');
  const checkVisibility = Element.prototype.checkVisibility;
  const boundingRect = Element.prototype.getBoundingClientRect;
  const rectAt = (name) => get(DOMRectReadOnly.prototype, name);
  const rectWidth = rectAt('width'), rectHeight = rectAt('height'), rectTop = rectAt('top'), rectRight = rectAt('right'), rectBottom = rectAt('bottom'), rectLeft = rectAt('left');
  // Window's attributes and operations live on the global itself.
  const innerWidth = get(win, 'innerWidth');
  const innerHeight = get(win, 'innerHeight');
  const computedStyle = win.getComputedStyle;
  const propertyValue = CSSStyleDeclaration.prototype.getPropertyValue;
  const elementFromPoint = Document.prototype.elementFromPoint;
  const ElementCtor = Element;
  const RootCtor = typeof ShadowRoot === 'function' ? ShadowRoot : undefined;
  const parentNode = get(Node.prototype, 'parentNode');
  const assignedSlot = get(Element.prototype, 'assignedSlot');
  const host = RootCtor === undefined ? undefined : get(RootCtor.prototype, 'host');
  const contains = Node.prototype.contains;
  const value = gopd(InputCtor.prototype, 'value');
  const getValue = value.get;
  const setValue = value.set;
  const now = Performance.prototype.now;
  const observe = Observer.prototype.observe;
  const getEntries = PerformanceObserverEntryList.prototype.getEntries;
  const startTime = get(PerformanceEntry.prototype, 'startTime');
  const initiatorType = get(PerformanceResourceTiming.prototype, 'initiatorType');
  const IO = typeof IntersectionObserver === 'function' ? IntersectionObserver : undefined;
  const EntryCtor = typeof IntersectionObserverEntry === 'function' ? IntersectionObserverEntry : undefined;
  const ioObserve = IO === undefined ? undefined : IO.prototype.observe;
  const ioUnobserve = IO === undefined ? undefined : IO.prototype.unobserve;
  const entryTarget = EntryCtor === undefined ? undefined : get(EntryCtor.prototype, 'target');
  const entryVisible = EntryCtor === undefined ? undefined : get(EntryCtor.prototype, 'isVisible');
  const entryTime = EntryCtor === undefined ? undefined : get(EntryCtor.prototype, 'time');
  const later = win.setTimeout;
  const Weak = WeakMap;
  const weakGet = WeakMap.prototype.get;
  const weakSet = WeakMap.prototype.set;
  const FOLLOW_MS = 10000;
  // How long an address or card fill waits for the engine's answers, and
  // the least time between the engine's answers for one field, which
  // Chromium requires to be at least 100 when it tracks visibility; both in
  // milliseconds.
  const WAIT_MS = 1000, DELAY_MS = 100;
  // A field and its ancestors together at least this opaque. Fainter is hidden.
  const MIN_OPACITY = 0.5;
  // A field's box, as transformed, at least this many CSS pixels each way.
  const MIN_SIDE = 8;
  // Where in its box, as (x, y) fractions, a click must reach a field: its
  // centre, and a quarter of the box in from each corner. A field clipped to
  // a strip, or transformed into a line, whose box is mostly empty, misses one.
  const HIT_POINTS = [0.5, 0.5, 0.25, 0.25, 0.75, 0.25, 0.25, 0.75, 0.75, 0.75];
  const capturing = typeof capture === 'function' && typeof followed === 'function';
  const capturingAddresses = capturing && typeof addressCapture === 'function';
  const notingLogins = typeof focused === 'function';
  const notingAddresses = typeof addressFocused === 'function';
  const notingCards = typeof cardFocused === 'function';
  const reportingAddresses = typeof addressFilled === 'function';
  const reportingCards = typeof cardFilled === 'function';
  // -- Everything below calls only the references captured above. --

  const is = (C, v) => apply(hasInstance, C, [v]);
  const textLike = (type) => type === 'text' || type === 'email' || type === 'tel';

  // A node's parent as layout draws it, in the flat tree: the slot a shadow
  // host's child is drawn in, else its parent, leaving a shadow tree through
  // its host; the document past the root element. A slot inside a closed
  // shadow root isn't returned, so its wrappers are never met: a host whose
  // shadow root is closed looks like any element to script outside it.
  const flatParent = (node) => {
    const slot = apply(assignedSlot, node, []);
    const up = slot !== null && slot !== undefined ? slot : apply(parentNode, node, []);
    return RootCtor !== undefined && is(RootCtor, up) ? apply(host, up, []) : up;
  };
  // Whether a computed style leaves `name` at its initial value, which hides nothing.
  const initial = (style, name, value) => {
    const own = apply(propertyValue, style, [name]);
    return own === '' || own === value;
  };

  // Whether a field is disabled or read-only, by its own type's getters.
  // Anything but an input, a select or a text area is never filled.
  const blocked = (field) => {
    if (is(InputCtor, field)) return apply(disabled, field, []) === true || apply(readOnly, field, []) === true;
    if (is(SelectCtor, field)) return apply(selectDisabled, field, []) === true;
    if (is(AreaCtor, field)) return apply(areaDisabled, field, []) === true || apply(areaReadOnly, field, []) === true;
    return true;
  };

  // A field the user can see: rendered without a hiding style, its box at
  // least 8 CSS pixels each way as transformed, mostly opaque, with no clip,
  // clip-path, filter, mask, mask border image or blend mode on it or on any
  // element it is drawn inside, through slots and shadow hosts, and the
  // element a click reaches at each of HIT_POINTS, all inside this frame's
  // viewport. The hit tests find a field clipped by an ancestor's overflow,
  // covered, or transformed into a line. Every element up to the document is
  // checked; a walk that ends anywhere else means no. Transparent colours
  // aren't read, nor the wrappers of a closed shadow root a field is slotted
  // into, which script outside it can't tell apart from no shadow root.
  // Unknown means no.
  const visible = (input) => {
    if (checkVisibility === undefined || computedStyle === undefined || elementFromPoint === undefined || parentNode === undefined || assignedSlot === undefined) return false;
    if (apply(checkVisibility, input, [{ __proto__: null, opacityProperty: true, visibilityProperty: true, contentVisibilityAuto: true }]) !== true) return false;
    const box = apply(boundingRect, input, []);
    const rect = { __proto__: null, width: apply(rectWidth, box, []), height: apply(rectHeight, box, []), top: apply(rectTop, box, []), right: apply(rectRight, box, []), bottom: apply(rectBottom, box, []), left: apply(rectLeft, box, []) };
    const width = innerWidth === undefined ? 0 : apply(innerWidth, win, []);
    const height = innerHeight === undefined ? 0 : apply(innerHeight, win, []);
    if (!(rect.width >= MIN_SIDE && rect.height >= MIN_SIDE)) return false;
    let opacity = 1;
    for (let node = input, depth = 0; node !== doc; node = flatParent(node), depth++) {
      if (depth > 512 || !is(ElementCtor, node)) return false;
      const style = apply(computedStyle, win, [node]);
      const own = +apply(propertyValue, style, ['opacity']);
      if (!(own >= 0)) return false;
      opacity *= own;
      if (!initial(style, 'clip', 'auto') || !initial(style, 'clip-path', 'none') || !initial(style, 'filter', 'none')) return false;
      // A mask works through its image, which the shorthand may not spell out.
      if (!initial(style, 'mask-image', 'none') || !initial(style, '-webkit-mask-image', 'none') || !initial(style, 'mix-blend-mode', 'normal')) return false;
      // So does a mask border image, which can fill the whole box.
      if (!initial(style, '-webkit-mask-box-image', 'none') || !initial(style, '-webkit-mask-box-image-source', 'none') || !initial(style, 'mask-border', 'none') || !initial(style, 'mask-border-source', 'none')) return false;
    }
    if (!(opacity >= MIN_OPACITY)) return false;
    for (let i = 0; i < HIT_POINTS.length; i += 2) {
      const x = rect.left + rect.width * HIT_POINTS[i], y = rect.top + rect.height * HIT_POINTS[i + 1];
      if (!(x >= 0 && y >= 0 && x < width && y < height)) return false;
      const hit = apply(elementFromPoint, doc, [x, y]);
      if (hit === null || (hit !== input && apply(contains, input, [hit]) !== true)) return false;
    }
    return true;
  };
  // A field the user can see and type into: visible, enabled and not read-only.
  const fillable = (input) => !blocked(input) && visible(input);

  // A form's login fields, or null without a password field: its first
  // password field; for a sign-up or change form whose last two password
  // fields match, the last one as the new password; and as the username the
  // field marked autocomplete=username, else the last text-like field before
  // the first password field. `filling` leaves out every field the user
  // can't see or type into.
  const fields = (form, filling) => {
    const inputs = apply(querySelectorAll, form, ['input']);
    const count = apply(nodeCount, inputs, []);
    let user = null, named = null, first = null, previous = null, last = null, passwords = 0;
    for (let i = 0; i < count; i++) {
      const input = apply(nodeAt, inputs, [i]);
      if (filling && !fillable(input)) continue;
      const type = apply(inputType, input, []);
      if (type === 'password') {
        if (first === null) first = input;
        previous = last;
        last = input;
        passwords++;
        continue;
      }
      if (!textLike(type)) continue;
      if (first === null) user = input;
      const auto = apply(getAttribute, input, ['autocomplete']);
      if (named === null && typeof auto === 'string' && apply(includes, apply(toLowerCase, auto, []), ['username'])) named = input;
    }
    if (first === null) return null;
    let password = first;
    if (passwords >= 2) {
      const newer = apply(getValue, last, []);
      if (newer !== '' && newer === apply(getValue, previous, [])) password = last;
    }
    return { user: named !== null ? named : user, first: first, password: password };
  };

  // What a field holds, trimmed: a select's chosen option's text, unless
  // that option has an empty value, as a "Choose…" option does.
  const held = (field, kind) => {
    let text = '';
    if (kind === 'select') {
      const index = apply(selectedIndex.get, field, []);
      if (index < 0) return '';
      const option = apply(formAt, apply(options, field, []), [index]);
      if (option === null || apply(optionValue, option, []) === '') return '';
      text = apply(optionText, option, []);
    } else {
      text = apply(kind === 'area' ? areaValue.get : getValue, field, []);
    }
    return typeof text === 'string' ? apply(trim, text, []) : '';
  };
  // Send the parts of a submitted address form, read only from fields the
  // user can see as it is submitted, by the rule above and by the engine's
  // latest answer (see `seen`), which the submit doesn't wait for: each
  // role's first field that holds something, the name from its given and
  // family parts when it has no name field, and the street from its lines
  // when it has no street field. False, sending nothing, without a street
  // and a city or postal code. A field scrolled out of the frame's viewport
  // can't be told from one placed outside it to hide, so it isn't read; nor
  // is one the engine never answered for, as before the user clicked into
  // the form, or anywhere without the engine's answer.
  const captureAddress = (form) => {
    const found = addressFields(form, (field) => visible(field) && seen(field, 0));
    if (!shaped(found)) return false;
    const first = { __proto__: null };
    let lines = '';
    for (let i = 0; i < found.count; i++) {
      const item = found.items[i];
      const text = held(item.field, item.kind);
      if (text === '') continue;
      if (item.role >= LINE1 && item.role <= LINE3) lines = lines === '' ? text : lines + '\n' + text;
      else if (first[item.role] === undefined) first[item.role] = text;
    }
    const part = (role) => first[role] === undefined ? '' : first[role];
    const given = part(GIVEN), family = part(FAMILY);
    const parts = { __proto__: null,
      name: part(NAME) !== '' ? part(NAME) : given !== '' && family !== '' ? given + ' ' + family : given + family,
      street: part(STREET) !== '' ? part(STREET) : lines };
    if (parts.street === '' || (part(CITY) === '' && part(POSTAL) === '')) return false;
    addressCapture(parts.name, part(ORGANIZATION), parts.street, part(CITY), part(REGION), part(POSTAL), part(COUNTRY), part(PHONE), part(EMAIL));
    return true;
  };

  // Send the login of a submitted form with a non-empty password. False,
  // sending nothing, for any other form.
  const captureLogin = (form) => {
    const found = fields(form, false);
    if (found === null) return false;
    const password = apply(getValue, found.password, []);
    if (typeof password !== 'string' || password === '') return false;
    const username = found.user === null ? '' : apply(getValue, found.user, []);
    capture(typeof username === 'string' ? username : '', password);
    return true;
  };

  let submitted = -1;
  const onSubmit = function (event) {
    try {
      const trusted = gopd(event, 'isTrusted');
      if (trusted === undefined || apply(trusted.get, event, []) !== true) return;
      if (isActive === undefined || apply(isActive, apply(userActivation, nav, []), []) !== true) return;
      const form = apply(target, event, []);
      if (!is(FormCtor, form)) return;
      // A sign-up form can hold both; each is offered on its own.
      const loginSent = captureLogin(form);
      const addressSent = capturingAddresses && captureAddress(form);
      if (loginSent || addressSent) submitted = apply(now, perf, []);
    } catch (_) {}
  };

  const onEntries = function (list) {
    try {
      if (submitted < 0) return;
      const entries = apply(getEntries, list, []);
      for (let i = 0; i < entries.length; i++) {
        const kind = apply(initiatorType, entries[i], []);
        if ((kind === 'fetch' || kind === 'xmlhttprequest') && apply(startTime, entries[i], []) >= submitted) {
          if (apply(now, perf, []) - submitted <= FOLLOW_MS) followed();
          submitted = -1;
          return;
        }
      }
    } catch (_) {}
  };

  // -- Addresses --

  // A field's role in an address. NONE is a field of some other kind, which
  // is never filled with an address.
  const NONE = -1, NAME = 0, GIVEN = 1, FAMILY = 2, ORGANIZATION = 3, STREET = 4, LINE1 = 5, LINE2 = 6, LINE3 = 7,
    CITY = 8, REGION = 9, POSTAL = 10, COUNTRY = 11, PHONE = 12, EMAIL = 13;
  // The values a fill brings, in order: the name, its given and family
  // parts, the organization, the street on its lines and on one line, its
  // first line, its second, its second line on, its third line on, then the
  // city, region, postal code, country, phone and email. The browser splits
  // them, so nothing here reads a saved value but to compare it with an option.
  const ADDRESS_VALUES = 16;
  // Autocomplete field names that name an address part.
  const TOKENS = { __proto__: null, 'name': NAME, 'given-name': GIVEN, 'family-name': FAMILY, 'organization': ORGANIZATION,
    'street-address': STREET, 'address-line1': LINE1, 'address-line2': LINE2, 'address-line3': LINE3, 'address-level2': CITY,
    'address-level1': REGION, 'postal-code': POSTAL, 'country': COUNTRY, 'country-name': COUNTRY, 'tel': PHONE, 'tel-national': PHONE, 'email': EMAIL };
  // Autocomplete field names of other kinds of field, as are all those of cards ("cc-").
  const OTHER = { __proto__: null, 'username': true, 'current-password': true, 'new-password': true, 'one-time-code': true,
    'nickname': true, 'organization-title': true, 'honorific-prefix': true, 'honorific-suffix': true, 'additional-name': true,
    'address-level3': true, 'address-level4': true, 'sex': true, 'url': true, 'photo': true, 'language': true, 'impp': true,
    'bday': true, 'bday-day': true, 'bday-month': true, 'bday-year': true, 'transaction-currency': true, 'transaction-amount': true,
    'tel-country-code': true, 'tel-area-code': true, 'tel-local': true, 'tel-extension': true };
  // Words in a field's label, name or id, in the order they are tried. The
  // first that a text holds gives the field's role.
  const WORDS = [
    [NONE, ['user', 'login', 'password', 'card', 'search', 'coupon', 'promo', 'captcha']],
    [EMAIL, ['email', 'e mail']],
    [PHONE, ['phone', 'mobile', 'telephone']],
    [POSTAL, ['zip', 'postal', 'postcode', 'post code']],
    [COUNTRY, ['country']],
    [REGION, ['state', 'province', 'region', 'county', 'prefecture']],
    [CITY, ['city', 'town', 'locality', 'suburb']],
    [ORGANIZATION, ['company', 'organization', 'organisation', 'business']],
    [LINE3, ['line 3', 'line3', 'address 3', 'address3']],
    [LINE2, ['line 2', 'line2', 'address 2', 'address2', 'apartment', 'suite']],
    [LINE1, ['line 1', 'line1', 'address 1', 'address1']],
    [STREET, ['street', 'address']],
    [GIVEN, ['first name', 'firstname', 'given']],
    [FAMILY, ['last name', 'lastname', 'surname', 'family']],
    [NAME, ['full name', 'fullname', 'your name', 'name']],
  ];
  const FIELDS = 'input,select,textarea';
  // At most this many fields, options and characters of a label are read.
  const MAX_FIELDS = 512, MAX_OPTIONS = 4096, MAX_LABEL = 200;

  const space = (c) => c === ' ' || c === '\t' || c === '\n' || c === '\r' || c === '\f';
  // A field's kind: a select, a text area, or a text-like input's type. Null for anything else.
  const kindOf = (field) => {
    if (is(InputCtor, field)) { const type = apply(inputType, field, []); return textLike(type) ? type : null; }
    if (is(SelectCtor, field)) return apply(selectType, field, []) === 'select-one' ? 'select' : null;
    if (is(AreaCtor, field)) return 'area';
    return null;
  };
  const formOf = (field, kind) => apply(kind === 'select' ? selectForm : kind === 'area' ? areaForm : inputForm, field, []);
  const labelsOf = (field, kind) => apply(kind === 'select' ? selectLabels : kind === 'area' ? areaLabels : inputLabels, field, []);
  // Whether an autocomplete token names a field of any kind.
  const namesField = (token) => TOKENS[token] !== undefined || CARD_TOKENS[token] !== undefined || OTHER[token] === true || apply(slice, token, [0, 3]) === 'cc-';
  // The role an autocomplete attribute gives a field, by `table`, from its
  // last token that names a field: NONE for a field of another kind, and
  // null when no token names one, as for "on", "off" or a hint.
  const tokenRole = (attribute, table) => {
    if (typeof attribute !== 'string') return null;
    const text = apply(toLowerCase, attribute, []);
    let end = text.length;
    for (let tokens = 0; end > 0 && tokens < 16; tokens++) {
      while (end > 0 && space(text[end - 1])) end--;
      let start = end;
      while (start > 0 && !space(text[start - 1])) start--;
      if (start === end) break;
      const token = apply(slice, text, [start, end]);
      const role = table[token];
      if (role !== undefined) return role;
      if (namesField(token)) return NONE;
      end = start;
    }
    return null;
  };
  // The role the words of `table` give a text, lowercased with every other
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
  // The role the words of `table` give a field's labels, then its
  // aria-label, its placeholder, its name and its id, the first that has any.
  const labelRole = (field, kind, table) => {
    const labels = labelsOf(field, kind);
    const count = labels === null || labels === undefined ? 0 : apply(nodeCount, labels, []);
    for (let i = 0; i < count && i < 4; i++) {
      const role = wordRole(apply(textContent, apply(nodeAt, labels, [i]), []), table);
      if (role !== null) return role;
    }
    const names = ['aria-label', 'placeholder', 'name', 'id'];
    for (let i = 0; i < names.length; i++) {
      const role = wordRole(apply(getAttribute, field, [names[i]]), table);
      if (role !== null) return role;
    }
    return null;
  };
  // A field's role: its autocomplete attribute first, then an email or
  // telephone input's type, then its labels and names.
  const roleOf = (field, kind) => {
    const token = tokenRole(apply(getAttribute, field, ['autocomplete']), TOKENS);
    if (token !== null) return token;
    if (kind === 'email') return EMAIL;
    if (kind === 'tel') return PHONE;
    return labelRole(field, kind, WORDS);
  };
  // The address fields in `scope`, in document order, each with its kind and
  // role: a form's fields; an element's fields outside any form; with null,
  // the document's fields outside any form. With `check`, only the fields it
  // passes. `has` counts each role.
  const addressFields = (scope, check) => {
    const list = scope === null ? apply(queryDocument, doc, [FIELDS]) : apply(querySelectorAll, scope, [FIELDS]);
    const loose = !is(FormCtor, scope);
    const count = apply(nodeCount, list, []);
    const found = { __proto__: null, items: { __proto__: null }, count: 0, has: { __proto__: null } };
    for (let i = 0; i < count && i < MAX_FIELDS; i++) {
      const field = apply(nodeAt, list, [i]);
      const kind = kindOf(field);
      if (kind === null || (loose && formOf(field, kind) !== null)) continue;
      const role = roleOf(field, kind);
      if (role === null || role === NONE || (check !== null && !check(field))) continue;
      found.items[found.count++] = { __proto__: null, field: field, kind: kind, role: role };
      found.has[role] = true;
    }
    return found;
  };
  // A street and a city or postal code: what makes fields an address.
  const shaped = (found) => (found.has[STREET] === true || found.has[LINE1] === true) && (found.has[CITY] === true || found.has[POSTAL] === true);
  // Whether a field has a role in an address.
  const addressRole = (field) => {
    const kind = kindOf(field);
    const role = kind === null ? null : roleOf(field, kind);
    return role !== null && role !== NONE;
  };
  // A field's form, or null outside any; undefined for anything but a field.
  const formAround = (node) => is(InputCtor, node) ? apply(inputForm, node, []) : is(SelectCtor, node) ? apply(selectForm, node, [])
    : is(AreaCtor, node) ? apply(areaForm, node, []) : undefined;
  // Where an address fill from a focused field may reach: its form; outside
  // any form, for a field with an address role, the nearest element below
  // the body and the root that holds another address field outside any
  // form. Null, reaching nothing, for any other field; undefined for
  // anything but a field.
  const groupOf = (field) => {
    const form = formAround(field);
    if (form !== null) return form;
    if (!addressRole(field)) return null;
    const body = apply(docBody, doc, []);
    let node = apply(parentNode, field, []);
    for (let depth = 0; depth < 64 && is(ElementCtor, node) && node !== body; depth++) {
      const up = apply(parentNode, node, []);
      if (!is(ElementCtor, up)) break;
      if (addressFields(node, null).count > 1) return node;
      node = up;
    }
    return null;
  };

  // -- Cards --

  // A field's role in a card form. A security code (CSC) is a card field,
  // so focusing one counts, but it is never filled.
  const CARD_NAME = 0, CARD_GIVEN = 1, CARD_FAMILY = 2, CARD_NUMBER = 3, CARD_EXP = 4, CARD_MONTH = 5, CARD_YEAR = 6, CSC = 7;
  // The values a card fill brings, in order: the cardholder's name, its
  // given and family parts, the number's digits, the expiry month as two
  // digits and the year as four. The browser splits the name.
  const CARD_VALUES = 6;
  const CARD_TOKENS = { __proto__: null, 'cc-name': CARD_NAME, 'cc-given-name': CARD_GIVEN, 'cc-family-name': CARD_FAMILY,
    'cc-number': CARD_NUMBER, 'cc-exp': CARD_EXP, 'cc-exp-month': CARD_MONTH, 'cc-exp-year': CARD_YEAR, 'cc-csc': CSC };
  // Words that give a field without a cc- token its role, tried in order,
  // as `WORDS` are for an address: a security code first, so no code is
  // ever taken for the number or the expiry.
  const CARD_WORDS = [
    [CSC, ['cvc', 'cvv', 'csc', 'cvn', 'security code', 'card code', 'verification']],
    [NONE, ['zip', 'postal', 'coupon', 'promo', 'gift', 'email', 'phone', 'birth', 'dob']],
    [CARD_MONTH, ['exp month', 'expiry month', 'expiration month', 'expmonth', 'exp mm', 'month']],
    [CARD_YEAR, ['exp year', 'expiry year', 'expiration year', 'expyear', 'exp yy', 'year']],
    [CARD_EXP, ['expiry', 'expiration', 'exp date', 'expdate', 'expires', 'cc exp', 'ccexp', 'valid thru', 'valid through', 'mm yy']],
    [CARD_NUMBER, ['card number', 'cardnumber', 'card no', 'ccnumber', 'cc number', 'credit card', 'debit card', 'number on card']],
    [CARD_NAME, ['name on card', 'nameoncard', 'cardholder', 'card holder', 'holder name', 'ccname', 'cc name']],
  ];
  // English month names, by number, for a select whose options name the month.
  const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];

  // A card field's kind: a select, or a text-like, number or month input's type. Null for anything else.
  const cardKind = (field) => {
    if (is(InputCtor, field)) { const type = apply(inputType, field, []); return textLike(type) || type === 'number' || type === 'month' ? type : null; }
    if (is(SelectCtor, field)) return apply(selectType, field, []) === 'select-one' ? 'select' : null;
    return null;
  };
  // A field's card role: its cc- token first, then its labels and names.
  // An email field is never a card field.
  const cardRoleOf = (field, kind) => {
    const token = tokenRole(apply(getAttribute, field, ['autocomplete']), CARD_TOKENS);
    if (token !== null) return token;
    if (kind === 'email') return NONE;
    return labelRole(field, kind, CARD_WORDS);
  };
  // The card fields in `scope`, in document order, read as `addressFields`
  // reads an address's: a form's fields, or an element's fields outside any
  // form. A security code is left out of the items but counted in `has` and
  // in `fields`, every card field seen. With `check`, only the fields it
  // passes.
  const cardFields = (scope, check) => {
    const list = apply(querySelectorAll, scope, [FIELDS]);
    const loose = !is(FormCtor, scope);
    const count = apply(nodeCount, list, []);
    const found = { __proto__: null, items: { __proto__: null }, count: 0, fields: 0, has: { __proto__: null } };
    for (let i = 0; i < count && i < MAX_FIELDS; i++) {
      const field = apply(nodeAt, list, [i]);
      const kind = cardKind(field);
      if (kind === null || (loose && formOf(field, kind) !== null)) continue;
      const role = cardRoleOf(field, kind);
      if (role === null || role === NONE || (check !== null && !check(field))) continue;
      found.fields++;
      found.has[role] = true;
      if (role !== CSC) found.items[found.count++] = { __proto__: null, field: field, kind: kind, role: role };
    }
    return found;
  };
  // A field's card role, a security code's included; null for any other field.
  const cardRole = (field) => {
    const kind = cardKind(field);
    const role = kind === null ? null : cardRoleOf(field, kind);
    return role === null || role === NONE ? null : role;
  };
  // Where a card fill from a focused field may reach, as `groupOf` says for
  // an address: its form; outside any form, for a card field, the nearest
  // element below the body and the root that holds another card field
  // outside any form, else that field alone, as in a payment processor's
  // frame that holds only the number. Null, reaching nothing, for any other
  // field; undefined for anything but a field.
  const cardGroupOf = (field) => {
    const form = formAround(field);
    if (form !== null) return form;
    if (cardRole(field) === null) return null;
    const body = apply(docBody, doc, []);
    let node = apply(parentNode, field, []);
    for (let depth = 0; depth < 64 && is(ElementCtor, node) && node !== body; depth++) {
      const up = apply(parentNode, node, []);
      if (!is(ElementCtor, up)) break;
      if (cardFields(node, null).fields > 1) return node;
      node = up;
    }
    return field;
  };
  // The card fields to fill: those `check` passes where the focused field
  // may reach (see `cardGroupOf`), and none at all without a focused field,
  // in any frame: a page-wide search could reach a form whose hiding can't
  // be read, as in a closed shadow root. Never a security code.
  const cardTarget = (check) => {
    const active = apply(activeElement, doc, []);
    const scope = cardGroupOf(active);
    if (scope === undefined || scope === null) return null;
    if (scope !== active) {
      const found = cardFields(scope, check);
      return found.count > 0 ? found : null;
    }
    const role = cardRole(active);
    if (role === CSC || !check(active)) return null;
    const alone = { __proto__: null, items: { __proto__: null }, count: 1, fields: 1, has: { __proto__: null } };
    alone.items[0] = { __proto__: null, field: active, kind: cardKind(active), role: role };
    alone.has[role] = true;
    return alone;
  };
  const lowerAttribute = (field, name) => {
    const text = apply(getAttribute, field, [name]);
    return typeof text === 'string' ? apply(toLowerCase, text, []) : '';
  };
  // What a combined expiry field takes: MM/YY, or MM/YYYY when its
  // placeholder asks for four digits of year, or its length is that of
  // MMYYYY or MM/YYYY; no separator when its length is that of MMYY or
  // MMYYYY, and " / " when its placeholder has one. A month input takes YYYY-MM.
  const expiryText = (item, month, year) => {
    if (item.kind === 'month') return year + '-' + month;
    const hint = lowerAttribute(item.field, 'placeholder');
    const max = item.kind === 'select' ? -1 : apply(maxLength, item.field, []);
    const four = apply(includes, hint, ['yyyy']) || apply(includes, hint, ['aaaa']) || (!apply(includes, hint, ['yy']) && (max === 6 || max === 7));
    const separator = apply(includes, hint, [' / ']) ? ' / ' : max === 4 || max === 6 ? '' : '/';
    return month + separator + (four ? year : apply(slice, year, [2, 4]));
  };
  // Choose the first of `texts` some option of `select` has.
  const chooseAny = (select, texts) => {
    for (let i = 0; i < texts.length; i++) {
      if (texts[i] !== '' && choose(select, texts[i])) return true;
    }
    return false;
  };
  // The texts a card field may take, best first; null for none.
  const cardTexts = (item, values) => {
    const month = values[4], year = values[5];
    let texts;
    switch (item.role) {
      case CARD_NAME: texts = [values[0]]; break;
      case CARD_GIVEN: texts = [values[1]]; break;
      case CARD_FAMILY: texts = [values[2]]; break;
      case CARD_NUMBER: texts = [values[3]]; break;
      case CARD_EXP: texts = [expiryText(item, month, year), month + '/' + apply(slice, year, [2, 4]), month + '/' + year]; break;
      case CARD_MONTH: {
        const name = MONTHS[+month - 1];
        texts = [month, apply(slice, month, [0, 1]) === '0' ? apply(slice, month, [1, 2]) : month, name === undefined ? '' : name, name === undefined ? '' : apply(slice, name, [0, 3])];
        break;
      }
      case CARD_YEAR: {
        const two = apply(slice, year, [2, 4]);
        if (item.kind === 'select') { texts = [year, two]; break; }
        const hint = lowerAttribute(item.field, 'placeholder');
        texts = [apply(maxLength, item.field, []) === 2 || (apply(includes, hint, ['yy']) && !apply(includes, hint, ['yyyy'])) ? two : year];
        break;
      }
      default: return null;
    }
    return typeof texts[0] !== 'string' || texts[0] === '' ? null : texts;
  };
  // Whether a card field would take a value: a select only with an option for one of its texts.
  const takesCard = (item, values) => {
    const texts = cardTexts(item, values);
    if (texts === null || item.kind !== 'select') return texts !== null;
    for (let i = 0; i < texts.length; i++) {
      if (texts[i] !== '' && optionOf(item.field, texts[i]) >= 0) return true;
    }
    return false;
  };
  // Fill one card field. False when it took nothing.
  const putCard = (item, values) => {
    const texts = cardTexts(item, values);
    if (texts === null) return false;
    if (item.kind === 'select') return chooseAny(item.field, texts);
    put(item.field, texts[0]);
    return true;
  };

  // Where an address or card fill from the focused field would reach, as
  // the last note said: an address form or group (see `groupOf`) that
  // holds an address, a card form or group (see `cardGroupOf`) that holds
  // a card field, or null for none. Once an address or card field had
  // focus, each change is noted, content-free: focus moving to another form
  // or group, to anything that reaches none, and back. The renderer numbers
  // the notes, so a fill the browser froze on an earlier one is void
  // (C11-1). Focus coming back from another frame sends nothing, since
  // nothing here saw it go, and neither does the same field focused again,
  // as when the window regains focus.
  let focusedForm = null, focusedAddress = null, focusedCard = null;
  let addressNoted = false, cardNoted = false;
  const onFocus = function (event) {
    try {
      const field = apply(target, event, []);
      if (notingLogins && is(InputCtor, field)) {
        const form = apply(inputForm, field, []);
        if (form !== null && form !== focusedForm && fields(form, false) !== null) {
          focusedForm = form;
          focused();
        }
      }
      if (notingAddresses) {
        const role = addressRole(field);
        const scope = role || addressNoted ? groupOf(field) : null;
        // Answers for each field a submit may read, kept as they change.
        if (capturingAddresses && role && scope !== null) {
          const group = addressFields(scope, null);
          for (let i = 0; i < group.count; i++) watch(group.items[i].field);
        }
        const reach = scope !== undefined && scope !== null && shaped(addressFields(scope, null)) ? scope : null;
        if (reach !== focusedAddress) {
          focusedAddress = reach;
          addressNoted = true;
          addressFocused();
        }
      }
      if (notingCards) {
        const scope = cardNoted || cardRole(field) !== null ? cardGroupOf(field) : null;
        const reach = scope === undefined || scope === null || (scope !== field && cardFields(scope, null).fields === 0) ? null : scope;
        if (reach !== focusedCard) {
          focusedCard = reach;
          cardNoted = true;
          cardFocused();
        }
      }
    } catch (_) {}
  };

  if (capturing) {
    try { apply(listen, win, ['submit', onSubmit, true]); } catch (_) {}
    try { apply(observe, new Observer(onEntries), [{ type: 'resource' }]); } catch (_) {}
  }
  try { apply(listen, win, ['focusin', onFocus, true]); } catch (_) {}

  // Null-prototype options, so the constructor reads no getter a page put on Object.prototype.
  const fire = (input, type) => apply(dispatch, input, [new EventCtor(type, { __proto__: null, bubbles: true })]);
  const put = (input, text) => {
    apply(is(AreaCtor, input) ? areaValue.set : setValue, input, [text]);
    fire(input, 'input');
    fire(input, 'change');
  };
  // The fields to fill: with a field focused, those of its form the user can
  // see, and none at all if its form has none or it has no form; else, only
  // in the main frame, those of the one form that has them, and none if two
  // do: one may be hidden in a way that can't be read, as in a closed shadow
  // root. Never a field found hidden.
  const fillFields = (main) => {
    const form = formAround(apply(activeElement, doc, []));
    if (form !== undefined) return form === null || fields(form, false) === null ? null : fields(form, true);
    if (main !== true) return null;
    const all = apply(forms, doc, []);
    const count = apply(formCount, all, []);
    let only = null;
    for (let i = 0; i < count; i++) {
      const found = fields(apply(formAt, all, [i]), true);
      if (found === null) continue;
      if (only !== null) return null;
      only = found;
    }
    return only;
  };

  // The index of the first option whose value or text is `text`, ignoring
  // case and the spaces around it; -1 when none is.
  const optionOf = (select, text) => {
    const wanted = apply(toLowerCase, apply(trim, text, []), []);
    const list = apply(options, select, []);
    const count = apply(formCount, list, []);
    for (let i = 0; i < count && i < MAX_OPTIONS; i++) {
      const option = apply(formAt, list, [i]);
      const value = apply(toLowerCase, apply(trim, apply(optionValue, option, []), []), []);
      if ((value !== '' && value === wanted) || apply(toLowerCase, apply(trim, apply(optionText, option, []), []), []) === wanted) return i;
    }
    return -1;
  };
  // Choose the first option whose value or text is `text`. False when none is.
  const choose = (select, text) => {
    const index = optionOf(select, text);
    if (index < 0) return false;
    apply(selectedIndex.set, select, [index]);
    fire(select, 'input');
    fire(select, 'change');
    return true;
  };
  // The address fields to fill: those `check` passes where the focused
  // field may reach (see `groupOf`), and none at all if they aren't an
  // address. Never without a focused field, in any frame: a page-wide search
  // could reach a form whose hiding can't be read, as in a closed shadow
  // root.
  const addressTarget = (check) => {
    const scope = groupOf(apply(activeElement, doc, []));
    const found = scope === undefined || scope === null ? null : addressFields(scope, check);
    return found !== null && shaped(found) ? found : null;
  };
  // Which of the fill's values a field takes. A first street line takes the
  // whole street when there is no second-line field, and a second line takes
  // the rest when there is no third.
  const valueFor = (item, found) => {
    switch (item.role) {
      case NAME: return 0;
      case GIVEN: return 1;
      case FAMILY: return 2;
      case ORGANIZATION: return 3;
      case STREET: return item.kind === 'area' ? 4 : 5;
      case LINE1: return found.has[LINE2] === true ? 6 : 5;
      case LINE2: return found.has[LINE3] === true ? 7 : 8;
      case LINE3: return 9;
      default: return item.role - CITY + 10;
    }
  };

  const login = function (username, password, main) {
    try {
      const found = fillFields(main);
      if (found === null) return false;
      if (found.user !== null && username !== '') put(found.user, username);
      put(found.first, password);
      return true;
    } catch (_) {
      return false;
    }
  };

  // -- What the engine says the user can see --

  // Script can't read every way a page hides a field: the wrappers of a
  // closed shadow root a field is slotted into are out of its reach. The
  // engine's own answer, IntersectionObserver v2's `isVisible`, is computed
  // on what layout draws, closed shadow roots and the frame's place in the
  // page that embeds it included. As specified, it is true only when the
  // field and everything it is drawn in are fully opaque, unfiltered and at
  // most moved or evenly scaled, and nothing is drawn over any part of it.
  // An address or card field is filled, and an address field read at a
  // submit, only when that answer says so, as well as the rule above;
  // without the API there is no answer, so none is. Logins don't wait for it.
  // The latest answer for each field observed: whether the user can see it,
  // and when the engine computed it, on the frame's performance clock.
  const answers = new Weak();
  // The address or card fill waiting for the engine's answers, if any; a
  // newer fill of the same kind replaces it, and it then reports nothing.
  const waiting = { __proto__: null, address: null, card: null };
  let observer = null;
  // Whether the engine said, at `since` or later, that the user can see `field`.
  const seen = (field, since) => {
    const said = apply(weakGet, answers, [field]);
    return said !== undefined && said.time >= since && said.visible === true;
  };
  const answered = (field, since) => {
    const said = apply(weakGet, answers, [field]);
    return said !== undefined && said.time >= since;
  };
  // Keep an answer for `field`, updated whenever it changes.
  const watch = (field) => {
    if (observer === null) return;
    try { apply(ioObserve, observer, [field]); } catch (_) {}
  };
  // Ask afresh: a field observed anew is answered at the engine's next
  // update, even when its answer hasn't changed.
  const ask = (field) => {
    if (observer === null) return;
    try {
      apply(ioUnobserve, observer, [field]);
      apply(ioObserve, observer, [field]);
    } catch (_) {}
  };
  // Tell the renderer what a fill did (see addressFilled): numbers only.
  const report = (kind, outcome, filled, hidden) => {
    if (kind === 'card') cardFilled(outcome, '' + filled, '' + hidden);
    else addressFilled(outcome, '' + filled, '' + hidden);
  };
  // Finish `asked` (see `gated`): with every answer the fill asked for, fill
  // each field the engine calls clearly visible and report how many it left
  // empty, or fill nothing when the focused field isn't one or focus moved;
  // with an answer missing, fill nothing.
  const finish = (asked) => {
    waiting[asked.kind] = null;
    const kind = asked.kind, since = asked.since, target = asked.target, wants = asked.wants, fill = asked.fill;
    for (let i = 0; i < asked.count; i++) {
      if (!answered(asked.fields[i], since)) return report(kind, 'unconfirmed', 0, 0);
    }
    const found = apply(activeElement, doc, []) === asked.active ? target(fillable) : null;
    if (found === null) return report(kind, 'done', 0, 0);
    if (!seen(asked.active, since)) return report(kind, 'focus-hidden', 0, 0);
    let hidden = 0;
    for (let i = 0; i < found.count; i++) {
      if (!seen(found.items[i].field, since) && wants(found.items[i], found)) hidden++;
    }
    // A field the fill didn't ask about, as one added meanwhile, has no answer, and is left out.
    const shown = target((field) => fillable(field) && seen(field, since));
    return report(kind, 'done', shown === null ? 0 : fill(shown), hidden);
  };
  // Finish the `kind` fill waiting, if every field it asked about has an answer.
  const settle = (kind) => {
    const asked = waiting[kind];
    if (asked === null) return;
    for (let i = 0; i < asked.count; i++) {
      if (!answered(asked.fields[i], asked.since)) return;
    }
    finish(asked);
  };
  const onAnswers = function (entries) {
    try {
      for (let i = 0; i < entries.length; i++) {
        const entry = entries[i];
        apply(weakSet, answers, [apply(entryTarget, entry, []), { __proto__: null, visible: apply(entryVisible, entry, []) === true, time: apply(entryTime, entry, []) }]);
      }
      settle('address');
      settle('card');
    } catch (_) {}
  };
  if ((notingAddresses || notingCards) && IO !== undefined && ioObserve !== undefined && ioUnobserve !== undefined && entryTarget !== undefined && entryVisible !== undefined
    && entryTime !== undefined && later !== undefined) {
    try { observer = new IO(onAnswers, { __proto__: null, trackVisibility: true, delay: DELAY_MS, threshold: 0 }); } catch (_) {}
  }
  // An address or card fill, once the engine has answered for the focused
  // field and every field the fill may reach: `target(check)` gives the
  // fields `check` passes where the focused field may reach, or null;
  // `wants(item, found)` whether a field would take a value; `fill(found)`
  // fills them and gives how many took one. Each field asked about is
  // asked afresh, so no answer from before the fill counts. A field the
  // engine doesn't call clearly visible is left empty on its own, and
  // counted; a focused field it doesn't, or a focus that moved meanwhile,
  // fills nothing; and so does a missing answer after WAIT_MS, or a browser
  // that can't answer. False at once, filling nothing, when no field would
  // take a value anyway; otherwise true, and the outcome is reported.
  const gated = (kind, target, wants, fill) => {
    const found = target(fillable);
    if (found === null) return false;
    let any = false;
    for (let i = 0; i < found.count && !any; i++) any = wants(found.items[i], found);
    if (!any) return false;
    if (observer === null) {
      report(kind, 'unconfirmed', 0, 0);
      return true;
    }
    const active = apply(activeElement, doc, []);
    const asked = { __proto__: null, kind: kind, active: active, since: apply(now, perf, []), fields: { __proto__: null, 0: active }, count: 1, target: target, wants: wants, fill: fill };
    for (let i = 0; i < found.count; i++) asked.fields[asked.count++] = found.items[i].field;
    for (let i = 0; i < asked.count; i++) ask(asked.fields[i]);
    waiting[kind] = asked;
    apply(later, win, [function () {
      try { if (waiting[kind] === asked) finish(asked); } catch (_) {}
    }, WAIT_MS]);
    return true;
  };

  // `main` is taken for the renderer's one calling convention; an address
  // fill needs a focused field in every frame.
  const address = function (main, ...values) {
    try {
      if (values.length !== ADDRESS_VALUES || !reportingAddresses) return false;
      const text = (item, found) => values[valueFor(item, found)];
      const wants = (item, found) => {
        const value = text(item, found);
        return typeof value === 'string' && value !== '' && (item.kind !== 'select' || optionOf(item.field, value) >= 0);
      };
      const fill = (found) => {
        let filled = 0;
        for (let i = 0; i < found.count; i++) {
          const item = found.items[i];
          const value = text(item, found);
          if (typeof value !== 'string' || value === '') continue;
          if (item.kind === 'select') {
            if (choose(item.field, value)) filled++;
          } else {
            put(item.field, value);
            filled++;
          }
        }
        return filled;
      };
      return gated('address', addressTarget, wants, fill);
    } catch (_) {
      return false;
    }
  };
  // `main` is taken for the renderer's one calling convention; a card
  // fill needs a focused field in every frame.
  const card = function (main, ...values) {
    try {
      // A document that can't take a card never got the card note.
      if (!notingCards || !reportingCards || values.length !== CARD_VALUES) return false;
      const fill = (found) => {
        let filled = 0;
        for (let i = 0; i < found.count; i++) {
          if (putCard(found.items[i], values)) filled++;
        }
        return filled;
      };
      return gated('card', cardTarget, (item) => takesCard(item, values), fill);
    } catch (_) {
      return false;
    }
  };
  return { __proto__: null, login: login, address: address, card: card };
})
