//! `login.js` run in a real JavaScript engine: the system's JavaScriptCore,
//! which is part of macOS, so nothing is installed and no crate is added.
//! CEF's V8 can't run in unit tests. JavaScriptCore parses the same
//! ECMAScript, so a script it rejects, V8 rejects too. The page is a stub of
//! the globals `login.js` reads at load and when it fills: classes with the
//! browser's names, prototypes and getters, and a hit test over the boxes a
//! test gives its elements. It stands in for Chromium's layout, which these
//! tests can't run.

use super::SCRIPT;
use std::ffi::{CString, c_char, c_void};

type Context = *mut c_void;
type JsString = *mut c_void;
type Value = *const c_void;

#[link(name = "JavaScriptCore", kind = "framework")]
unsafe extern "C" {
    fn JSGlobalContextCreate(global_class: *mut c_void) -> Context;
    fn JSGlobalContextRelease(context: Context);
    fn JSStringCreateWithUTF8CString(text: *const c_char) -> JsString;
    fn JSStringRelease(text: JsString);
    fn JSStringGetMaximumUTF8CStringSize(text: JsString) -> usize;
    fn JSStringGetUTF8CString(text: JsString, buffer: *mut c_char, size: usize) -> usize;
    fn JSCheckScriptSyntax(context: Context, script: JsString, url: JsString, line: i32, exception: *mut Value) -> bool;
    fn JSEvaluateScript(context: Context, script: JsString, this: *mut c_void, url: JsString, line: i32, exception: *mut Value) -> Value;
    fn JSValueToStringCopy(context: Context, value: Value, exception: *mut Value) -> JsString;
}

/// One JavaScriptCore global context, released on drop.
pub(crate) struct Engine(Context);

impl Engine {
    pub(crate) fn new() -> Engine {
        // SAFETY: a null class asks for a default global object.
        Engine(unsafe { JSGlobalContextCreate(std::ptr::null_mut()) })
    }

    fn string(text: &str) -> JsString {
        let text = CString::new(text).expect("no NUL in a script");
        // SAFETY: a valid NUL-terminated string; the caller releases the result.
        unsafe { JSStringCreateWithUTF8CString(text.as_ptr()) }
    }

    fn text(&self, value: Value) -> String {
        if value.is_null() { return "null".into(); }
        // SAFETY: a value of this context; the copy is released here.
        unsafe {
            let copy = JSValueToStringCopy(self.0, value, std::ptr::null_mut());
            if copy.is_null() { return "(unprintable)".into(); }
            let size = JSStringGetMaximumUTF8CStringSize(copy);
            let mut buffer = vec![0u8; size.max(1)];
            let written = JSStringGetUTF8CString(copy, buffer.as_mut_ptr().cast(), size);
            JSStringRelease(copy);
            String::from_utf8_lossy(&buffer[..written.saturating_sub(1)]).into_owned()
        }
    }

    /// Whether `script` parses, with the engine's error if it doesn't.
    fn check(&self, script: &str, url: &str) -> Result<(), String> {
        let (script, url) = (Engine::string(script), Engine::string(url));
        let mut exception: Value = std::ptr::null();
        // SAFETY: strings and context are live; both strings are released.
        let ok = unsafe { JSCheckScriptSyntax(self.0, script, url, 1, &mut exception) };
        unsafe { JSStringRelease(script); JSStringRelease(url); }
        if ok { Ok(()) } else { Err(self.text(exception)) }
    }

    /// Run `script`, giving its completion value as a string, or the exception it threw.
    pub(crate) fn run(&self, script: &str, url: &str) -> Result<String, String> {
        let (script, url) = (Engine::string(script), Engine::string(url));
        let mut exception: Value = std::ptr::null();
        // SAFETY: as in `check`; `this` null is the global object.
        let value = unsafe { JSEvaluateScript(self.0, script, std::ptr::null_mut(), url, 1, &mut exception) };
        unsafe { JSStringRelease(script); JSStringRelease(url); }
        if exception.is_null() { Ok(self.text(value)) } else { Err(self.text(exception)) }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: created by JSGlobalContextCreate and released once.
        unsafe { JSGlobalContextRelease(self.0) }
    }
}

/// The page: the browser's classes, by name, with what `login.js` reads.
/// Elements are made with `el(tag, props, children)`; `props.rect` is the
/// element's box, `props.style` its computed style, and a `clip` style or
/// `overflow: hidden` clips its descendants to `props.clipTo` (empty by
/// default) for the hit test, as layout would. `props.paints(x, y)`, if
/// given, says which points of its box a transform leaves it drawn at, for
/// the hit test. `props.labels` are the texts of a field's labels, and
/// `props.text` an option's or label's text.
const PAGE: &str = r#"
(function (g) {
  'use strict';
  g.__log = [];
  g.__listeners = [];
  g.__observed = [];
  g.__activation = { active: true };
  class EventTarget {
    addEventListener(type, listener, capture) { g.__listeners.push({ target: this, type: type, listener: listener, capture: capture }); }
    dispatchEvent(event) { g.__log.push(event.type + ':' + (this.__name || '')); return true; }
  }
  class Node extends EventTarget {
    constructor() { super(); this.__parent = null; this.__children = []; }
    get parentElement() { return this.__parent instanceof Element ? this.__parent : null; }
    get parentNode() { return this.__parent; }
    get textContent() { return this.__text || ''; }
    contains(other) { for (let node = other; node !== null; node = node.__parent) { if (node === this) return true; } return false; }
  }
  // Tag names only, or several separated by commas.
  const select = (root, selector) => { const tags = selector.split(','); const found = []; walk(root, (node) => { if (node !== root && tags.includes(node.__tag)) found.push(node); }); return new NodeList(found); };
  const formOf = (field) => { for (let node = field.__parent; node !== null; node = node.__parent) { if (node instanceof HTMLFormElement) return node; } return null; };
  class Element extends Node {
    querySelectorAll(selector) { return select(this, selector); }
    getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.__attrs, name) ? this.__attrs[name] : null; }
    checkVisibility(options) {
      for (let node = this; node instanceof Element; node = node.__parent) {
        const style = node.__style;
        if (style.display === 'none') return false;
        if (options && options.visibilityProperty && style.visibility === 'hidden') return false;
        if (options && options.opacityProperty && style.opacity === '0') return false;
      }
      return true;
    }
    getBoundingClientRect() { return new DOMRectReadOnly(this.__rect); }
    get assignedSlot() { return this.__slot || null; }
  }
  // A shadow root: a light-DOM element slotted into it is drawn inside its
  // wrappers, which the light-DOM parent chain never meets.
  class ShadowRoot extends Node { get host() { return this.__host; } }
  class HTMLElement extends Element {}
  class HTMLFormElement extends HTMLElement {}
  class HTMLInputElement extends HTMLElement {
    get type() { return this.__attrs.type || 'text'; }
    get form() { return formOf(this); }
    get disabled() { return this.__attrs.disabled === true; }
    get readOnly() { return this.__attrs.readOnly === true; }
    get maxLength() { return this.__attrs.maxLength === undefined ? -1 : this.__attrs.maxLength; }
    get labels() { return new NodeList(this.__labels); }
    get value() { return this.__value; }
    set value(text) { this.__value = text; g.__log.push('value:' + (this.__name || '')); }
  }
  class HTMLTextAreaElement extends HTMLElement {
    get form() { return formOf(this); }
    get disabled() { return this.__attrs.disabled === true; }
    get readOnly() { return this.__attrs.readOnly === true; }
    get labels() { return new NodeList(this.__labels); }
    get value() { return this.__value; }
    set value(text) { this.__value = text; g.__log.push('value:' + (this.__name || '')); }
  }
  class HTMLOptionElement extends HTMLElement {
    get text() { return this.__text || ''; }
    get value() { return this.__attrs.value === undefined ? this.text : this.__attrs.value; }
  }
  class HTMLSelectElement extends HTMLElement {
    get type() { return this.__attrs.multiple ? 'select-multiple' : 'select-one'; }
    get form() { return formOf(this); }
    get disabled() { return this.__attrs.disabled === true; }
    get labels() { return new NodeList(this.__labels); }
    get options() { return new HTMLCollection(this.__children.filter((child) => child instanceof HTMLOptionElement)); }
    get selectedIndex() { return this.__selected === undefined ? (this.__children.length > 0 ? 0 : -1) : this.__selected; }
    set selectedIndex(i) { this.__selected = i; g.__log.push('value:' + (this.__name || '')); }
  }
  class NodeList { constructor(items) { this.__items = items; } item(i) { return this.__items[i] || null; } get length() { return this.__items.length; } }
  class HTMLCollection { constructor(items) { this.__items = items; } item(i) { return this.__items[i] || null; } get length() { return this.__items.length; } }
  class DOMRectReadOnly {
    constructor(r) { this.__r = r; }
    get left() { return this.__r.x; } get top() { return this.__r.y; } get width() { return this.__r.w; } get height() { return this.__r.h; }
    get right() { return this.__r.x + this.__r.w; } get bottom() { return this.__r.y + this.__r.h; }
  }
  class CSSStyleDeclaration {
    constructor(style) { this.__s = style; }
    getPropertyValue(name) { const value = this.__s[name]; return value === undefined ? ({ opacity: '1', clip: 'auto', 'clip-path': 'none' })[name] || '' : value; }
  }
  const walk = (node, visit) => { visit(node); for (const child of node.__children) walk(child, visit); };
  const inside = (r, x, y) => x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
  const clips = (style) => style.overflow === 'hidden' || (style.clip !== undefined && style.clip !== 'auto') || (style['clip-path'] !== undefined && style['clip-path'] !== 'none');
  class Document extends Node {
    querySelectorAll(selector) { return select(this, selector); }
    get body() { return this.__body; }
    get activeElement() { return g.__active || this.body; }
    get forms() { const found = []; walk(this, (node) => { if (node instanceof HTMLFormElement) found.push(node); }); return new HTMLCollection(found); }
    elementFromPoint(x, y) {
      let hit = null;
      walk(this.body, (node) => {
        if (!(node instanceof Element) || !node.__rect || !node.checkVisibility({ visibilityProperty: true })) return;
        if (!inside(node.__rect, x, y) || (node.__paints !== undefined && !node.__paints(x, y))) return;
        for (let up = node.__parent; up instanceof Element; up = up.__parent) {
          if (clips(up.__style) && !inside(up.__clipTo || { x: 0, y: 0, w: 0, h: 0 }, x, y)) return;
        }
        hit = node;
      });
      return hit;
    }
  }
  class Event { constructor(type, init) { this.type = type; this.__init = init; } get target() { return this.__target; } }
  class Navigator { get userActivation() { return g.__activation; } }
  class UserActivation { get isActive() { return this.active; } }
  Object.setPrototypeOf(g.__activation, UserActivation.prototype);
  class Performance { now() { return g.__now !== undefined ? g.__now : g.__clock; } }
  class PerformanceObserver { constructor(callback) { this.__callback = callback; } observe(options) { g.__observed.push(options.type); } }
  class PerformanceObserverEntryList { getEntries() { return []; } }
  class PerformanceEntry { get startTime() { return 0; } }
  class PerformanceResourceTiming extends PerformanceEntry { get initiatorType() { return 'fetch'; } }
  // IntersectionObserver v2, answering as the engine is documented to, from
  // the boxes and styles layout draws: along the chain a field is drawn in,
  // through every slot (`__drawnIn`, a closed shadow root's too, which
  // script outside it can't read) and shadow host. Visible means its box is
  // in the viewport; every element of that chain is rendered at opacity 1,
  // with no filter and no transform but a translation or a proportional
  // scale; nothing rendered after it, but its own contents, is over any part
  // of its box; and its frame is neither faded nor covered in its parent
  // (`__frameClear`). Each target observed is answered at the next frame,
  // then whenever its answer changes, and never while the page isn't
  // rendering (`__rendering` false). This is a model, not Chromium.
  class IntersectionObserverEntry {
    get target() { return this.__target; }
    get isVisible() { return this.__visible; }
    get time() { return this.__time; }
  }
  class IntersectionObserver {
    constructor(callback, options) {
      // Chromium throws without a delay of at least 100 ms.
      if (options.trackVisibility === true && !(options.delay >= 100)) throw new TypeError('trackVisibility needs a delay of at least 100');
      this.__callback = callback;
      this.__track = options.trackVisibility === true;
      this.__targets = new Map();
      g.__observers.push(this);
    }
    observe(target) { if (!this.__targets.has(target)) this.__targets.set(target, undefined); }
    unobserve(target) { this.__targets.delete(target); }
  }
  g.__observers = [];
  g.__frameClear = true;
  g.__rendering = true;
  const drawnParent = (node) => { const up = node.__drawnIn || node.__slot || node.__parent; return up instanceof ShadowRoot ? up.__host : up; };
  const plainTransform = (t) => t === undefined || t === 'none' || /^translate/.test(t) || /^scale\([^,]*\)$/.test(t);
  const overlaps = (a, b) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
  g.__engineVisible = function (target) {
    const box = target.__rect;
    if (!g.__frameClear || !box || !(box.w > 0 && box.h > 0) || !overlaps(box, { x: 0, y: 0, w: g.innerWidth, h: g.innerHeight })) return false;
    let node = target;
    for (; node instanceof Element; node = drawnParent(node)) {
      const style = node.__style;
      if (style.display === 'none' || style.visibility === 'hidden') return false;
      if ((style.opacity !== undefined && +style.opacity < 1) || (style.filter !== undefined && style.filter !== 'none') || !plainTransform(style.transform)) return false;
    }
    if (node !== g.document) return false;
    let after = false, covered = false;
    walk(g.document.body, (other) => {
      if (other === target) { after = true; return; }
      if (!after || covered || !(other instanceof Element) || !other.__rect || target.contains(other)) return;
      // A fully transparent element hides nothing.
      if (other.checkVisibility({ visibilityProperty: true, opacityProperty: true }) && overlaps(other.__rect, box)) covered = true;
    });
    return !covered;
  };
  g.__frame = function () {
    if (!g.__rendering) return;
    for (const observer of g.__observers) {
      const entries = [];
      for (const [target, last] of observer.__targets) {
        const visible = observer.__track && g.__engineVisible(target);
        if (last === visible) continue;
        observer.__targets.set(target, visible);
        const entry = new IntersectionObserverEntry();
        entry.__target = target; entry.__visible = visible; entry.__time = g.__clock;
        entries.push(entry);
      }
      if (entries.length > 0) observer.__callback(entries, observer);
    }
  };
  // Timers, and the clock: `__advance(ms)` moves it in 16 ms frames, at each
  // of which the observers answer, then the timers due run.
  g.__clock = 0;
  g.__timers = [];
  g.setTimeout = function (callback, ms) { g.__timers.push({ callback: callback, at: g.__clock + (+ms || 0), done: false }); return g.__timers.length; };
  g.__advance = function (ms) {
    for (const end = g.__clock + ms; g.__clock + 16 <= end;) {
      g.__clock += 16;
      g.__frame();
      for (const timer of g.__timers.filter((t) => !t.done && t.at <= g.__clock)) { timer.done = true; timer.callback(); }
    }
  };
  Object.assign(g, { EventTarget, Node, Element, HTMLElement, HTMLFormElement, HTMLInputElement, HTMLTextAreaElement, HTMLOptionElement, HTMLSelectElement, NodeList, HTMLCollection, DOMRectReadOnly,
    CSSStyleDeclaration, Document, ShadowRoot, Event, Navigator, UserActivation, Performance, PerformanceObserver, PerformanceObserverEntryList,
    PerformanceEntry, PerformanceResourceTiming, IntersectionObserver, IntersectionObserverEntry });
  g.window = g;
  g.navigator = new Navigator();
  g.performance = new Performance();
  g.document = new Document();
  g.document.__tag = '#document';
  g.document.__style = {};
  // Window's attributes are the global's own, as in browsers.
  Object.defineProperty(g, 'innerWidth', { get() { return 1280; }, configurable: true });
  Object.defineProperty(g, 'innerHeight', { get() { return 800; }, configurable: true });
  g.getComputedStyle = function (element) { return new CSSStyleDeclaration(element.__style); };
  const make = { form: HTMLFormElement, input: HTMLInputElement, textarea: HTMLTextAreaElement, select: HTMLSelectElement, option: HTMLOptionElement };
  g.el = function (tag, props, children) {
    const node = new (make[tag] || HTMLElement)();
    node.__tag = tag;
    node.__name = props.name || '';
    node.__attrs = props.attrs || {};
    node.__style = props.style || {};
    node.__rect = props.rect || null;
    node.__clipTo = props.clipTo;
    node.__paints = props.paints;
    node.__value = props.value || '';
    node.__text = props.text;
    node.__labels = (props.labels || []).map((text) => { const label = new HTMLElement(); label.__tag = 'label'; label.__text = text; return label; });
    for (const child of children || []) { child.__parent = node; node.__children.push(child); }
    return node;
  };
  // Attach a shadow tree of `children` to `host`; `slot` names where its light-DOM children are drawn.
  g.shadow = function (host, children) {
    const root = new ShadowRoot();
    root.__host = host;
    for (const child of children) child.__parent = root;
    root.__children = children;
    return root;
  };
  g.slotInto = function (node, slot) { node.__slot = slot; return node; };
  g.mount = function (body) { body.__parent = g.document; g.document.__children = [body]; g.document.__body = body; };
  g.mount(g.el('body', { rect: { x: 0, y: 0, w: 1280, h: 800 } }, []));
  g.__calls = [];
  g.capture = function (username, password) { g.__calls.push(['capture', username, password]); };
  g.followed = function () { g.__calls.push(['followed']); };
  g.focused = function () { g.__calls.push(['focused']); };
  g.addressCapture = function () { g.__calls.push(['address'].concat(Array.from(arguments))); };
  g.addressFocused = function () { g.__calls.push(['addressFocused']); };
  g.cardFocused = function () { g.__calls.push(['cardFocused'].concat(Array.from(arguments))); };
  g.addressFilled = function () { g.__calls.push(['addressFilled'].concat(Array.from(arguments))); };
  g.cardFilled = function () { g.__calls.push(['cardFilled'].concat(Array.from(arguments))); };
  // Each field that holds something, by name: a select once an option was
  // chosen, by the chosen option's value.
  g.held = function () {
    const out = [];
    walk(g.document, (node) => {
      if (node instanceof HTMLSelectElement) { if (node.__selected !== undefined) out.push(node.__name + '=' + node.options.item(node.__selected).value); }
      else if ((node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement) && node.__value !== '') out.push(node.__name + '=' + node.__value);
    });
    return out.join('|');
  };
})(this);
"#;

