// Crossbow's video reporter, for its own Picture in Picture player. The
// renderer evaluates this in every http(s) top frame as soon as the frame's
// script context exists and before any page script runs. As in login.js,
// every built-in it uses is captured here, first, and later called only
// through these references, without iterators, so a page that patches
// built-ins afterwards can at most feed its own values to report(), which
// only this closure holds. Nothing here logs or stores a value.
//
// report(...strings): "1", a number chosen once for this page, the device
// pixel ratio, the viewport's width and height, the scroll x and y, then for
// each of the four largest videos: its number, paused, ended, muted, volume,
// whether it started during a user activation, and its rect (x, y, width,
// height), all in CSS pixels. It is sent on media events, scrolling and
// resizing, at most every quarter second, and every second while the page has
// a video.
//
// The returned function runs one command from the browser: play, pause,
// seek (seconds), mute (1 or 0), reveal (scroll the video into view, centred)
// or restore (scroll the page to x, y).
(function (report) {
  'use strict';
  const apply = Reflect.apply;
  const gopd = Object.getOwnPropertyDescriptor;
  const get = (owner, name) => { const d = gopd(owner, name); return d === undefined ? undefined : d.get; };
  const set = (owner, name) => { const d = gopd(owner, name); return d === undefined ? undefined : d.set; };
  const win = window;
  const doc = document;
  const nav = navigator;
  const listen = EventTarget.prototype.addEventListener;
  const byTag = Document.prototype.getElementsByTagName;
  const itemAt = HTMLCollection.prototype.item;
  const count = get(HTMLCollection.prototype, 'length');
  const bounds = Element.prototype.getBoundingClientRect;
  const intoView = Element.prototype.scrollIntoView;
  const rectX = get(DOMRectReadOnly.prototype, 'x');
  const rectY = get(DOMRectReadOnly.prototype, 'y');
  const rectWidth = get(DOMRectReadOnly.prototype, 'width');
  const rectHeight = get(DOMRectReadOnly.prototype, 'height');
  const media = HTMLMediaElement.prototype;
  const paused = get(media, 'paused');
  const ended = get(media, 'ended');
  const muted = get(media, 'muted');
  const setMuted = set(media, 'muted');
  const volume = get(media, 'volume');
  const currentTime = get(media, 'currentTime');
  const setCurrentTime = set(media, 'currentTime');
  const duration = get(media, 'duration');
  const play = media.play;
  const pause = media.pause;
  // Window is a [Global] interface: its members live on window itself.
  const ratio = get(win, 'devicePixelRatio');
  const innerWidth = get(win, 'innerWidth');
  const innerHeight = get(win, 'innerHeight');
  const scrollX = get(win, 'scrollX');
  const scrollY = get(win, 'scrollY');
  const scrollTo = win.scrollTo;
  const timeout = win.setTimeout;
  const target = get(Event.prototype, 'target');
  const eventType = get(Event.prototype, 'type');
  const userActivation = get(Navigator.prototype, 'userActivation');
  const isActive = typeof UserActivation === 'function' ? get(UserActivation.prototype, 'isActive') : undefined;
  const VideoCtor = HTMLVideoElement;
  const hasInstance = Function.prototype[Symbol.hasInstance];
  const Weak = WeakMap;
  const weakGet = WeakMap.prototype.get;
  const weakSet = WeakMap.prototype.set;
  const sort = Array.prototype.sort;
  const text = String;
  const finite = Number.isFinite;
  const max = Math.max;
  const min = Math.min;
  const floor = Math.floor;
  const random = Math.random;
  const promiseCatch = Promise.prototype.catch;
  const MAX_VIDEOS = 4;
  const THROTTLE_MS = 250;
  const PERIOD_MS = 1000;
  // -- Everything below calls only the references captured above. --

  const page = text(floor(random() * 9007199254740990) + 1);
  const numbers = new Weak();
  const started = new Weak();
  let next = 1;
  let pending = false;
  let reported = 0;

  const number = (video) => {
    let id = apply(weakGet, numbers, [video]);
    if (id === undefined) { id = next; next += 1; apply(weakSet, numbers, [video, id]); }
    return id;
  };
  const flag = (value) => (value ? '1' : '0');

  const videos = () => {
    const all = apply(byTag, doc, ['video']);
    const total = apply(count, all, []);
    const found = [];
    for (let i = 0; i < total; i += 1) {
      const video = apply(itemAt, all, [i]);
      const rect = apply(bounds, video, []);
      const width = apply(rectWidth, rect, []);
      const height = apply(rectHeight, rect, []);
      if (width > 0 && height > 0) found[found.length] = { video, rect, area: width * height };
    }
    apply(sort, found, [(a, b) => b.area - a.area]);
    return found;
  };

  const send = () => {
    pending = false;
    const found = videos();
    // Say once that the last video went, then stay quiet.
    if (found.length === 0 && reported === 0) return;
    const args = ['1', page, text(apply(ratio, win, [])), text(apply(innerWidth, win, [])), text(apply(innerHeight, win, [])), text(apply(scrollX, win, [])), text(apply(scrollY, win, []))];
    const shown = min(found.length, MAX_VIDEOS);
    for (let i = 0; i < shown; i += 1) {
      const video = found[i].video;
      const rect = found[i].rect;
      const values = [
        text(number(video)), flag(apply(paused, video, [])), flag(apply(ended, video, [])), flag(apply(muted, video, [])),
        text(apply(volume, video, [])), flag(apply(weakGet, started, [video]) === true),
        text(apply(rectX, rect, [])), text(apply(rectY, rect, [])), text(apply(rectWidth, rect, [])), text(apply(rectHeight, rect, [])),
      ];
      for (let j = 0; j < values.length; j += 1) args[args.length] = values[j];
    }
    reported = shown;
    apply(report, undefined, args);
    if (shown > 0) schedule(PERIOD_MS);
  };

  const schedule = (delay) => {
    if (pending) return;
    pending = true;
    apply(timeout, win, [send, delay]);
  };

  const activated = () => {
    if (userActivation === undefined || isActive === undefined) return false;
    const state = apply(userActivation, nav, []);
    return state !== null && state !== undefined && apply(isActive, state, []) === true;
  };

  const onMedia = (event) => {
    const video = apply(target, event, []);
    if (apply(eventType, event, []) === 'play' && apply(hasInstance, VideoCtor, [video])) {
      apply(weakSet, started, [video, activated()]);
    }
    schedule(THROTTLE_MS);
  };
  const types = ['play', 'playing', 'pause', 'ended', 'volumechange', 'emptied', 'loadedmetadata', 'resize'];
  for (let i = 0; i < types.length; i += 1) apply(listen, doc, [types[i], onMedia, true]);
  const onLayout = () => schedule(THROTTLE_MS);
  apply(listen, win, ['scroll', onLayout, { capture: true, passive: true }]);
  apply(listen, win, ['resize', onLayout, { passive: true }]);

  const find = (id) => {
    const all = apply(byTag, doc, ['video']);
    const total = apply(count, all, []);
    for (let i = 0; i < total; i += 1) {
      const video = apply(itemAt, all, [i]);
      if (apply(weakGet, numbers, [video]) === id) return video;
    }
    return null;
  };

  return function (name, first, second) {
    if (name === 'restore') {
      if (finite(first) && finite(second)) apply(scrollTo, win, [{ left: first, top: second, behavior: 'instant' }]);
      schedule(0);
      return;
    }
    const video = find(first);
    if (video === null) return;
    if (name === 'play') {
      const result = apply(play, video, []);
      if (result !== undefined) apply(promiseCatch, result, [() => {}]);
    } else if (name === 'pause') {
      apply(pause, video, []);
    } else if (name === 'seek') {
      const end = apply(duration, video, []);
      const at = max(0, apply(currentTime, video, []) + second);
      apply(setCurrentTime, video, [finite(end) ? min(at, end) : at]);
    } else if (name === 'mute') {
      apply(setMuted, video, [second === 1]);
    } else if (name === 'reveal') {
      apply(intoView, video, [{ block: 'center', inline: 'center', behavior: 'instant' }]);
    }
    schedule(0);
  };
})
