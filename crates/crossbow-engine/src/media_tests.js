// A stand-in page for running media.js in JavaScriptCore in the tests: only
// the DOM members the script captures, each recording what the script did in
// `crossbow`. Videos are listed in `crossbow.videos`; timers run only when a
// test calls crossbow.run().
var crossbow = { listeners: [], timers: [], calls: [], reports: [], videos: [], active: false, scroll: [0, 0] };

function define(owner, name, getter, setter) {
  Object.defineProperty(owner, name, { get: getter, set: setter, configurable: true, enumerable: true });
}

function EventTarget() {}
EventTarget.prototype.addEventListener = function (type, listener, options) {
  crossbow.listeners.push({ target: this, type: type, listener: listener, options: options });
};
function Node() {}
Node.prototype = Object.create(EventTarget.prototype);
function Element() {}
Element.prototype = Object.create(Node.prototype);
Element.prototype.getBoundingClientRect = function () { return new DOMRectReadOnly(this.box[0], this.box[1], this.box[2], this.box[3]); };
Element.prototype.scrollIntoView = function (options) { crossbow.calls.push(['reveal', this.name, options.block, options.inline, options.behavior]); };
function HTMLElement() {}
HTMLElement.prototype = Object.create(Element.prototype);
function HTMLMediaElement() {}
HTMLMediaElement.prototype = Object.create(HTMLElement.prototype);
define(HTMLMediaElement.prototype, 'paused', function () { return this.state.paused; });
define(HTMLMediaElement.prototype, 'ended', function () { return this.state.ended; });
define(HTMLMediaElement.prototype, 'muted', function () { return this.state.muted; }, function (value) { this.state.muted = value; crossbow.calls.push(['muted', this.name, value]); });
define(HTMLMediaElement.prototype, 'volume', function () { return this.state.volume; });
define(HTMLMediaElement.prototype, 'currentTime', function () { return this.state.time; }, function (value) { this.state.time = value; crossbow.calls.push(['time', this.name, value]); });
define(HTMLMediaElement.prototype, 'duration', function () { return this.state.duration; });
// Like a page without a user activation: play() is refused with a rejected promise.
HTMLMediaElement.prototype.play = function () { crossbow.calls.push(['play', this.name]); return Promise.reject(new Error('NotAllowedError')); };
HTMLMediaElement.prototype.pause = function () { this.state.paused = true; crossbow.calls.push(['pause', this.name]); };
function HTMLVideoElement(name, box, state) { this.name = name; this.box = box; this.state = state; }
HTMLVideoElement.prototype = Object.create(HTMLMediaElement.prototype);
function HTMLCollection(items) { this.items = items; }
HTMLCollection.prototype.item = function (i) { return i < this.items.length ? this.items[i] : null; };
define(HTMLCollection.prototype, 'length', function () { return this.items.length; });
function Document() {}
Document.prototype = Object.create(Node.prototype);
Document.prototype.getElementsByTagName = function (tag) { return new HTMLCollection(tag === 'video' ? crossbow.videos.slice() : []); };
function DOMRectReadOnly(x, y, width, height) { this.box = [x, y, width, height]; }
define(DOMRectReadOnly.prototype, 'x', function () { return this.box[0]; });
define(DOMRectReadOnly.prototype, 'y', function () { return this.box[1]; });
define(DOMRectReadOnly.prototype, 'width', function () { return this.box[2]; });
define(DOMRectReadOnly.prototype, 'height', function () { return this.box[3]; });
function Event(type, target) { this.kind = type; this.at = target; }
define(Event.prototype, 'type', function () { return this.kind; });
define(Event.prototype, 'target', function () { return this.at; });
function UserActivation() {}
define(UserActivation.prototype, 'isActive', function () { return crossbow.active; });
function Navigator() {}
define(Navigator.prototype, 'userActivation', function () { return new UserActivation(); });

// Window's members are its own, as on a [Global] interface.
var window = Object.create(EventTarget.prototype);
define(window, 'devicePixelRatio', function () { return 2; });
define(window, 'innerWidth', function () { return 1280; });
define(window, 'innerHeight', function () { return 720; });
define(window, 'scrollX', function () { return crossbow.scroll[0]; });
define(window, 'scrollY', function () { return crossbow.scroll[1]; });
window.scrollTo = function (options) { crossbow.scroll = [options.left, options.top]; crossbow.calls.push(['scrollTo', options.left, options.top, options.behavior]); };
window.setTimeout = function (callback, delay) { crossbow.timers.push({ callback: callback, delay: delay }); return crossbow.timers.length; };
var document = new Document();
var navigator = new Navigator();

crossbow.video = function (name, box, muted) {
  const video = new HTMLVideoElement(name, box, { paused: false, ended: false, muted: muted, volume: 1, time: 30, duration: 120 });
  crossbow.videos.push(video);
  return video;
};
// A media event, delivered to the listeners on document as Blink would.
crossbow.fire = function (type, target) {
  crossbow.listeners.forEach(function (entry) { if (entry.target === document && entry.type === type) entry.listener(new Event(type, target)); });
};
// A scroll or resize, delivered to the listeners on window.
crossbow.layout = function (type) {
  crossbow.listeners.forEach(function (entry) { if (entry.target === window && entry.type === type) entry.listener(new Event(type, window)); });
};
// Run the timers set so far and give their delays.
crossbow.run = function () {
  const due = crossbow.timers;
  crossbow.timers = [];
  due.forEach(function (timer) { timer.callback(); });
  return due.map(function (timer) { return timer.delay; }).join(' ');
};
crossbow.listening = function () {
  return crossbow.listeners.map(function (entry) {
    const target = entry.target === document ? 'document' : entry.target === window ? 'window' : 'other';
    return target + ' ' + entry.type + ' ' + JSON.stringify(entry.options);
  }).join('\n');
};
// Reports one per line, or 'not strings' if any part wasn't a string.
crossbow.sent = function () {
  const all = crossbow.reports.every(function (report) { return report.every(function (part) { return typeof part === 'string'; }); });
  const lines = crossbow.reports.map(function (report) { return report.join(' '); }).join('\n');
  crossbow.reports = [];
  return all ? lines : 'not strings';
};
crossbow.done = function () {
  const lines = crossbow.calls.map(function (call) { return call.join(' '); }).join('\n');
  crossbow.calls = [];
  return lines;
};