/// An engine with the stub page and `login.js` installed as the renderer
/// installs it, with or without the capture functions. Its fill functions
/// are the globals `fill`, `fillAddress` and `fillCard`.
fn installed(capture: bool) -> Result<Engine, String> {
    with_natives(if capture { "capture, followed, focused, addressCapture, addressFocused, cardFocused, addressFilled, cardFilled" } else { "undefined, undefined, focused, undefined, addressFocused, cardFocused, addressFilled, cardFilled" })
}

/// `login.js` as the renderer installs it in a frame of another site than
/// the page's, such as a payment processor's card fields: only the card
/// note and the card fill's report.
fn installed_for_cards_only() -> Result<Engine, String> {
    with_natives("undefined, undefined, undefined, undefined, undefined, cardFocused, undefined, cardFilled")
}

/// `login.js` as the renderer installs it in a document that can't take a
/// card, such as an http page: without the card note or the card report.
fn installed_without_cards(capture: bool) -> Result<Engine, String> {
    with_natives(if capture { "capture, followed, focused, addressCapture, addressFocused, undefined, addressFilled, undefined" } else { "undefined, undefined, focused, undefined, addressFocused, undefined, addressFilled, undefined" })
}

fn with_natives(natives: &str) -> Result<Engine, String> {
    after_page("", natives)
}

/// `login.js` installed after `prelude` changed the stub page, as a browser
/// without some API would be.
fn after_page(prelude: &str, natives: &str) -> Result<Engine, String> {
    let engine = Engine::new();
    engine.run(PAGE, "crossbow-test://page").expect("the stub page runs");
    engine.run(&format!("{prelude}; 0"), "crossbow-test://prelude").expect("the prelude runs");
    engine.check(SCRIPT, "crossbow://login")?;
    let made = format!("const made = ({SCRIPT})({natives}); globalThis.fill = made.login; globalThis.fillAddress = made.address; globalThis.fillCard = made.card;
        Object.getPrototypeOf(made) + ' ' + typeof fill + ' ' + typeof fillAddress + ' ' + typeof fillCard");
    engine.run(&made, "crossbow://login").and_then(|kinds| {
        if kinds == "null function function function" { Ok(engine) } else { Err(format!("login.js returned {kinds}, not its fill functions")) }
    })
}

fn listeners(engine: &Engine) -> String {
    engine.run("__listeners.map((l) => (l.target === window ? 'window' : 'other') + ':' + l.type + ':' + l.capture).join(',') + ' observe:' + __observed.join(',')", "crossbow-test://check").unwrap()
}

/// R5-1: the script parses, runs at load without throwing, and installs what
/// the renderer expects: its fill function, the focus listener, and with
/// capture the submit listener and the request observer.
#[test]
fn login_js_compiles_and_installs_its_listeners_in_a_real_javascript_engine() {
    let with = installed(true).unwrap_or_else(|error| panic!("login.js doesn't load: {error}"));
    assert_eq!(listeners(&with), "window:submit:true,window:focusin:true observe:resource");
    let without = installed(false).unwrap_or_else(|error| panic!("login.js doesn't load without capture: {error}"));
    assert_eq!(listeners(&without), "window:focusin:true observe:", "no submit listener or observer without capture");
}

/// The check finds what R5-1 was: a name declared twice in the script's
/// function body is a SyntaxError, so the whole script never runs.
#[test]
fn a_name_declared_twice_in_login_js_fails_to_compile() {
    let engine = Engine::new();
    let doubled = SCRIPT.replacen("'use strict';", "'use strict';\n  const apply = 0;", 1);
    let error = engine.check(&doubled, "crossbow://login").expect_err("a duplicate declaration is an early error");
    assert!(error.contains("SyntaxError") && error.contains("apply"), "{error}");
}

/// A login form at `y`, with a username and password field, as the stub's elements.
fn login_form(name: &str, y: u32, form_props: &str) -> String {
    format!("el('form', {{ name: '{name}', rect: {{ x: 100, y: {y}, w: 400, h: 120 }}{form_props} }}, [
        el('input', {{ name: '{name}-user', attrs: {{ type: 'email', autocomplete: 'username' }}, rect: {{ x: 110, y: {}, w: 300, h: 30 }} }}),
        el('input', {{ name: '{name}-pass', attrs: {{ type: 'password' }}, rect: {{ x: 110, y: {}, w: 300, h: 30 }} }}),
    ])", y + 10, y + 60)
}

/// R5-1, the script's behaviour once it loads: a trusted, user-activated
/// submit reaches capture() with the form's values; an untrusted one, or one
/// without user activation, doesn't; and a fill puts the login into a visible
/// form and says so.
#[test]
fn the_loaded_script_captures_a_trusted_submit_and_fills_a_visible_form() {
    let engine = installed(true).unwrap();
    let submit = |trusted: bool, active: bool| format!("(() => {{
        __calls = []; __activation.active = {active};
        const form = document.forms.item(0);
        form.__children[0].__value = 'fixture-user'; form.__children[1].__value = 'fixture-pass';
        const event = new Event('submit'); event.__target = form;
        Object.defineProperty(event, 'isTrusted', {{ get() {{ return {trusted}; }} }});
        __listeners.find((l) => l.type === 'submit').listener(event);
        return JSON.stringify(__calls); }})()");
    engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}])); 0", login_form("login", 100, "")), "t").unwrap();
    assert_eq!(engine.run(&submit(true, true), "t").unwrap(), r#"[["capture","fixture-user","fixture-pass"]]"#);
    assert_eq!(engine.run(&submit(false, true), "t").unwrap(), "[]", "an untrusted submit");
    assert_eq!(engine.run(&submit(true, false), "t").unwrap(), "[]", "without user activation");
    assert_eq!(engine.run("__log = []; String(fill('fixture-user', 'fixture-pass', true)) + ' ' + __log.join(',')", "t").unwrap(),
        "true value:login-user,input:login-user,change:login-user,value:login-pass,input:login-pass,change:login-pass");
}

/// Fill a page of `forms` (stub elements, in document order) in the main
/// frame with no field focused; the result and which fields received a value.
fn fill_page(forms: &[String]) -> String {
    let engine = installed(true).unwrap();
    engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}])); 0", forms.join(",")), "t").unwrap();
    engine.run("__log = []; String(fill('fixture-user', 'fixture-pass', true)) + ' ' + __log.filter((e) => e.startsWith('value:')).join(',')", "t").unwrap()
}

/// WP2: a field the user can't see is never filled, so the login goes into
/// the visible form after it, or nowhere. The visually-hidden pattern (a
/// 1 px box with `clip: rect(0 0 0 0)` and `overflow: hidden`, whose inputs
/// keep their full size), a clip-path, a field another element covers, and a
/// nearly transparent form each pass the old checks.
#[test]
fn a_fill_skips_a_visually_hidden_clipped_covered_or_faint_form() {
    let visible = login_form("real", 300, "");
    let hidden = [
        ("visually-hidden", login_form("hidden", 100, ", style: { position: 'absolute', overflow: 'hidden', clip: 'rect(0px, 0px, 0px, 0px)' }, clipTo: { x: 0, y: 0, w: 1, h: 1 }")),
        ("clip-path", login_form("hidden", 100, ", style: { 'clip-path': 'inset(50%)' }")),
        ("faint", login_form("hidden", 100, ", style: { opacity: '0.05' }")),
        ("off screen", login_form("hidden", 900, "")),
        ("disabled", login_form("hidden", 100, "").replace("type: 'password' }", "type: 'password', disabled: true }")),
        ("read-only", login_form("hidden", 100, "").replace("type: 'password' }", "type: 'password', readOnly: true }")),
        ("zero size", login_form("hidden", 100, "").replace("y: 160, w: 300", "y: 160, w: 0")),
    ];
    for (case, form) in &hidden {
        assert_eq!(fill_page(&[form.clone(), visible.clone()]), "true value:real-user,value:real-pass", "{case}: the visible form after it");
        assert_eq!(fill_page(std::slice::from_ref(form)), "false ", "{case}: nothing else to fill");
    }
    // A field under another element: the hit test at its centre finds the cover.
    let covered = format!("{}, el('div', {{ name: 'cover', rect: {{ x: 100, y: 100, w: 400, h: 120 }} }}, [])", login_form("hidden", 100, ""));
    assert_eq!(fill_page(&[covered.clone(), visible.clone()]), "true value:real-user,value:real-pass", "covered");
    assert_eq!(fill_page(&[covered]), "false ", "covered: nothing else to fill");
    assert_eq!(fill_page(&[visible]), "true value:real-user,value:real-pass", "an ordinary form is filled");
}

/// WP2's remainder, the sixth recheck's six forms, each before a visible
/// form with no field focused. A filter, a mask image or mask border image
/// (the seventh recheck's `-webkit-mask-box-image` and `mask-border`), a
/// blend mode, and fields transformed into a box under 8×8 px now leave the
/// form unfilled, so the login goes to the visible form, and nowhere
/// without it. Two still count as visible, as this build defines it:
/// transparent text, border and background (colours aren't read), and an
/// opacity of exactly 0.5 (the threshold). Alone, such a form is filled;
/// before a visible form, two forms could be meant, so neither is.
#[test]
fn a_fill_skips_filtered_masked_blended_and_tiny_forms() {
    let visible = login_form("real", 300, "");
    let refused = [
        ("filter: opacity(0)", login_form("hidden", 100, ", style: { filter: 'opacity(0)' }")),
        ("mask-image: linear-gradient(transparent, transparent)", login_form("hidden", 100, ", style: { 'mask-image': 'linear-gradient(transparent, transparent)' }")),
        ("mix-blend-mode on a white page", login_form("hidden", 100, ", style: { 'mix-blend-mode': 'screen' }")),
        ("fields scaled to a 4x1 px box", login_form("hidden", 100, "").replace("w: 300, h: 30", "w: 4, h: 1")),
        ("fields 7 px tall", login_form("hidden", 100, "").replace("w: 300, h: 30", "w: 300, h: 7")),
        ("a filter on an ancestor", format!("el('div', {{ style: {{ filter: 'blur(40px)' }} }}, [{}])", login_form("hidden", 100, ""))),
        ("-webkit-mask-box-image: a transparent fill", login_form("hidden", 100, ", style: { '-webkit-mask-box-image': 'linear-gradient(transparent, transparent) 1 fill' }")),
        ("mask-border: a transparent fill", login_form("hidden", 100, ", style: { 'mask-border': 'linear-gradient(transparent, transparent) 1 fill' }")),
    ];
    for (case, form) in &refused {
        assert_eq!(fill_page(&[form.clone(), visible.clone()]), "true value:real-user,value:real-pass", "{case}: the visible form after it");
        assert_eq!(fill_page(std::slice::from_ref(form)), "false ", "{case}: nothing else to fill");
    }
    let still = [
        ("transparent text, border and background", login_form("hidden", 100, ", style: { color: 'transparent', background: 'transparent', 'border-color': 'transparent' }")),
        ("opacity 0.5 exactly", login_form("hidden", 100, ", style: { opacity: '0.5' }")),
    ];
    for (case, form) in &still {
        assert_eq!(fill_page(std::slice::from_ref(form)), "true value:hidden-user,value:hidden-pass", "{case}: still counts as visible");
        assert_eq!(fill_page(&[form.clone(), visible.clone()]), "false ", "{case}: and so does the form after it, so neither is filled");
    }
    assert_eq!(fill_page(&[login_form("real", 300, "").replace("w: 300, h: 30", "w: 8, h: 8")]), "true value:real-user,value:real-pass", "8×8 px is enough");
}

/// WP2's remainder: the walk follows the flat tree. A light-DOM form slotted
/// into a shadow tree is drawn inside that tree's wrappers, which its
/// light-DOM parents never meet: a faint or filtered wrapper hides it, and
/// the walk reaches the wrapper through the slot and leaves the shadow tree
/// through its host. In an ordinary wrapper it is filled when it is the only
/// form, and beside another visible form, neither is.
#[test]
fn a_fill_follows_the_flat_tree_through_slots_and_shadow_hosts() {
    let page = |wrapper: &str, host: &str, real: bool| {
        let engine = installed(true).unwrap();
        engine.run(&format!("(() => {{
            const form = {};
            const host = el('div', {{ name: 'host', style: {{ {host} }} }}, [form]);
            const slot = el('slot', {{}}, []);
            shadow(host, [el('div', {{ name: 'wrapper', style: {{ {wrapper} }} }}, [slot])]);
            slotInto(form, slot);
            mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [host{}]));
            return 0; }})()", login_form("slotted", 100, ""), if real { format!(", {}", login_form("real", 300, "")) } else { String::new() }), "t").unwrap();
        engine.run("__log = []; String(fill('fixture-user', 'fixture-pass', true)) + ' ' + __log.filter((e) => e.startsWith('value:')).join(',')", "t").unwrap()
    };
    assert_eq!(page("", "", false), "true value:slotted-user,value:slotted-pass", "an ordinary wrapper");
    assert_eq!(page("", "", true), "false ", "an ordinary wrapper, beside a visible form");
    assert_eq!(page("opacity: '0.1'", "", true), "true value:real-user,value:real-pass", "a faint wrapper in the shadow tree");
    assert_eq!(page("filter: 'opacity(0)'", "", true), "true value:real-user,value:real-pass", "a filtered wrapper in the shadow tree");
    assert_eq!(page("opacity: '0.7'", "opacity: '0.7'", true), "true value:real-user,value:real-pass", "0.7 × 0.7 across the host is under 0.5");
}

/// With no login field focused, only the main frame fills, and only when
/// one form could be meant. A subframe fills only the login form its focus
/// is in, and never falls back to another form.
#[test]
fn a_subframe_fills_only_the_login_form_that_has_focus() {
    let engine = installed(true).unwrap();
    engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}, {}])); __active = undefined; 0", login_form("first", 100, ""), login_form("second", 300, "")), "t").unwrap();
    let fill = |main: bool| engine.run(&format!("__log = []; String(fill('fixture-user', 'fixture-pass', {main})) + ' ' + __log.filter((e) => e.startsWith('value:')).join(',')"), "t").unwrap();
    assert_eq!(fill(false), "false ", "a subframe with no login field focused");
    assert_eq!(fill(true), "false ", "nor the main frame, with two visible forms either could be meant");
    engine.run("__active = document.forms.item(0).__children[1]; 0", "t").unwrap();
    assert_eq!(fill(true), "true value:first-user,value:first-pass", "the main frame fills the focused form");
    engine.run("__active = document.forms.item(1).__children[1]; 0", "t").unwrap();
    assert_eq!(fill(false), "true value:second-user,value:second-pass", "a subframe fills the focused login form");
    engine.run("__active = document.forms.item(1).__children[1]; document.forms.item(1).__style = { opacity: '0.05' }; 0", "t").unwrap();
    assert_eq!(fill(false), "false ", "a hidden focused login form, and no fallback");
    assert_eq!(fill(true), "false ", "nor in the main frame");
}

/// `form` as the light-DOM child of a host whose shadow tree draws it in a
/// wrapper styled `wrapper`. In an open shadow root the form's assignedSlot
/// is the slot; in a closed one it is null, as script outside it sees it.
/// Either way layout draws it in the slot, which the engine's own
/// visibility answer follows.
fn in_shadow(form: &str, wrapper: &str, open: bool) -> String {
    format!("(() => {{ const form = {form}; const host = el('div', {{ name: 'host' }}, [form]); const slot = el('slot', {{}}, []);
        shadow(host, [el('div', {{ name: 'wrapper', style: {{ {wrapper} }} }}, [slot])]); form.__drawnIn = slot; {} return host; }})()", if open { "slotInto(form, slot);" } else { "" })
}

/// WP2's closed shadow root, from the seventh recheck: a form slotted into a
/// closed shadow root is drawn in wrappers that script outside it can't
/// reach, so a faint, filtered or masked wrapper there can't be read, and
/// the form passes every check. With no field focused, the main frame now
/// fills only when exactly one form has login fields the user may see; with
/// that form before a visible one, two do, so nothing is filled and the
/// login never goes into the hidden form. Clicking the visible form's
/// password field fills it. An open root's wrapper is read, so its hidden
/// form doesn't count, and the visible one is filled.
#[test]
fn with_no_field_focused_a_login_fills_only_when_one_form_could_be_meant() {
    let visible = login_form("real", 300, "");
    for (case, wrapper) in [("opacity 0.01", "opacity: '0.01'"), ("filter: opacity(0)", "filter: 'opacity(0)'"), ("a transparent mask image", "'mask-image': 'linear-gradient(transparent, transparent)'")] {
        let closed = in_shadow(&login_form("slotted", 100, ""), wrapper, false);
        assert_eq!(fill_page(&[closed.clone(), visible.clone()]), "false ", "closed root, {case}: neither form");
        let engine = installed(true).unwrap();
        engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{closed}, {visible}])); __active = document.forms.item(1).__children[1]; 0"), "t").unwrap();
        assert_eq!(engine.run("__log = []; String(fill('fixture-user', 'fixture-pass', true)) + ' ' + __log.filter((e) => e.startsWith('value:')).join(',')", "t").unwrap(),
            "true value:real-user,value:real-pass", "closed root, {case}: the focused visible form");
        let open = in_shadow(&login_form("slotted", 100, ""), wrapper, true);
        assert_eq!(fill_page(&[open, visible.clone()]), "true value:real-user,value:real-pass", "open root, {case}: its wrapper is read");
    }
    assert_eq!(fill_page(&[in_shadow(&login_form("slotted", 100, ""), "", false)]), "true value:slotted-user,value:slotted-pass", "the only form, in a closed root, is filled: its wrappers can't be read");
    assert_eq!(fill_page(&[login_form("first", 100, ""), visible.clone()]), "false ", "two visible forms");
    assert_eq!(fill_page(&[login_form("search", 100, "").replace("type: 'password'", "type: 'search'"), visible]), "true value:real-user,value:real-pass", "a form without a password field isn't a login form");
}

/// WP2's geometry, from the seventh recheck: fields rotated 45° and
/// squashed into 0.3 px lines have 212×212 boxes, which pass the 8×8 check,
/// with the line through each centre; an ancestor's overflow clip can leave
/// only a 2 px strip through the password field's centre. A click must now
/// reach a field at its centre and a quarter of its box in from each
/// corner, which a line or a strip misses, so neither form is filled and
/// the login goes to the visible form. A field whose right edge another
/// element covers, as a show-password button does, is still filled.
#[test]
fn a_fill_needs_the_field_at_its_centre_and_a_quarter_in_from_each_corner() {
    let visible = login_form("real", 300, "");
    let line = |x: u32, y: u32| format!("x: {x}, y: {y}, w: 212, h: 212 }}, paints: (x, y) => Math.abs((x - {x}) - (y - {y})) < 0.25");
    let refused = [
        ("rotate(45deg) scaleY(0.01): 0.3 px lines in 212×212 boxes", login_form("hidden", 100, "").replace("x: 110, y: 110, w: 300, h: 30 }", &line(154, 9)).replace("x: 110, y: 160, w: 300, h: 30 }", &line(154, 59))),
        ("an overflow clip leaving a 2 px strip through the password field's centre", login_form("hidden", 100, ", style: { overflow: 'hidden' }, clipTo: { x: 100, y: 174, w: 400, h: 2 }")),
    ];
    for (case, form) in &refused {
        assert!(form.contains("paints") || form.contains("clipTo"), "{case}");
        assert_eq!(fill_page(&[form.clone(), visible.clone()]), "true value:real-user,value:real-pass", "{case}: the visible form after it");
        assert_eq!(fill_page(std::slice::from_ref(form)), "false ", "{case}: nothing else to fill");
    }
    let button = format!("{visible}, el('div', {{ name: 'show', rect: {{ x: 365, y: 360, w: 45, h: 30 }} }}, [])");
    assert_eq!(fill_page(&[button]), "true value:real-user,value:real-pass", "a button over the right edge");
}

// ---------------------------------------------------------------------------
// Addresses (ASP-P17)
// ---------------------------------------------------------------------------

/// The synthetic address the tests fill, in `ADDRESS_PARTS` order.
const ADDRESS: [&str; super::ADDRESS_PARTS] =
    ["Fixture Person", "Fixture Org", "1 Test Street\nUnit 2\nBuilding 3", "Testville", "California", "90000", "United States", "+1 555 0100", "fixture@example.test"];

/// `text` as a JavaScript string literal.
fn js(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n"))
}

/// A field of an address form: tag, name, attributes (a JS object's body),
/// label texts (a JS array's body) and, for a select, "value:text" options.
type Field<'a> = (&'a str, &'a str, &'a str, &'a str, &'a [&'a str]);

/// A form named `name` at `y` holding `fields`, 32 px apart.
fn address_form(name: &str, y: u32, form_props: &str, fields: &[Field<'_>]) -> String {
    let rows: Vec<String> = fields.iter().enumerate().map(|(i, (tag, field, attrs, labels, options))| {
        let options: Vec<String> = options.iter().map(|option| {
            let (value, text) = option.split_once(':').unwrap();
            format!("el('option', {{ attrs: {{ value: {} }}, text: {} }}, [])", js(value), js(text))
        }).collect();
        format!("el('{tag}', {{ name: '{name}-{field}', attrs: {{ {attrs} }}, labels: [{labels}], rect: {{ x: 110, y: {}, w: 300, h: 28 }} }}, [{}])", y + 6 + 32 * i as u32, options.join(", "))
    }).collect();
    format!("el('form', {{ name: '{name}', rect: {{ x: 100, y: {y}, w: 400, h: {} }}{form_props} }}, [{}])", 12 + 32 * fields.len(), rows.join(",\n"))
}

/// An engine with `forms` (stub elements, in document order) on its page.
fn address_page(forms: &[String], capture: bool) -> Engine {
    let engine = installed(capture).unwrap();
    engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}])); __active = undefined; 0", forms.join(",")), "t").unwrap();
    engine
}

/// Fill `parts` as the renderer does, with the values the browser computes;
/// the result and, once a second has passed for the engine to answer, what
/// every field then holds.
fn fill_address(engine: &Engine, parts: &[&str; super::ADDRESS_PARTS], main: bool) -> String {
    let values: Vec<String> = super::address_values(parts).iter().map(|value| js(value.expose())).collect();
    engine.run(&format!("__log = []; (() => {{ const result = fillAddress({main}, {}); __advance(1100); return String(result) + ' ' + held(); }})()", values.join(", ")), "t").unwrap()
}

/// A shipping form named by autocomplete tokens, with a card name and a
/// username field that are never filled with an address.
fn token_form(name: &str, y: u32, form_props: &str) -> String {
    address_form(name, y, form_props, &[
        ("input", "name", "autocomplete: 'shipping name'", "", &[]),
        ("input", "card", "autocomplete: 'cc-name'", "'Name on card'", &[]),
        ("input", "org", "autocomplete: 'organization'", "", &[]),
        ("input", "line1", "autocomplete: 'shipping address-line1'", "", &[]),
        ("input", "line2", "autocomplete: 'shipping address-line2'", "", &[]),
        ("input", "city", "autocomplete: 'address-level2'", "", &[]),
        ("select", "region", "autocomplete: 'address-level1'", "", &[":Choose…", "CA:California", "NY:New York"]),
        ("input", "postal", "autocomplete: 'section-a shipping postal-code'", "", &[]),
        ("select", "country", "autocomplete: 'country'", "", &["US:United States", "FR:France"]),
        ("input", "phone", "type: 'tel', autocomplete: 'tel'", "", &[]),
        ("input", "email", "type: 'email', autocomplete: 'email'", "", &[]),
        ("input", "user", "autocomplete: 'username'", "'Name'", &[]),
    ])
}

const TOKEN_FILLED: &str = "true tokens-name=Fixture Person|tokens-org=Fixture Org|tokens-line1=1 Test Street|tokens-line2=Unit 2, Building 3|tokens-city=Testville|tokens-region=CA|tokens-postal=90000|tokens-country=US|tokens-phone=+1 555 0100|tokens-email=fixture@example.test";

/// ASP-P17: a fill maps each part to its field by autocomplete token first,
/// whatever hints and section names the attribute also carries, choosing a
/// select's option by its text; the street goes over the line fields; a
/// card's or username's token keeps its field out even with a label that
/// says "Name".
#[test]
fn an_address_fills_each_field_by_its_autocomplete_token() {
    let engine = focus_first(address_page(&[token_form("tokens", 10, "")], false));
    assert_eq!(fill_address(&engine, &ADDRESS, true), TOKEN_FILLED);
}

/// ASP-P17: without tokens, a field's labels, then its aria-label,
/// placeholder, name and id say what it holds; an email or telephone input's
/// type does too. A text area takes the street on its lines. Fields that are
/// a username or a search box are never filled.
#[test]
fn an_address_fills_fields_by_their_labels_names_and_types() {
    let form = address_form("labels", 10, "", &[
        ("input", "user", "", "'Username'", &[]),
        ("input", "full", "", "'Full name'", &[]),
        ("textarea", "street", "", "'Street address'", &[]),
        ("input", "city", "", "'Town / City'", &[]),
        ("input", "state", "placeholder: 'State or province'", "", &[]),
        ("input", "zip", "name: 'billing_postcode'", "", &[]),
        ("input", "country", "id: 'country-field'", "", &[]),
        ("input", "phone", "'aria-label': 'Mobile number'", "", &[]),
        ("input", "email", "type: 'email'", "", &[]),
        ("input", "search", "placeholder: 'Search addresses'", "", &[]),
    ]);
    let engine = focus_first(address_page(&[form], false));
    assert_eq!(fill_address(&engine, &ADDRESS, true),
        "true labels-full=Fixture Person|labels-street=1 Test Street\nUnit 2\nBuilding 3|labels-city=Testville|labels-state=California|labels-zip=90000|labels-country=United States|labels-phone=+1 555 0100|labels-email=fixture@example.test");
    // Split name fields, one street line and three line fields.
    let split = address_form("split", 10, "", &[
        ("input", "given", "autocomplete: 'given-name'", "", &[]),
        ("input", "family", "", "'Last name'", &[]),
        ("input", "street", "autocomplete: 'street-address'", "", &[]),
        ("input", "postal", "", "'ZIP code'", &[]),
    ]);
    assert_eq!(fill_address(&focus_first(address_page(&[split], false)), &ADDRESS, true),
        "true split-given=Fixture|split-family=Person|split-street=1 Test Street, Unit 2, Building 3|split-postal=90000");
    let lines = address_form("lines", 10, "", &[
        ("input", "a", "", "'Address line 1'", &[]),
        ("input", "b", "", "'Address line 2'", &[]),
        ("input", "c", "name: 'address3'", "", &[]),
        ("input", "city", "", "'City'", &[]),
    ]);
    assert_eq!(fill_address(&focus_first(address_page(&[lines], false)), &ADDRESS, true), "true lines-a=1 Test Street|lines-b=Unit 2|lines-c=Building 3|lines-city=Testville");
}

/// ASP-P17: a country, or a region, is chosen by its option's value or
/// text, ignoring case and spaces. One no option matches leaves the select
/// as it was and still fills the rest; a text field takes it as saved.
#[test]
fn a_country_is_chosen_by_code_or_name_and_never_guessed() {
    let form = || address_form("country", 10, "", &[
        ("input", "street", "autocomplete: 'street-address'", "", &[]),
        ("input", "city", "autocomplete: 'address-level2'", "", &[]),
        ("select", "country", "autocomplete: 'country-name'", "", &[":Choose a country", "US:United States", "FR:France", "DE:Germany"]),
    ]);
    let with = |country: &str| {
        let mut parts = ADDRESS;
        parts[6] = country;
        fill_address(&focus_first(address_page(&[form()], false)), &parts, true)
    };
    let filled = |country: &str| format!("true country-street=1 Test Street, Unit 2, Building 3|country-city=Testville|country-country={country}");
    assert_eq!(with("FR"), filled("FR"), "by its code");
    assert_eq!(with("fr"), filled("FR"), "by its code in another case");
    assert_eq!(with("  france "), filled("FR"), "by its name");
    assert_eq!(with("United States"), filled("US"));
    assert_eq!(with("Deutschland"), "true country-street=1 Test Street, Unit 2, Building 3|country-city=Testville", "no option matches: the select is left alone");
    let text = address_form("text", 10, "", &[
        ("input", "street", "autocomplete: 'street-address'", "", &[]),
        ("input", "postal", "autocomplete: 'postal-code'", "", &[]),
        ("input", "country", "autocomplete: 'country'", "", &[]),
    ]);
    let mut parts = ADDRESS;
    parts[6] = "Deutschland";
    assert_eq!(fill_address(&focus_first(address_page(&[text], false)), &parts, true), "true text-street=1 Test Street, Unit 2, Building 3|text-postal=90000|text-country=Deutschland");
}

/// ASP-P17: the fill uses the password fill's visibility rule. With a field
/// of the visible form focused, it fills that form and never a hidden form
/// before it; with a field of the hidden form focused, nothing. Round 9: a
/// form in a closed shadow root, whose faint wrapper can't be read, is never
/// reached from a visible form's field (until round 9, with nothing focused,
/// the two made the fill refuse both). A disabled or read-only field in the
/// focused form is left out. Fields outside any form are an address too.
#[test]
fn an_address_never_fills_a_hidden_decoy_form_or_field() {
    let visible = token_form("tokens", 400, "");
    let decoys = [
        ("visually hidden", token_form("hidden", 10, ", style: { position: 'absolute', overflow: 'hidden', clip: 'rect(0px, 0px, 0px, 0px)' }, clipTo: { x: 0, y: 0, w: 1, h: 1 }")),
        ("faint", token_form("hidden", 10, ", style: { opacity: '0.05' }")),
        ("filtered", token_form("hidden", 10, ", style: { filter: 'opacity(0)' }")),
        ("off screen", token_form("hidden", 900, "")),
    ];
    for (case, decoy) in &decoys {
        let engine = address_page(&[decoy.clone(), visible.clone()], false);
        engine.run("__active = document.forms.item(1).__children[5]; 0", "t").unwrap();
        assert_eq!(fill_address(&engine, &ADDRESS, true), TOKEN_FILLED, "{case}: the visible form after it");
        assert_eq!(fill_address(&focus_first(address_page(std::slice::from_ref(decoy), false)), &ADDRESS, true), "false ", "{case}: its own field focused");
    }
    let closed = in_shadow(&token_form("hidden", 10, ""), "opacity: '0.01'", false);
    let engine = address_page(&[closed, visible.clone()], false);
    engine.run("__active = document.forms.item(1).__children[5]; 0", "t").unwrap();
    assert_eq!(fill_address(&engine, &ADDRESS, true), TOKEN_FILLED, "a closed shadow root's faint form before the focused visible one");
    let partial = token_form("tokens", 10, "").replace("autocomplete: 'tel' }", "autocomplete: 'tel', disabled: true }").replace("autocomplete: 'organization' }", "autocomplete: 'organization', readOnly: true }");
    assert_eq!(fill_address(&focus_first(address_page(&[partial], false)), &ADDRESS, true), TOKEN_FILLED.replace("tokens-org=Fixture Org|", "").replace("tokens-phone=+1 555 0100|", ""));
    let loose = "el('div', { rect: { x: 100, y: 10, w: 400, h: 100 } }, [
        el('input', { name: 'loose-street', attrs: { autocomplete: 'street-address' }, rect: { x: 110, y: 16, w: 300, h: 28 } }),
        el('input', { name: 'loose-postal', attrs: { autocomplete: 'postal-code' }, rect: { x: 110, y: 48, w: 300, h: 28 } })])";
    assert_eq!(fill_address(&focus_first(address_page(&[loose.into()], false)), &ADDRESS, true), "true loose-street=1 Test Street, Unit 2, Building 3|loose-postal=90000");
}

/// ASP-P17: with an address field focused, only its form is filled, in any
/// frame; with none focused, nothing is, in a subframe or (round 9) the
/// main frame. (A cross-origin frame never has the script and is never sent
/// a fill: see the browser-side test
/// a_fill_of_an_address_goes_only_to_a_frame_that_installed_it.) A focused
/// form whose visible fields aren't an address fills nothing, with no fallback.
#[test]
fn an_address_fills_only_the_focused_form_and_a_subframe_needs_focus() {
    let engine = address_page(&[token_form("first", 10, ""), token_form("second", 400, "")], false);
    assert_eq!(fill_address(&engine, &ADDRESS, false), "false ", "a subframe with no address field focused");
    assert_eq!(fill_address(&engine, &ADDRESS, true), "false ", "nor the main frame");
    engine.run("__active = document.forms.item(1).__children[5]; 0", "t").unwrap();
    assert_eq!(fill_address(&engine, &ADDRESS, false), TOKEN_FILLED.replace("tokens-", "second-"), "the focused form only, in a subframe");
    assert_eq!(fill_address(&engine, &ADDRESS, true), TOKEN_FILLED.replace("tokens-", "second-"), "and in the main frame");
    engine.run("__active = document.forms.item(1).__children[5]; document.forms.item(1).__style = { opacity: '0.05' }; 0", "t").unwrap();
    assert_eq!(fill_address(&address_page(&[token_form("first", 10, ""), token_form("second", 400, ", style: { opacity: '0.05' }")], false), &ADDRESS, true), "false ", "unfocused: nothing, though one visible form");
    assert_eq!(fill_address(&engine, &ADDRESS, true), "false second-name=Fixture Person|second-org=Fixture Org|second-line1=1 Test Street|second-line2=Unit 2, Building 3|second-city=Testville|second-region=CA|second-postal=90000|second-country=US|second-phone=+1 555 0100|second-email=fixture@example.test",
        "a hidden focused address form fills nothing more, and nothing else");
}

/// The user clicks into `field` (a stub expression), as typing into it
/// does: the focus event the page's listeners get, then a moment.
fn click_into(engine: &Engine, field: &str) {
    engine.run(&format!("(() => {{ const event = new Event('focusin'); event.__target = {field}; __active = event.__target;
        __listeners.filter((l) => l.type === 'focusin').forEach((l) => l.listener(event)); __advance(100); return 0; }})()"), "t").unwrap();
}

/// Submitting fills in a test page, a moment after the last change: what
/// `addressCapture` received.
fn submit_address(engine: &Engine, form: usize, trusted: bool, active: bool) -> String {
    engine.run(&format!("(() => {{
        __advance(100); __calls = []; __activation.active = {active};
        const form = document.forms.item({form});
        const event = new Event('submit'); event.__target = form;
        Object.defineProperty(event, 'isTrusted', {{ get() {{ return {trusted}; }} }});
        const listener = __listeners.find((l) => l.type === 'submit');
        if (listener) listener.listener(event);
        return JSON.stringify(__calls); }})()"), "t").unwrap()
}

/// ASP-P17: a trusted, user-activated submit of an address form sends its
/// parts, a select's chosen option by its text; an untrusted one, one
/// without activation, one without a street and a city or postal code, and
/// any submit while capture is off, send nothing. A sign-up form with a
/// password sends the login and the address.
#[test]
fn a_submitted_address_form_is_captured_only_when_trusted_and_shaped() {
    let engine = address_page(&[token_form("tokens", 10, "")], true);
    engine.run("(() => { const f = document.forms.item(0).__children;
        f[0].__value = 'Fixture Person'; f[1].__value = 'Card Holder'; f[3].__value = '1 Test Street'; f[4].__value = ' Unit 2 ';
        f[5].__value = 'Testville'; f[6].__selected = 1; f[7].__value = '90000'; f[8].__selected = 1; f[11].__value = 'fixture-user'; return 0; })()", "t").unwrap();
    click_into(&engine, "document.forms.item(0).__children[0]");
    assert_eq!(submit_address(&engine, 0, true, true), r#"[["address","Fixture Person","","1 Test Street\nUnit 2","Testville","California","90000","France","",""]]"#);
    assert_eq!(submit_address(&engine, 0, false, true), "[]", "an untrusted submit");
    assert_eq!(submit_address(&engine, 0, true, false), "[]", "without user activation");
    engine.run("document.forms.item(0).__children[5].__value = ''; document.forms.item(0).__children[7].__value = ''; 0", "t").unwrap();
    assert_eq!(submit_address(&engine, 0, true, true), "[]", "no city and no postal code");
    let off = address_page(&[token_form("tokens", 10, "")], false);
    off.run("(() => { const f = document.forms.item(0).__children; f[3].__value = '1 Test Street'; f[5].__value = 'Testville'; return 0; })()", "t").unwrap();
    assert_eq!(submit_address(&off, 0, true, true), "[]", "capture off: no submit listener at all");
    let signup = address_form("signup", 10, "", &[
        ("input", "user", "type: 'email', autocomplete: 'username'", "", &[]),
        ("input", "pass", "type: 'password'", "", &[]),
        ("input", "street", "autocomplete: 'street-address'", "", &[]),
        ("input", "postal", "autocomplete: 'postal-code'", "", &[]),
    ]);
    let both = address_page(&[signup], true);
    both.run("(() => { const f = document.forms.item(0).__children; f[0].__value = 'fixture-user'; f[1].__value = 'fixture-pass'; f[2].__value = '1 Test Street'; f[3].__value = '90000'; return 0; })()", "t").unwrap();
    click_into(&both, "document.forms.item(0).__children[2]");
    assert_eq!(submit_address(&both, 0, true, true), r#"[["capture","fixture-user","fixture-pass"],["address","","","1 Test Street","","","90000","","",""]]"#);
}

/// ASP-P17: focusing a field of an address form notes it once, content-free,
/// whether capture is on or off; a form without a street and a city or
/// postal code, or a field that isn't an address part, isn't noted, though
/// moving focus to it from a noted form says focus left that form (C11-1).
#[test]
fn focusing_an_address_field_sends_a_content_free_note_once() {
    for capture in [true, false] {
        let partial = address_form("partial", 400, "", &[("input", "street", "autocomplete: 'street-address'", "", &[]), ("input", "phone", "type: 'tel'", "", &[])]);
        let engine = address_page(&[token_form("tokens", 10, ""), partial], capture);
        let focus = |form: usize, field: usize| engine.run(&format!("(() => {{ __calls = []; const event = new Event('focusin');
            event.__target = document.forms.item({form}).__children[{field}];
            __listeners.find((l) => l.type === 'focusin').listener(event); return JSON.stringify(__calls); }})()"), "t").unwrap();
        assert_eq!(focus(0, 1), r#"[["cardFocused"]]"#, "a card field: a card note, never an address note");
        assert_eq!(focus(0, 5), r#"[["addressFocused"]]"#, "capture {capture}");
        assert_eq!(focus(0, 3), "[]", "once per form");
        assert_eq!(focus(1, 0), r#"[["addressFocused"],["cardFocused"]]"#, "no city or postal code: focus left the noted forms");
        assert_eq!(focus(1, 1), "[]", "nothing noted to leave");
        let fresh = address_page(&[token_form("tokens", 10, ""), address_form("partial", 400, "", &[("input", "street", "autocomplete: 'street-address'", "", &[]), ("input", "phone", "type: 'tel'", "", &[])])], capture);
        let first = fresh.run("(() => { __calls = []; const event = new Event('focusin');
            event.__target = document.forms.item(1).__children[0];
            __listeners.find((l) => l.type === 'focusin').listener(event); return JSON.stringify(__calls); })()", "t").unwrap();
        assert_eq!(first, "[]", "no city or postal code, and nothing noted before");
    }
}

// -- payment cards (ASP-P16) -------------------------------------------------------------------
// A published test number and an invented name only.

const CARD: [&str; super::CARD_PARTS] = ["Fixture Cardholder", "4242424242424242", "07", "2034"];

/// An engine whose page, one that may take a card, holds `forms`.
fn card_page(forms: &[String]) -> Engine {
    let engine = installed(false).unwrap();
    engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}])); __active = undefined; 0", forms.join(",")), "t").unwrap();
    engine
}

/// Fill `parts` as the renderer does, with the values the browser computes:
/// the result and, once a second has passed for the engine to answer, what
/// every field then holds, and whether any submit event was sent.
fn fill_card(engine: &Engine, parts: &[&str; super::CARD_PARTS], main: bool) -> String {
    let values: Vec<String> = super::card_values(parts).iter().map(|value| js(value.expose())).collect();
    engine.run(&format!("__log = []; (() => {{ const result = fillCard({main}, {}); __advance(1100);
        return String(result) + ' ' + held() + (__log.some((entry) => entry.startsWith('submit')) ? ' SUBMITTED' : ''); }})()", values.join(", ")), "t").unwrap()
}

/// A checkout form named by autocomplete tokens: the cardholder, the
/// number, a combined MM / YY expiry, a security code and an email.
fn card_form(name: &str, y: u32, form_props: &str) -> String {
    address_form(name, y, form_props, &[
        ("input", "holder", "autocomplete: 'cc-name'", "", &[]),
        ("input", "number", "type: 'tel', autocomplete: 'billing cc-number'", "", &[]),
        ("input", "exp", "autocomplete: 'cc-exp', placeholder: 'MM / YY'", "", &[]),
        ("input", "csc", "autocomplete: 'cc-csc'", "'CVC'", &[]),
        ("input", "email", "type: 'email', autocomplete: 'email'", "", &[]),
    ])
}

const CARD_FILLED: &str = "true cards-holder=Fixture Cardholder|cards-number=4242424242424242|cards-exp=07 / 34";

/// ASP-P16: a card fills each field by its cc- autocomplete token, whatever
/// section and hint the attribute also carries; the security code is never
/// filled, even when it is the focused field; an email is no card field;
/// and the form is never submitted. A fill starts from a focused field (see
/// `a_card_fill_with_nothing_focused_fills_nothing`), here the first one.
#[test]
fn a_card_fills_each_field_by_its_cc_token_and_never_the_security_code() {
    let engine = focus_first(card_page(&[card_form("cards", 10, "")]));
    assert_eq!(fill_card(&engine, &CARD, true), CARD_FILLED);
    let engine = card_page(&[card_form("cards", 10, "")]);
    engine.run("__active = document.forms.item(0).__children[3]; 0", "t").unwrap();
    assert_eq!(fill_card(&engine, &CARD, false), CARD_FILLED, "with the security code focused, its form is filled and the code is left empty");
    let split = address_form("split", 10, "", &[
        ("input", "given", "autocomplete: 'cc-given-name'", "", &[]),
        ("input", "family", "autocomplete: 'cc-family-name'", "", &[]),
        ("input", "number", "autocomplete: 'cc-number'", "", &[]),
        ("input", "type", "autocomplete: 'cc-type'", "", &[]),
    ]);
    assert_eq!(fill_card(&focus_first(card_page(&[split])), &CARD, true), "true split-given=Fixture|split-family=Cardholder|split-number=4242424242424242");
}

/// ASP-P16: without cc- tokens, a field's labels, then its aria-label,
/// placeholder, name and id say what it holds. A security code by any of
/// those names is never filled, nor a postal code beside the card fields.
#[test]
fn a_card_fills_fields_by_their_labels_when_they_have_no_token() {
    let form = address_form("labels", 10, "", &[
        ("input", "holder", "", "'Name on card'", &[]),
        ("input", "number", "type: 'tel'", "'Card number'", &[]),
        ("input", "exp", "placeholder: 'MM/YYYY'", "'Expiry date'", &[]),
        ("input", "code", "", "'Security code'", &[]),
        ("input", "cvv", "name: 'cc_cvv'", "", &[]),
        ("input", "zip", "", "'ZIP code'", &[]),
    ]);
    assert_eq!(fill_card(&focus_first(card_page(&[form])), &CARD, true),
        "true labels-holder=Fixture Cardholder|labels-number=4242424242424242|labels-exp=07/2034");
    let named = address_form("named", 10, "", &[
        ("input", "a", "name: 'cardnumber'", "", &[]),
        ("input", "b", "'aria-label': 'Expiration month'", "", &[]),
        ("input", "c", "id: 'exp-year', maxLength: 2", "", &[]),
    ]);
    assert_eq!(fill_card(&focus_first(card_page(&[named])), &CARD, true), "true named-a=4242424242424242|named-b=07|named-c=34");
}

/// ASP-P16: a combined expiry field takes MM/YY, or MM/YYYY when its
/// placeholder or length asks for four digits of year, with no separator
/// when its length leaves no room; split fields take the month and year,
/// two or four digits as the field asks; selects are chosen by an option's
/// value or text; a month input takes YYYY-MM.
#[test]
fn a_card_expiry_fills_combined_and_split_fields_and_selects() {
    let expiry = |attrs: &str| {
        let form = address_form("e", 10, "", &[("input", "number", "autocomplete: 'cc-number'", "", &[]), ("input", "exp", attrs, "", &[])]);
        fill_card(&focus_first(card_page(&[form])), &CARD, true)
    };
    assert_eq!(expiry("autocomplete: 'cc-exp', placeholder: 'MM/YY'"), "true e-number=4242424242424242|e-exp=07/34");
    assert_eq!(expiry("autocomplete: 'cc-exp', placeholder: 'mm / yyyy'"), "true e-number=4242424242424242|e-exp=07 / 2034");
    assert_eq!(expiry("autocomplete: 'cc-exp', maxLength: 7"), "true e-number=4242424242424242|e-exp=07/2034");
    assert_eq!(expiry("autocomplete: 'cc-exp', maxLength: 4"), "true e-number=4242424242424242|e-exp=0734");
    assert_eq!(expiry("autocomplete: 'cc-exp'"), "true e-number=4242424242424242|e-exp=07/34", "MM/YY when nothing says otherwise");
    assert_eq!(expiry("type: 'month', autocomplete: 'cc-exp'"), "true e-number=4242424242424242|e-exp=2034-07");
    let months: Vec<String> = (1..=12).map(|m| format!("{m:02}:{m:02}")).collect();
    let months: Vec<&str> = months.iter().map(String::as_str).collect();
    let selects = address_form("s", 10, "", &[
        ("input", "number", "autocomplete: 'cc-number'", "", &[]),
        ("select", "month", "autocomplete: 'cc-exp-month'", "", &months),
        ("select", "year", "autocomplete: 'cc-exp-year'", "", &["2033:2033", "2034:2034", "2035:2035"]),
    ]);
    assert_eq!(fill_card(&focus_first(card_page(&[selects])), &CARD, true), "true s-number=4242424242424242|s-month=07|s-year=2034");
    let named = address_form("n", 10, "", &[
        ("input", "number", "autocomplete: 'cc-number'", "", &[]),
        ("select", "month", "autocomplete: 'cc-exp-month'", "", &[":Month", "1:January", "7:July", "12:December"]),
        ("select", "year", "autocomplete: 'cc-exp-year'", "", &[":Year", "33:2033", "34:2034"]),
    ]);
    assert_eq!(fill_card(&focus_first(card_page(&[named])), &CARD, true), "true n-number=4242424242424242|n-month=7|n-year=34");
    let inputs = address_form("i", 10, "", &[
        ("input", "number", "autocomplete: 'cc-number'", "", &[]),
        ("input", "month", "autocomplete: 'cc-exp-month'", "", &[]),
        ("input", "year", "autocomplete: 'cc-exp-year', placeholder: 'YY'", "", &[]),
    ]);
    assert_eq!(fill_card(&focus_first(card_page(&[inputs])), &CARD, true), "true i-number=4242424242424242|i-month=07|i-year=34");
}

/// ASP-P16: the card fill uses the password fill's visibility rule. With a
/// hidden decoy form before a visible one, focusing the visible one fills
/// it alone, and focusing the decoy's own field fills nothing; two visible
/// card forms with no field focused fill neither, until one has focus.
#[test]
fn a_card_never_fills_a_hidden_decoy_form_and_needs_focus() {
    let visible = card_form("cards", 400, "");
    let focused = |page: &[String], form: usize| {
        let engine = card_page(page);
        engine.run(&format!("__active = document.forms.item({form}).__children[1]; 0"), "t").unwrap();
        fill_card(&engine, &CARD, true)
    };
    for (case, decoy) in [
        ("visually hidden", card_form("hidden", 10, ", style: { position: 'absolute', overflow: 'hidden', clip: 'rect(0px, 0px, 0px, 0px)' }, clipTo: { x: 0, y: 0, w: 1, h: 1 }")),
        ("faint", card_form("hidden", 10, ", style: { opacity: '0.05' }")),
        ("off screen", card_form("hidden", 900, "")),
    ] {
        let page = [decoy, visible.clone()];
        assert_eq!(focused(&page, 1), CARD_FILLED, "{case}: the visible form after it, focused");
        assert_eq!(focused(&page, 0), "false ", "{case}: the decoy's own field focused");
    }
    let engine = card_page(&[card_form("first", 10, ""), card_form("second", 300, "")]);
    assert_eq!(fill_card(&engine, &CARD, true), "false ", "two visible card forms, none focused");
    assert_eq!(fill_card(&engine, &CARD, false), "false ", "a subframe with no card field focused");
    engine.run("__active = document.forms.item(1).__children[1]; 0", "t").unwrap();
    assert_eq!(fill_card(&engine, &CARD, true), CARD_FILLED.replace("cards-", "second-"), "the focused form only");
}

/// ASP-P16: an http page, or any other document that can't take a card,
/// gets the script without the card note (see `a_card_goes_only_to_a_secure_https_or_loopback_document`
/// in login_tests.rs). There a card fills nothing, whether a card field has
/// focus or not, in the main frame or not, and focusing one notes nothing.
#[test]
fn a_document_installed_without_the_card_note_never_takes_a_card() {
    for capture in [true, false] {
        let engine = installed_without_cards(capture).unwrap();
        engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}])); __active = undefined; 0", card_form("cards", 10, "")), "t").unwrap();
        assert_eq!(fill_card(&engine, &CARD, true), "false ", "capture {capture}, the main frame");
        let noted = engine.run("(() => { __calls = []; const event = new Event('focusin'); event.__target = document.forms.item(0).__children[1];
            __listeners.find((l) => l.type === 'focusin').listener(event); __active = event.__target; return JSON.stringify(__calls); })()", "t").unwrap();
        assert_eq!(noted, "[]", "capture {capture}: no card note");
        assert_eq!(fill_card(&engine, &CARD, true), "false ", "capture {capture}, a card field focused");
        assert_eq!(fill_card(&engine, &CARD, false), "false ", "capture {capture}, a subframe");
    }
}

/// ASP-P16: a payment processor's card frame, another site's, holds only
/// its own fields: here the number, with no form. Its script has only the
/// card note. It is filled only while its own field has focus, and then
/// only that field; focusing a login or address field there sends nothing.
#[test]
fn a_hosted_fields_frame_fills_only_its_own_field_and_only_with_focus() {
    let engine = installed_for_cards_only().unwrap();
    engine.run("mount(el('body', { rect: { x: 0, y: 0, w: 400, h: 60 } }, [
        el('input', { name: 'hosted-number', attrs: { type: 'tel', autocomplete: 'cc-number' }, rect: { x: 4, y: 4, w: 300, h: 30 } }),
        el('input', { name: 'hosted-csc', attrs: { autocomplete: 'cc-csc' }, rect: { x: 310, y: 4, w: 60, h: 30 } })]));
        __active = undefined; 0", "t").unwrap();
    assert_eq!(fill_card(&engine, &CARD, false), "false ", "not the focused frame");
    let focus = |field: &str| engine.run(&format!("(() => {{ __calls = []; const event = new Event('focusin'); event.__target = {field};
        __listeners.find((l) => l.type === 'focusin').listener(event); return JSON.stringify(__calls); }})()"), "t").unwrap();
    assert_eq!(focus("document.body.__children[0]"), r#"[["cardFocused"]]"#, "a content-free note");
    engine.run("__active = document.body.__children[0]; 0", "t").unwrap();
    assert_eq!(fill_card(&engine, &CARD, false), "true hosted-number=4242424242424242", "its own number field, and never the code");
    let login = installed_for_cards_only().unwrap();
    login.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}, {}])); 0", login_form("login", 100, ""), token_form("ship", 300, "")), "t").unwrap();
    let noted = login.run("(() => { __calls = []; for (const field of [document.forms.item(0).__children[1], document.forms.item(1).__children[5]]) {
        const event = new Event('focusin'); event.__target = field; __listeners.find((l) => l.type === 'focusin').listener(event); }
        return JSON.stringify(__calls); })()", "t").unwrap();
    assert_eq!(noted, "[]", "no login or address note from another site's frame");
}

/// ASP-P16: focusing a card field notes it once per form, content-free,
/// with capture on or off; a field that is no card field isn't noted.
#[test]
fn focusing_a_card_field_sends_a_content_free_note_once() {
    for capture in [true, false] {
        let engine = installed(capture).unwrap();
        engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}])); 0", card_form("cards", 10, "")), "t").unwrap();
        let focus = |field: usize| engine.run(&format!("(() => {{ __calls = []; const event = new Event('focusin');
            event.__target = document.forms.item(0).__children[{field}];
            __listeners.find((l) => l.type === 'focusin').listener(event); return JSON.stringify(__calls); }})()"), "t").unwrap();
        assert_eq!(focus(4), "[]", "an email field");
        assert_eq!(focus(1), r#"[["cardFocused"]]"#, "capture {capture}");
        assert_eq!(focus(2), "[]", "once per form");
        assert_eq!(focus(3), "[]", "the security code is in the same form");
    }
}

// -- Round 9: fills stay with the focused field (R8-3), and a submitted address takes only what the user saw (R8-1) --

/// A div at `y` holding `fields` (name, autocomplete) outside any form, 32 px apart.
fn loose_group(name: &str, y: u32, fields: &[(&str, &str)]) -> String {
    let rows: Vec<String> = fields.iter().enumerate().map(|(i, (field, auto))| {
        let kind = if *auto == "email" { "type: 'email', " } else { "" };
        format!("el('input', {{ name: '{name}-{field}', attrs: {{ {kind}autocomplete: '{auto}' }}, rect: {{ x: 110, y: {}, w: 300, h: 28 }} }})", y + 6 + 32 * i as u32)
    }).collect();
    format!("el('div', {{ name: '{name}', rect: {{ x: 100, y: {y}, w: 400, h: {} }} }}, [{}])", 12 + 32 * fields.len(), rows.join(", "))
}

/// An address fill in the main frame after `setup` (which mounts the page
/// and may set `__active`): the result and what every field holds.
fn address_after(setup: &str) -> String {
    let engine = installed(false).unwrap();
    engine.run(&format!("(() => {{ __active = undefined; {setup}; return 0; }})()"), "t").unwrap_or_else(|error| panic!("setup: {error}"));
    fill_address(&engine, &ADDRESS, true)
}

/// A login fill in the main frame after `setup`: the result and which fields received a value.
fn login_after(setup: &str) -> String {
    let engine = installed(true).unwrap();
    engine.run(&format!("(() => {{ __active = undefined; {setup}; return 0; }})()"), "t").unwrap_or_else(|error| panic!("setup: {error}"));
    engine.run("__log = []; String(fill('fixture-user', 'fixture-pass', true)) + ' ' + __log.filter((e) => e.startsWith('value:')).join(',')", "t").unwrap()
}

fn mount(children: &[String]) -> String {
    format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}]))", children.join(","))
}

/// R8-3, the eighth recheck's cases: with a field focused, an address fills
/// only that field's own form, or, outside any form, its nearest container
/// that holds another address field; never a form or group elsewhere, and
/// nothing when those fields aren't an address. A lone visible street
/// field is not an address, so the receipt's page gets nothing at all,
/// though a faint form slotted into a closed shadow root beside it passes
/// every check script outside that root can make. An ordinary visible
/// form, and a group of fields outside any form, still fill from a focused
/// field of their own.
#[test]
fn ported_r8_3_a_focused_field_fills_only_its_own_form_or_group() {
    let street_only = loose_group("shown", 500, &[("street", "street-address")]);
    let closed = in_shadow(&token_form("hidden", 10, ""), "opacity: '0.01'", false);
    let newsletter = address_form("news", 600, "", &[("input", "email", "type: 'email', autocomplete: 'email'", "", &[])]);
    let visible = token_form("tokens", 100, "");
    assert_eq!(address_after(&format!("{}; __active = document.body.__children[1].__children[0]", mount(&[closed.clone(), street_only.clone()]))), "false ",
        "the receipt's page: a visible street-only field focused, a faint form in a closed shadow root");
    assert_eq!(address_after(&format!("{}; __active = document.forms.item(1).__children[0]", mount(&[visible.clone(), newsletter.clone()]))), "false ",
        "a newsletter field focused, a visible address form elsewhere");
    assert_eq!(address_after(&format!("{}; __active = document.body.__children[1].__children[0]", mount(&[visible.clone(), street_only.clone()]))), "false ",
        "a street-only field focused, a visible address form elsewhere");
    assert_eq!(address_after(&format!("{}; __active = document.body.__children[0].__children[0]", mount(std::slice::from_ref(&street_only)))), "false ", "a lone street field");
    // Two groups outside any form: the focused field's own, never the other.
    let home = loose_group("home", 10, &[("street", "street-address"), ("postal", "postal-code")]);
    let other = loose_group("other", 300, &[("street", "street-address"), ("postal", "postal-code"), ("email", "email")]);
    assert_eq!(address_after(&format!("{}; __active = document.body.__children[0].__children[1]", mount(&[home.clone(), other.clone()]))),
        "true home-street=1 Test Street, Unit 2, Building 3|home-postal=90000", "only the focused field's group");
    // What still fills: from a focused field only (see
    // an_address_fill_with_nothing_focused_fills_nothing).
    assert_eq!(address_after(&format!("{}; __active = document.forms.item(0).__children[5]", mount(&[visible.clone(), newsletter]))), TOKEN_FILLED, "a visible form, focused");
    assert_eq!(address_after(&format!("{}; __active = document.body.__children[0].__children[0]", mount(std::slice::from_ref(&home)))), "true home-street=1 Test Street, Unit 2, Building 3|home-postal=90000", "a group outside any form, focused");
}

/// R8-3 for logins (WP2): a focused field outside a login form never sends
/// the login to a form elsewhere, visible or slotted faintly into a closed
/// shadow root, as the page-wide rule did. A focused login form, and the
/// one login form when nothing is focused, still fill.
#[test]
fn a_focused_field_outside_a_login_form_never_reaches_one_elsewhere() {
    let visible = login_form("real", 300, "");
    let newsletter = address_form("news", 600, "", &[("input", "email", "type: 'email', autocomplete: 'email'", "", &[])]);
    assert_eq!(login_after(&format!("{}; __active = document.forms.item(1).__children[0]", mount(&[visible.clone(), newsletter]))), "false ", "a newsletter field focused");
    let loose = "el('div', { name: 'loose', rect: { x: 100, y: 500, w: 400, h: 120 } }, [
        el('input', { name: 'loose-user', attrs: { type: 'email', autocomplete: 'username' }, rect: { x: 110, y: 510, w: 300, h: 30 } }),
        el('input', { name: 'loose-pass', attrs: { type: 'password' }, rect: { x: 110, y: 560, w: 300, h: 30 } })])".to_owned();
    let closed = in_shadow(&login_form("slotted", 100, ""), "opacity: '0.01'", false);
    assert_eq!(login_after(&format!("{}; __active = document.body.__children[1].__children[1]", mount(&[closed, loose]))), "false ",
        "a formless login focused, a faint form in a closed shadow root");
    assert_eq!(login_after(&format!("{}; __active = document.forms.item(0).__children[1]", mount(std::slice::from_ref(&visible)))), "true value:real-user,value:real-pass", "the focused login form");
    assert_eq!(login_after(&mount(&[visible])), "true value:real-user,value:real-pass", "the one login form, nothing focused");
}

/// R8-3: a field is filled only when the visibility walk reaches the
/// document through every ancestor it is drawn in. A walk that ends
/// anywhere else, here at a shadow root whose host the script can't read,
/// leaves ancestors unchecked, so a login with nothing focused gets
/// nothing (for a focused field, and for addresses, which need one, see
/// a_focused_field_whose_walk_ends_outside_the_document_is_refused).
#[test]
fn a_field_whose_ancestors_cant_all_be_checked_is_never_filled() {
    let slotted = |form: &str| format!("(() => {{ const form = {form}; const host = el('div', {{ name: 'host' }}, [form]); const slot = el('slot', {{}}, []);
        const root = shadow(host, [el('div', {{ name: 'wrapper' }}, [slot])]); root.__host = null; slotInto(form, slot); return host; }})()");
    assert_eq!(login_after(&mount(&[slotted(&login_form("cut", 100, ""))])), "false ", "a login");
    // The same form drawn in an open shadow root whose host is readable fills.
    assert_eq!(login_after(&mount(&[in_shadow(&login_form("open", 100, ""), "", true)])), "true value:open-user,value:open-pass");
}

/// R8-1, ported from the eighth recheck's probe: a submitted address takes
/// its parts only from fields the user can see, so a page can't plant an
/// organization, phone or email (display: none, holding its own values), or
/// a street ahead of the one the user typed. Hidden fields alone that make
/// an address send nothing.
#[test]
fn ported_r8_1_a_submitted_address_takes_only_fields_the_user_can_see() {
    let form = address_form("shop", 10, "", &[
        ("input", "hidden-street", "autocomplete: 'street-address'", "", &[]),
        ("input", "name", "autocomplete: 'name'", "", &[]),
        ("input", "street", "autocomplete: 'street-address'", "", &[]),
        ("input", "city", "autocomplete: 'address-level2'", "", &[]),
        ("input", "postal", "autocomplete: 'postal-code'", "", &[]),
        ("input", "org", "autocomplete: 'organization'", "", &[]),
        ("input", "phone", "type: 'tel', autocomplete: 'tel'", "", &[]),
        ("input", "email", "type: 'email', autocomplete: 'email'", "", &[]),
    ]);
    let engine = address_page(&[form], true);
    engine.run("(() => { const f = document.forms.item(0).__children;
        f[1].__value = 'Ada Fixture'; f[2].__value = '1 Fixture Street'; f[3].__value = 'Testville'; f[4].__value = '75001';
        for (const i of [0, 5, 6, 7]) f[i].__style = { display: 'none' };
        f[0].__value = '9 Planted Road'; f[5].__value = 'Planted Org'; f[6].__value = '+1 555 0199'; f[7].__value = 'planted@attacker.example'; return 0; })()", "t").unwrap();
    click_into(&engine, "document.forms.item(0).__children[1]");
    assert_eq!(submit_address(&engine, 0, true, true), r#"[["address","Ada Fixture","","1 Fixture Street","Testville","","75001","","",""]]"#);
    engine.run("(() => { const f = document.forms.item(0).__children; for (const i of [1, 2, 3, 4]) f[i].__style = { display: 'none' }; for (const i of [0, 5, 6, 7]) f[i].__style = {}; return 0; })()", "t").unwrap();
    assert_eq!(submit_address(&engine, 0, true, true), "[]", "the visible fields aren't an address");
}

/// Focus the first input, select or text area under the body, in document
/// order, as a user clicking into the page's first field.
fn focus_first(engine: Engine) -> Engine {
    engine.run("(() => { const first = (node) => { for (const child of node.__children) {
        if (child instanceof HTMLInputElement || child instanceof HTMLSelectElement || child instanceof HTMLTextAreaElement) return child;
        const found = first(child); if (found) return found; } return null; };
        __active = first(document.body); return __active ? 0 : 1; })()", "t").unwrap();
    engine
}

/// An address fill starts only from a focused field (team lead, round 9):
/// with nothing focused, the key button's fill reaches no address, not even
/// the page's one visible address form or group of fields outside any form,
/// in the main frame or a subframe. Focusing a field of either fills it.
#[test]
fn an_address_fill_with_nothing_focused_fills_nothing() {
    let visible = token_form("tokens", 100, "");
    let home = loose_group("home", 10, &[("street", "street-address"), ("postal", "postal-code")]);
    for (case, page) in [("one visible form", &visible), ("one group outside any form", &home)] {
        for main in [true, false] {
            assert_eq!(fill_address(&address_page(std::slice::from_ref(page), false), &ADDRESS, main), "false ", "{case}, main frame {main}");
        }
    }
    assert_eq!(fill_address(&focus_first(address_page(std::slice::from_ref(&visible), false)), &ADDRESS, true), TOKEN_FILLED, "focused: its form");
    assert_eq!(fill_address(&focus_first(address_page(std::slice::from_ref(&home), false)), &ADDRESS, false), "true home-street=1 Test Street, Unit 2, Building 3|home-postal=90000", "focused: its group, in a subframe too");
}

/// A focused field whose visibility walk ends before the document, here at
/// a shadow root whose host the script can't read, is refused: neither the
/// address nor the login reaches its form. The same forms in an open shadow
/// root with a readable host fill.
#[test]
fn a_focused_field_whose_walk_ends_outside_the_document_is_refused() {
    let cut = |form: &str| format!("(() => {{ const form = {form}; const host = el('div', {{ name: 'host' }}, [form]); const slot = el('slot', {{}}, []);
        const root = shadow(host, [el('div', {{ name: 'wrapper' }}, [slot])]); root.__host = null; slotInto(form, slot); return host; }})()");
    let focus_in = "__active = document.forms.item(0).__children[1]";
    assert_eq!(address_after(&format!("{}; {focus_in}", mount(&[cut(&token_form("cut", 10, ""))]))), "false ", "an address");
    assert_eq!(login_after(&format!("{}; {focus_in}", mount(&[cut(&login_form("cut", 100, ""))]))), "false ", "a login");
    assert_eq!(address_after(&format!("{}; {focus_in}", mount(&[in_shadow(&token_form("tokens", 10, ""), "", true)]))), TOKEN_FILLED, "an address, host readable");
    assert_eq!(login_after(&format!("{}; {focus_in}", mount(&[in_shadow(&login_form("open", 100, ""), "", true)]))), "true value:open-user,value:open-pass", "a login, host readable");
}

// -- payment cards on round 9's fill scope --------------------------------------------------------

/// A card fill in the main frame after `setup` (which mounts the page and
/// may set `__active`): the result and what every field holds.
fn card_after(setup: &str) -> String {
    let engine = installed(false).unwrap();
    engine.run(&format!("(() => {{ __active = undefined; {setup}; return 0; }})()"), "t").unwrap_or_else(|error| panic!("setup: {error}"));
    fill_card(&engine, &CARD, true)
}

/// A div at `y` holding a card number and a combined expiry outside any form.
fn loose_card(name: &str, y: u32) -> String {
    format!("el('div', {{ name: '{name}', rect: {{ x: 100, y: {y}, w: 400, h: 76 }} }}, [
        el('input', {{ name: '{name}-number', attrs: {{ type: 'tel', autocomplete: 'cc-number' }}, rect: {{ x: 110, y: {}, w: 300, h: 28 }} }}),
        el('input', {{ name: '{name}-exp', attrs: {{ autocomplete: 'cc-exp' }}, rect: {{ x: 110, y: {}, w: 300, h: 28 }} }})])", y + 6, y + 38)
}

/// Cards follow round 9's rule for addresses (team lead, after the round 9
/// merge): a card fill starts only from a focused field, so with nothing
/// focused the key button's fill reaches no card field, not even the page's
/// one visible card form, in the main frame or a subframe.
#[test]
fn a_card_fill_with_nothing_focused_fills_nothing() {
    let visible = card_form("cards", 10, "");
    for main in [true, false] {
        assert_eq!(fill_card(&card_page(std::slice::from_ref(&visible)), &CARD, main), "false ", "main frame {main}");
    }
    assert_eq!(fill_card(&focus_first(card_page(std::slice::from_ref(&visible))), &CARD, true), CARD_FILLED, "focused: its form");
}

/// R8-3 for cards: a focused field fills only its own form or, outside any
/// form, its nearest container that holds another card field, never a form
/// or group elsewhere; a field with no form and no such container, as in a
/// payment processor's frame, fills only itself. A focused field outside
/// any card form reaches nothing.
#[test]
fn a_focused_card_field_fills_only_its_own_form_or_group() {
    let newsletter = address_form("news", 600, "", &[("input", "email", "type: 'email', autocomplete: 'email'", "", &[])]);
    let visible = card_form("cards", 100, "");
    assert_eq!(card_after(&format!("{}; __active = document.forms.item(1).__children[0]", mount(&[visible, newsletter]))), "false ", "a newsletter field focused, a visible card form elsewhere");
    assert_eq!(card_after(&format!("{}; __active = document.body.__children[0].__children[1]", mount(&[loose_card("home", 10), loose_card("other", 300)]))),
        "true home-number=4242424242424242|home-exp=07/34", "only the focused field's group");
    let lone = "el('input', { name: 'lone-number', attrs: { type: 'tel', autocomplete: 'cc-number' }, rect: { x: 110, y: 500, w: 300, h: 28 } })".to_owned();
    let closed = in_shadow(&card_form("hidden", 10, ""), "opacity: '0.01'", false);
    assert_eq!(card_after(&format!("{}; __active = document.body.__children[1]", mount(&[closed, lone.clone()]))), "true lone-number=4242424242424242",
        "a lone number fills only itself, never a faint form in a closed shadow root");
    assert_eq!(card_after(&format!("{}; __active = document.body.__children[1]", mount(&[loose_card("other", 10), lone]))), "true lone-number=4242424242424242",
        "the body is no group: a lone number beside another group fills only itself");
}

/// A focused card field whose visibility walk ends before the document is
/// refused; the same form in an open shadow root with a readable host fills.
#[test]
fn a_focused_card_field_whose_walk_ends_outside_the_document_is_refused() {
    let cut = format!("(() => {{ const form = {}; const host = el('div', {{ name: 'host' }}, [form]); const slot = el('slot', {{}}, []);
        const root = shadow(host, [el('div', {{ name: 'wrapper' }}, [slot])]); root.__host = null; slotInto(form, slot); return host; }})()", card_form("cut", 10, ""));
    assert_eq!(card_after(&format!("{}; __active = document.forms.item(0).__children[1]", mount(&[cut]))), "false ");
    assert_eq!(card_after(&format!("{}; __active = document.forms.item(0).__children[1]", mount(&[in_shadow(&card_form("cards", 10, ""), "", true)]))), CARD_FILLED, "host readable");
}

// -- Round 10: the engine's own visibility answer (IntersectionObserver v2) for addresses and cards --
// The stub's observer is a model of what Chromium is documented to answer;
// a native run in Crossbow is still needed (see app/tests/fixtures/login/visibility.html).

/// An input at `y` (300×28 at x 110) with `attrs`.
fn inp(name: &str, attrs: &str, y: u32) -> String {
    format!("el('input', {{ name: '{name}', attrs: {{ {attrs} }}, rect: {{ x: 110, y: {y}, w: 300, h: 28 }} }})")
}

/// `inner` (stub expressions) as the light-DOM children of a host whose
/// shadow root draws them in a wrapper styled `wrapper`: closed unless
/// `open`, so script outside it reads no slot and no wrapper.
fn slotted(inner: &[String], wrapper: &str, open: bool) -> String {
    format!("(() => {{ const kids = [{}]; const host = el('div', {{ name: 'host', rect: {{ x: 100, y: 300, w: 400, h: 200 }} }}, kids); const slot = el('slot', {{}}, []);
        shadow(host, [el('div', {{ name: 'wrapper', style: {{ {wrapper} }} }}, [slot])]); for (const k of kids) {{ k.__drawnIn = slot; {} }} return host; }})()",
        inner.join(", "), if open { "slotInto(k, slot);" } else { "" })
}

/// Recheck 9's planted fields: a name, organization, postal code, phone and email.
fn planted(prefix: &str) -> Vec<String> {
    vec![
        inp(&format!("{prefix}-name"), "autocomplete: 'name'", 310),
        inp(&format!("{prefix}-org"), "autocomplete: 'organization'", 342),
        inp(&format!("{prefix}-postal"), "autocomplete: 'postal-code'", 374),
        inp(&format!("{prefix}-phone"), "type: 'tel', autocomplete: 'tel'", 406),
        inp(&format!("{prefix}-email"), "type: 'email', autocomplete: 'email'", 438),
    ]
}

/// What the fills reported to the renderer: each address or card fill's
/// outcome, fields filled and fields left empty as not clearly visible.
fn reports(engine: &Engine) -> String {
    engine.run("JSON.stringify(__calls.filter((call) => call[0] === 'addressFilled' || call[0] === 'cardFilled'))", "t").unwrap()
}

/// An address fill in the main frame after `setup`, as `address_after`: the
/// result and what every field holds, then what the fill reported.
fn address_report(setup: &str) -> (String, String) {
    let engine = installed(false).unwrap();
    engine.run(&format!("(() => {{ __active = undefined; __calls = []; {setup}; return 0; }})()"), "t").unwrap_or_else(|error| panic!("setup: {error}"));
    let held = fill_address(&engine, &ADDRESS, true);
    (held, reports(&engine))
}

/// The same for a card.
fn card_report(engine: Engine, main: bool) -> (String, String) {
    engine.run("__calls = []; 0", "t").unwrap();
    let held = fill_card(&engine, &CARD, main);
    (held, reports(&engine))
}

const STREET: &str = "1 Test Street, Unit 2, Building 3";

/// The stub's observer answers as Chromium is documented to (an instrument
/// check, not a test of login.js): a field drawn in a closed shadow root
/// under a faint or filtered wrapper, rotated, squashed, covered by
/// anything rendered after it, outside the viewport, or in a frame its
/// parent covers, is not visible; a translated or proportionally scaled
/// one is.
#[test]
fn the_stub_observer_answers_from_what_layout_draws() {
    let answer = |field: &str, setup: &str| {
        let engine = installed(false).unwrap();
        engine.run(&format!("(() => {{ {setup}; return 0; }})()"), "t").unwrap();
        engine.run(&format!("String(__engineVisible({field}))"), "t").unwrap()
    };
    let first = "document.body.__children[0].__children[0]";
    let plain = mount(&[format!("el('div', {{}}, [{}])", inp("plain", "", 16))]);
    assert_eq!(answer(first, &plain), "true");
    for (case, wrapper, open) in [("faint", "opacity: '0.01'", false), ("filtered", "filter: 'opacity(0)'", false), ("faint, open root", "opacity: '0.99'", true)] {
        assert_eq!(answer("document.body.__children[0].__children[0]", &mount(&[slotted(&[inp("hid", "", 310)], wrapper, open)])), "false", "{case}");
    }
    assert_eq!(answer("document.body.__children[0].__children[0]", &mount(&[slotted(&[inp("shown", "", 310)], "", false)])), "true", "a plain closed root");
    for (transform, visible) in [("rotate(1deg)", "false"), ("scale(1, 0.1)", "false"), ("matrix(1, 0, 0, 1, 0, 0)", "false"), ("translate(4px, 4px)", "true"), ("scale(1.5)", "true")] {
        let page = mount(&[format!("el('div', {{ style: {{ transform: '{transform}' }} }}, [{}])", inp("t", "", 16))]);
        assert_eq!(answer(first, &page), visible, "{transform}");
    }
    let icon = mount(&[format!("el('div', {{}}, [{}, el('span', {{ rect: {{ x: 380, y: 20, w: 20, h: 20 }} }}, [])])", inp("covered", "", 16))]);
    assert_eq!(answer(first, &icon), "false", "an icon over its corner");
    let clear_icon = mount(&[format!("el('div', {{}}, [{}, el('span', {{ style: {{ opacity: '0' }}, rect: {{ x: 380, y: 20, w: 20, h: 20 }} }}, [])])", inp("covered", "", 16))]);
    assert_eq!(answer(first, &clear_icon), "true", "a fully transparent icon");
    assert_eq!(answer(first, &mount(&[format!("el('div', {{}}, [{}])", inp("low", "", 900))])), "false", "below the viewport");
    assert_eq!(answer(first, &format!("{plain}; __frameClear = false")), "false", "its frame covered in the parent page");
}

/// R9-4, the open part of R8-3 from recheck 9: beside a visible street field
/// the user focuses, a page slots a name, organization, postal code, phone
/// and email into a closed shadow root under a near-transparent or filtered
/// wrapper, in the same plain container or form. Script outside the root
/// can't see the wrapper; the engine's answer does. Each of those fields is
/// left empty on its own, and a lone street field is no address, so
/// nothing is filled and the fill says why: 0 filled, 5 not clearly visible.
/// With a visible city too, the street and city fill.
#[test]
fn ported_r9_4_closed_shadow_fields_beside_a_focused_bait_field_get_no_address_part() {
    let bait = inp("bait-street", "autocomplete: 'street-address'", 16);
    let city = inp("city", "autocomplete: 'address-level2'", 48);
    for (case, wrapper) in [("opacity 0.01", "opacity: '0.01'"), ("filter: opacity(0)", "filter: 'opacity(0)'")] {
        for (scope, tag, focus) in [("a plain container", "div", "document.body.__children[0].__children[0]"), ("a form", "form", "document.forms.item(0).__children[0]")] {
            let page = format!("el('{tag}', {{ name: 'box', rect: {{ x: 100, y: 10, w: 400, h: 500 }} }}, [{bait}, {}])", slotted(&planted("hid"), wrapper, false));
            let (held, report) = address_report(&format!("{}; __active = {focus}", mount(&[page])));
            assert_eq!(held, "true ", "{case}, {scope}: no field gets a part");
            assert_eq!(report, r#"[["addressFilled","done","0","5"]]"#, "{case}, {scope}: and the fill says why");
            let page = format!("el('{tag}', {{ name: 'box', rect: {{ x: 100, y: 10, w: 400, h: 500 }} }}, [{bait}, {city}, {}])", slotted(&planted("hid"), wrapper, false));
            let (held, report) = address_report(&format!("{}; __active = {focus}", mount(&[page])));
            assert_eq!(held, format!("true bait-street={STREET}|city=Testville"), "{case}, {scope}, with a visible city");
            assert_eq!(report, r#"[["addressFilled","done","2","5"]]"#, "{case}, {scope}, with a visible city");
        }
    }
}

/// R9-4, the open part of R8-1 from recheck 9: a submitted address reads only
/// fields the engine last said the user can see, so planted fields in a
/// faint closed-root wrapper inside the form supply nothing, without the
/// submit waiting. A field the engine never answered for, as before the
/// user clicked into the form, is never read either.
#[test]
fn ported_r9_4_a_submitted_address_never_reads_closed_shadow_fields() {
    let page = || {
        let engine = installed(true).unwrap();
        let form = format!("el('form', {{ name: 'shop', rect: {{ x: 100, y: 10, w: 400, h: 600 }} }}, [{}, {}, {}, {}, {}])",
            inp("name", "autocomplete: 'name'", 16), inp("street", "autocomplete: 'street-address'", 48), inp("city", "autocomplete: 'address-level2'", 80), inp("postal", "autocomplete: 'postal-code'", 112),
            slotted(&[inp("org", "autocomplete: 'organization'", 310), inp("phone", "type: 'tel', autocomplete: 'tel'", 342), inp("email", "type: 'email', autocomplete: 'email'", 374)], "opacity: '0.01'", false));
        engine.run(&format!("{}; 0", mount(&[form])), "t").unwrap();
        engine.run("(() => { const f = document.forms.item(0).__children; f[0].__value = 'Ada Fixture'; f[1].__value = '1 Fixture Street'; f[2].__value = 'Testville'; f[3].__value = '75001';
            const h = f[4].__children; h[0].__value = 'Planted Org'; h[1].__value = '+1 555 0199'; h[2].__value = 'planted@attacker.example'; return 0; })()", "t").unwrap();
        engine
    };
    let engine = page();
    click_into(&engine, "document.forms.item(0).__children[1]");
    assert_eq!(submit_address(&engine, 0, true, true), r#"[["address","Ada Fixture","","1 Fixture Street","Testville","","75001","","",""]]"#);
    assert_eq!(submit_address(&page(), 0, true, true), "[]", "no field has an answer yet: nothing is read, so nothing is offered");
}

/// Round 10, per field: a fill leaves out each field the engine doesn't
/// call clearly visible and fills the rest, then reports how many of each;
/// one whose focused field isn't clearly visible fills nothing.
#[test]
fn an_address_fill_leaves_out_each_field_the_engine_cant_see_and_says_how_many() {
    let form = |street_hidden: bool| {
        let street = inp("street", "autocomplete: 'street-address'", 48);
        let (shown, hidden) = if street_hidden { (String::new(), format!("{street}, ")) } else { (format!("{street}, "), String::new()) };
        format!("el('form', {{ name: 'shop', rect: {{ x: 100, y: 10, w: 400, h: 600 }} }}, [{}, {shown}{}, {}, {}])",
            inp("name", "autocomplete: 'name'", 16), inp("city", "autocomplete: 'address-level2'", 80), inp("postal", "autocomplete: 'postal-code'", 112),
            slotted(&[format!("{hidden}{}", inp("org", "autocomplete: 'organization'", 310)), inp("phone", "type: 'tel', autocomplete: 'tel'", 342)], "opacity: '0.4'", false))
    };
    let (held, report) = address_report(&format!("{}; __active = document.forms.item(0).__children[0]", mount(&[form(false)])));
    assert_eq!(held, format!("true name=Fixture Person|street={STREET}|city=Testville|postal=90000"));
    assert_eq!(report, r#"[["addressFilled","done","4","2"]]"#, "4 of 6 filled, 2 not clearly visible");
    let (held, report) = address_report(&format!("{}; __active = document.forms.item(0).__children[3].__children[0]", mount(&[form(true)])));
    assert_eq!(held, "true ", "the focused street isn't clearly visible: nothing at all");
    assert_eq!(report, r#"[["addressFilled","focus-hidden","0","0"]]"#);
    let (held, report) = address_report(&format!("{}; __active = document.forms.item(0).__children[0]", mount(&[token_form("tokens", 10, "")])));
    assert_eq!(held, TOKEN_FILLED, "an ordinary visible form fills as before");
    assert_eq!(report, r#"[["addressFilled","done","10","0"]]"#);
}

/// Round 10: without the engine's answer, nothing fills. A browser without
/// IntersectionObserver, or with only its first version (no `isVisible`),
/// fills nothing and says it couldn't confirm the fields; so does a page
/// that gives no answer within the 1 s the fill waits, which reports then
/// and not before. The fill never uses an answer from before it was asked:
/// a form the user saw, clicked into and filled once, then faded, gets
/// nothing, though the engine's last answer, kept since the click (capture
/// on) or since the first fill, said it was visible.
#[test]
fn an_address_fill_without_the_engines_answer_in_time_fills_nothing() {
    let visible = token_form("tokens", 10, "");
    let focus = "__active = document.forms.item(0).__children[0]";
    for (case, prelude) in [("no IntersectionObserver", "delete globalThis.IntersectionObserver"), ("no isVisible", "delete IntersectionObserverEntry.prototype.isVisible")] {
        let engine = after_page(prelude, "undefined, undefined, focused, undefined, addressFocused, cardFocused, addressFilled, cardFilled").unwrap();
        engine.run(&format!("{}; {focus}; __calls = []; 0", mount(std::slice::from_ref(&visible))), "t").unwrap();
        assert_eq!(fill_address(&engine, &ADDRESS, true), "true ", "{case}");
        assert_eq!(reports(&engine), r#"[["addressFilled","unconfirmed","0","0"]]"#, "{case}");
    }
    let engine = installed(false).unwrap();
    engine.run(&format!("{}; {focus}; __calls = []; __rendering = false; 0", mount(std::slice::from_ref(&visible))), "t").unwrap();
    let values: Vec<String> = super::address_values(&ADDRESS).iter().map(|value| js(value.expose())).collect();
    assert_eq!(engine.run(&format!("String(fillAddress(true, {})) + ' ' + (__advance(984), held())", values.join(", ")), "t").unwrap(), "true ");
    assert_eq!(reports(&engine), "[]", "still waiting at 984 ms");
    engine.run("__advance(48); 0", "t").unwrap();
    assert_eq!(reports(&engine), r#"[["addressFilled","unconfirmed","0","0"]]"#, "no answer by 1 s");
    assert_eq!(engine.run("__rendering = true; __advance(200); held()", "t").unwrap(), "", "a late answer fills nothing");
    for capture in [true, false] {
        let engine = installed(capture).unwrap();
        engine.run(&format!("{}; 0", mount(&[in_shadow(&token_form("tokens", 10, ""), "", false)])), "t").unwrap();
        click_into(&engine, "document.forms.item(0).__children[0]");
        assert_eq!(fill_address(&engine, &ADDRESS, true), TOKEN_FILLED, "capture {capture}: seen and filled");
        engine.run("document.body.__children[0].__children[0].__drawnIn.__parent.__style = { opacity: '0.01' }; __calls = []; 0", "t").unwrap();
        fill_address(&engine, &ADDRESS, true);
        assert_eq!(engine.run("__log.filter((e) => e.startsWith('value:')).join(',')", "t").unwrap(), "", "capture {capture}: faded since the engine last answered");
        assert_eq!(reports(&engine), r#"[["addressFilled","focus-hidden","0","0"]]"#, "capture {capture}");
    }
}

/// What the engine's rule costs, as specified (a native run measures it):
/// an icon over one field, a label floating inside an unfocused field's box,
/// and a form still fading in all count as not clearly visible. The icon's
/// field is left empty and the rest fill; a focused field mid-fade fills
/// nothing until the fade ends.
#[test]
fn an_icon_a_floating_label_or_a_fade_in_leaves_fields_empty() {
    let icon = format!("el('form', {{ name: 'shop', rect: {{ x: 100, y: 10, w: 400, h: 200 }} }}, [{}, {}, el('span', {{ name: 'icon', rect: {{ x: 380, y: 52, w: 20, h: 20 }} }}, []), {}])",
        inp("street", "autocomplete: 'street-address'", 16), inp("city", "autocomplete: 'address-level2'", 48), inp("postal", "autocomplete: 'postal-code'", 80));
    let (held, report) = address_report(&format!("{}; __active = document.forms.item(0).__children[0]", mount(&[icon])));
    assert_eq!(held, format!("true street={STREET}|postal=90000"), "the city under the icon is left empty");
    assert_eq!(report, r#"[["addressFilled","done","2","1"]]"#);
    let label = format!("el('form', {{ name: 'shop', rect: {{ x: 100, y: 10, w: 400, h: 200 }} }}, [{}, {}, el('label', {{ name: 'floating', rect: {{ x: 114, y: 54, w: 60, h: 16 }} }}, []), {}])",
        inp("street", "autocomplete: 'street-address'", 16), inp("city", "autocomplete: 'address-level2'", 48), inp("postal", "autocomplete: 'postal-code'", 80));
    assert_eq!(address_report(&format!("{}; __active = document.forms.item(0).__children[0]", mount(&[label]))).1, r#"[["addressFilled","done","2","1"]]"#, "a floating label inside the city's box");
    let modal = |opacity: &str| format!("el('div', {{ name: 'modal', style: {{ opacity: '{opacity}' }}, rect: {{ x: 0, y: 0, w: 1280, h: 800 }} }}, [{}])", token_form("tokens", 10, ""));
    let (held, report) = address_report(&format!("{}; __active = document.forms.item(0).__children[0]", mount(&[modal("0.6")])));
    assert_eq!((held.as_str(), report.as_str()), ("true ", r#"[["addressFilled","focus-hidden","0","0"]]"#), "mid-fade");
    assert_eq!(address_report(&format!("{}; __active = document.forms.item(0).__children[0]", mount(&[modal("1")]))).0, TOKEN_FILLED, "faded in");
}

/// Round 10 for cards: the same per-field rule. A cardholder field slotted
/// into a faint closed-root wrapper is left empty and the number and expiry
/// fill; a focused number that isn't clearly visible fills nothing.
#[test]
fn a_card_fill_leaves_out_each_field_the_engine_cant_see() {
    let form = |number_hidden: bool| {
        let number = inp("number", "type: 'tel', autocomplete: 'cc-number'", 48);
        let (shown, hidden) = if number_hidden { (String::new(), format!(", {number}")) } else { (format!("{number}, "), String::new()) };
        format!("el('form', {{ name: 'pay', rect: {{ x: 100, y: 10, w: 400, h: 600 }} }}, [{shown}{}, {}])",
            inp("exp", "autocomplete: 'cc-exp', placeholder: 'MM / YY'", 80), slotted(&[format!("{}{hidden}", inp("holder", "autocomplete: 'cc-name'", 310))], "opacity: '0.01'", false))
    };
    let engine = card_page(&[form(false)]);
    engine.run("__active = document.forms.item(0).__children[0]; 0", "t").unwrap();
    assert_eq!(card_report(engine, true), ("true number=4242424242424242|exp=07 / 34".to_owned(), r#"[["cardFilled","done","2","1"]]"#.to_owned()));
    let engine = card_page(&[form(true)]);
    engine.run("__active = document.forms.item(0).__children[1].__children[1]; 0", "t").unwrap();
    assert_eq!(card_report(engine, true), ("true ".to_owned(), r#"[["cardFilled","focus-hidden","0","0"]]"#.to_owned()), "the focused number isn't clearly visible");
    let engine = after_page("delete globalThis.IntersectionObserver", "undefined, undefined, focused, undefined, addressFocused, cardFocused, addressFilled, cardFilled").unwrap();
    engine.run(&format!("{}; __active = document.forms.item(0).__children[0]; 0", mount(&[card_form("cards", 10, "")])), "t").unwrap();
    assert_eq!(card_report(engine, true), ("true ".to_owned(), r#"[["cardFilled","unconfirmed","0","0"]]"#.to_owned()), "no IntersectionObserver");
}

/// Round 10 for a payment processor's frame of another site: the engine's
/// answer in that frame covers the frame itself in the page that embeds it,
/// so the same number field fills while the frame is visible and not while
/// the page covers or fades it.
#[test]
fn a_cross_site_payment_frame_fills_only_while_the_engine_sees_it() {
    let frame = |clear: bool| {
        let engine = installed_for_cards_only().unwrap();
        engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 400, h: 60 }} }}, [
            el('input', {{ name: 'hosted-number', attrs: {{ type: 'tel', autocomplete: 'cc-number' }}, rect: {{ x: 4, y: 4, w: 300, h: 30 }} }})]));
            __active = document.body.__children[0]; __frameClear = {clear}; 0"), "t").unwrap();
        card_report(engine, false)
    };
    assert_eq!(frame(true), ("true hosted-number=4242424242424242".to_owned(), r#"[["cardFilled","done","1","0"]]"#.to_owned()), "visible");
    assert_eq!(frame(false), ("true ".to_owned(), r#"[["cardFilled","focus-hidden","0","0"]]"#.to_owned()), "covered in the parent page");
}

/// C10-2: one statement of a card fill's scope, the same in a payment
/// processor's frame of another site as anywhere: the focused field's own
/// form; outside any form, its nearest container below the body holding
/// another card field; else that field alone; never beyond its frame,
/// whose document is its own. The fixture's payment frame groups its number
/// and expiry, so fixture V7 rightly expects both to fill; here that frame,
/// as the stand-in models it, fills both from its focused number.
#[test]
fn the_fixture_payment_frame_fills_its_number_and_expiry_from_the_number() {
    let payment = include_str!("../../../app/tests/fixtures/login/payment.html");
    let at = |needle: &str| payment.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("<body>") < at("<div class=\"card-fields\">") && at("<div class=\"card-fields\">") < at("id=\"number\"") && at("id=\"exp\"") < at("</div>") && at("</div>") < at("</body>"),
        "the number and expiry share one group below the body");
    let visibility = include_str!("../../../app/tests/fixtures/login/visibility.html");
    assert!(visibility.contains("Visible frame: click its card number, choose Fill Card: the number and expiry fill, with no toast."));
    let engine = installed_for_cards_only().unwrap();
    engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 400, h: 120 }} }}, [el('div', {{ name: 'card-fields', rect: {{ x: 12, y: 8, w: 320, h: 100 }} }}, [{}, {}])]));
        __active = document.body.__children[0].__children[0]; 0", inp("payment-number", "type: 'tel', autocomplete: 'cc-number'", 20), inp("payment-exp", "autocomplete: 'cc-exp', placeholder: 'MM / YY'", 70)), "t").unwrap();
    assert_eq!(card_report(engine, false), ("true payment-number=4242424242424242|payment-exp=07 / 34".to_owned(), r#"[["cardFilled","done","2","0"]]"#.to_owned()));
}

/// C10-1: a card form removed after the card menu opened is never filled.
/// The browser can't see that change, so the card still goes to the frame
/// and document the menu was opened on; there, with the focused field gone,
/// focus is on the body (the stub sets that, as Chromium does), so the fill
/// finds no card field and says nothing was filled. Removed while the fill
/// waits for the engine's answer, it reports nothing filled. Either way no
/// field, in the document or out of it, takes a value.
#[test]
fn a_card_form_removed_after_the_menu_opened_is_never_filled() {
    let mounted = || {
        let engine = installed_for_cards_only().unwrap();
        engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 400, h: 100 }} }}, [{}])); __active = document.body.__children[0].__children[0]; 0", loose_card("hosted", 4)), "t").unwrap();
        engine
    };
    let remove = "const group = document.body.__children[0]; document.body.__children = []; group.__parent = null; __active = undefined;";
    let values: Vec<String> = super::card_values(&CARD).iter().map(|value| js(value.expose())).collect();
    let fill = |engine: &Engine, removed_first: bool| engine.run(&format!("__log = []; __calls = []; (() => {{ {} const result = fillCard(false, {}); {} __advance(1100);
        return String(result) + ' ' + __log.filter((entry) => entry.startsWith('value:')).join(','); }})()", if removed_first { remove } else { "" }, values.join(", "), if removed_first { "" } else { remove }), "t").unwrap();
    let engine = mounted();
    assert_eq!(fill(&engine, true), "false ", "removed before the fill arrives: nothing written, and the renderer says nothing was filled");
    assert_eq!(reports(&engine), "[]");
    let engine = mounted();
    assert_eq!(fill(&engine, false), "true ", "removed while the fill waits: nothing written");
    assert_eq!(reports(&engine), r#"[["cardFilled","done","0","0"]]"#);
    let engine = mounted();
    assert_eq!(engine.run(&format!("__log = []; __calls = []; String(fillCard(false, {})) + (__advance(1100), ' ') + __log.filter((entry) => entry.startsWith('value:')).join(',')", values.join(", ")), "t").unwrap(),
        "true value:hosted-number,value:hosted-exp", "still there: both fields fill");
}

/// Logins keep no engine gate (team lead, round 10): a password field with a
/// reveal icon over it fills, and so does a login in a browser without
/// IntersectionObserver, at once, with nothing waiting.
#[test]
fn a_login_fill_keeps_no_engine_gate() {
    let reveal = "el('form', { name: 'login', rect: { x: 100, y: 100, w: 400, h: 120 } }, [
        el('input', { name: 'login-user', attrs: { type: 'email', autocomplete: 'username' }, rect: { x: 110, y: 110, w: 300, h: 30 } }),
        el('input', { name: 'login-pass', attrs: { type: 'password' }, rect: { x: 110, y: 160, w: 300, h: 30 } }),
        el('button', { name: 'reveal', rect: { x: 384, y: 165, w: 20, h: 20 } }, [])])".to_owned();
    assert_eq!(login_after(&format!("{}; __active = document.forms.item(0).__children[1]", mount(&[reveal]))), "true value:login-user,value:login-pass");
    let engine = after_page("delete globalThis.IntersectionObserver", "capture, followed, focused, addressCapture, addressFocused, cardFocused, addressFilled, cardFilled").unwrap();
    engine.run(&format!("{}; 0", mount(&[login_form("login", 100, "")])), "t").unwrap();
    assert_eq!(engine.run("__log = []; String(fill('fixture-user', 'fixture-pass', true)) + ' ' + __log.filter((e) => e.startsWith('value:')).join(',')", "t").unwrap(),
        "true value:login-user,value:login-pass");
}

// -- Round 13: a frozen fill keeps its form or group (C11-1), and notes stay current (C11-2) --

const CARD_NOTE: &str = r#"[["cardFocused"]]"#;
const ADDRESS_NOTE: &str = r#"[["addressFocused"]]"#;

/// A focusin on `field` (a stub expression), as the user's click or the
/// page's script gives it: the card and address notes login.js sent.
fn notes_on_focus(engine: &Engine, field: &str) -> String {
    engine.run(&format!("(() => {{ __calls = []; __active = {field}; const event = new Event('focusin'); event.__target = {field};
        __listeners.find((l) => l.type === 'focusin').listener(event); return JSON.stringify(__calls.filter((c) => c[0] === 'cardFocused' || c[0] === 'addressFocused')); }})()"), "t").unwrap()
}

/// Recheck 11's second card form: a coupon field, then its own number and expiry.
fn other_card_form() -> String {
    "el('form', { name: 'other', rect: { x: 100, y: 300, w: 400, h: 140 } }, [
        el('input', { name: 'other-coupon', attrs: { type: 'text', autocomplete: 'off', placeholder: 'Coupon' }, rect: { x: 110, y: 306, w: 300, h: 28 } }),
        el('input', { name: 'other-number', attrs: { type: 'tel', autocomplete: 'cc-number' }, rect: { x: 110, y: 338, w: 300, h: 28 } }),
        el('input', { name: 'other-exp', attrs: { autocomplete: 'cc-exp' }, rect: { x: 110, y: 370, w: 300, h: 28 } })])".to_owned()
}

/// C11-1, recheck 11's case: with a card menu frozen on one card form, page
/// script moving focus to a field of another form that holds card fields,
/// as its coupon field, now sends a card note, in the page and in a
/// cards-only frame. The browser and the renderer compare note numbers, so
/// a fill frozen on the earlier note is void instead of filling the form
/// focus moved to. Once a card field had focus, so does focus moving to
/// anything that reaches no card field, such as a button, and coming back;
/// a field of the same form, or the same field focused again, as when the
/// window regains focus, sends none. Before any card field had focus, a
/// coupon field sends none.
#[test]
fn focus_leaving_a_card_form_or_group_sends_a_card_note() {
    let button = "el('button', { name: 'pay-button', rect: { x: 100, y: 600, w: 120, h: 32 } }, [])".to_owned();
    for (main, engine) in [(true, installed(false).unwrap()), (false, installed_for_cards_only().unwrap())] {
        engine.run(&format!("{}; 0", mount(&[card_form("cards", 10, ""), other_card_form(), button.clone()])), "t").unwrap();
        let (number, email) = ("document.forms.item(0).__children[1]", "document.forms.item(0).__children[4]");
        let (coupon, other_number) = ("document.forms.item(1).__children[0]", "document.forms.item(1).__children[1]");
        let button = "document.body.__children[2]";
        assert_eq!(notes_on_focus(&engine, coupon), "[]", "no card field had focus yet, main {main}");
        assert_eq!(notes_on_focus(&engine, number), CARD_NOTE, "main {main}");
        assert_eq!(notes_on_focus(&engine, email), "[]", "the same form's email: a fill still reaches this form, main {main}");
        assert_eq!(notes_on_focus(&engine, coupon), CARD_NOTE, "another card form's coupon field, main {main}");
        assert_eq!(notes_on_focus(&engine, other_number), "[]", "the same other form, main {main}");
        assert_eq!(notes_on_focus(&engine, button), CARD_NOTE, "a button reaches no card field, main {main}");
        assert_eq!(notes_on_focus(&engine, button), "[]", "focused again, main {main}");
        assert_eq!(notes_on_focus(&engine, number), CARD_NOTE, "back in the first form, main {main}");
        assert_eq!(notes_on_focus(&engine, number), "[]", "focused again, as when the window regains focus, main {main}");
    }
}

/// C11-1 for addresses, recheck 11's case: shipping then billing, two
/// address forms of the page, each send an address note, which the browser
/// now numbers (see login_tests). Once an address form had focus, focus
/// moving to a field that reaches no address, such as a search field, sends
/// one too, and so does coming back; the same form's other field sends
/// none. A search field focused first sends none.
#[test]
fn focus_leaving_an_address_form_sends_an_address_note() {
    let engine = installed(false).unwrap();
    let form = |name: &str, y: u32| address_form(name, y, "", &[
        ("input", "street", "autocomplete: 'street-address'", "", &[]), ("input", "city", "autocomplete: 'address-level2'", "", &[]), ("input", "postal", "autocomplete: 'postal-code'", "", &[])]);
    let search = "el('input', { name: 'search', attrs: { type: 'search' }, rect: { x: 600, y: 10, w: 200, h: 28 } })".to_owned();
    engine.run(&format!("{}; 0", mount(&[form("ship", 10), form("bill", 300), search])), "t").unwrap();
    let search = "document.body.__children[2]";
    assert_eq!(notes_on_focus(&engine, search), "[]", "no address field had focus yet");
    assert_eq!(notes_on_focus(&engine, "document.forms.item(0).__children[0]"), ADDRESS_NOTE, "the shipping street");
    assert_eq!(notes_on_focus(&engine, "document.forms.item(0).__children[1]"), "[]", "the shipping city");
    assert_eq!(notes_on_focus(&engine, "document.forms.item(1).__children[0]"), ADDRESS_NOTE, "the billing street");
    assert_eq!(notes_on_focus(&engine, search), ADDRESS_NOTE, "a search field reaches no address");
    assert_eq!(notes_on_focus(&engine, search), "[]", "focused again");
    assert_eq!(notes_on_focus(&engine, "document.forms.item(1).__children[2]"), ADDRESS_NOTE, "back in the billing form");
}

/// C11-2, recheck 11's return probe: coming back to a card field after
/// focus was in another frame sends no note, in a processor's frame with
/// number and expiry or with the number alone, since nothing in this
/// document saw focus go (the stub, like a frame that lost focus to
/// another, gets no focusin); the browser then sends the fill to the frame
/// with focus because its document noted a card field (see login_tests).
/// That frame's fill still fills its fields. Coming back after focus moved
/// within the document sends a note (above).
#[test]
fn coming_back_to_a_card_field_from_another_frame_sends_no_note_and_still_fills() {
    let engine = installed_for_cards_only().unwrap();
    engine.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 400, h: 100 }} }}, [{}])); 0", loose_card("hosted", 4)), "t").unwrap();
    let (number, exp) = ("document.body.__children[0].__children[0]", "document.body.__children[0].__children[1]");
    assert_eq!(notes_on_focus(&engine, number), CARD_NOTE);
    assert_eq!(notes_on_focus(&engine, exp), "[]", "the same group");
    engine.run("__active = undefined; 0", "t").unwrap();
    assert_eq!(notes_on_focus(&engine, number), "[]", "back from another frame");
    assert_eq!(card_report(engine, false), ("true hosted-number=4242424242424242|hosted-exp=07/34".to_owned(), r#"[["cardFilled","done","2","0"]]"#.to_owned()));
    let lone = installed_for_cards_only().unwrap();
    lone.run(&format!("mount(el('body', {{ rect: {{ x: 0, y: 0, w: 400, h: 60 }} }}, [{}])); 0", inp("only-number", "type: 'tel', autocomplete: 'cc-number'", 10)), "t").unwrap();
    assert_eq!(notes_on_focus(&lone, "document.body.__children[0]"), CARD_NOTE);
    lone.run("__active = undefined; 0", "t").unwrap();
    assert_eq!(notes_on_focus(&lone, "document.body.__children[0]"), "[]", "a number-only frame, back after the expiry frame");
}
