const root = document.documentElement;
document.addEventListener('keydown', () => { root.dataset.input = 'keyboard'; }, { passive: true });
document.addEventListener('pointerdown', () => { root.dataset.input = 'pointer'; }, { passive: true });

const samples = {
  work: {
    name: 'Work', icon: 'grid', folder: 'Current project', active: 'Launch notes', pin: 'Project board',
    today: ['Interface ideas', 'A fresh perspective'], title: ['A little less', 'scattered.'],
    description: ['The plan, the project, the references.', 'Right where you left them.'],
    checklist: ['Make room for the important things', 'Keep the next idea close'],
  },
  personal: {
    name: 'Personal', icon: 'sun', folder: 'Off the clock', active: 'Weekend plans', pin: 'Recipes to try',
    today: ['The next good read', 'A slower Sunday'], title: ['More room', 'for you.'],
    description: ['The weekend, the little plans, the good finds.', 'A different space. A different pace.'],
    checklist: ['Save something worth coming back to', 'Let work stay in its own Space'],
  },
  explore: {
    name: 'Explore', icon: 'compass', folder: 'Somewhere new', active: 'The next adventure', pin: 'Places to go',
    today: ['A scenic detour', 'Beyond the usual'], title: ['Follow your', 'curiosity.'],
    description: ['One idea leads to another.', 'Give your next rabbit hole a home.'],
    checklist: ['Collect the places that catch your eye', 'Pick up the trail when you return'],
  },
};

const tabs = [...document.querySelectorAll('[data-space][role="tab"]')];
const preview = document.querySelector('#space-preview');
function setLines(element, lines) {
  element.replaceChildren(document.createTextNode(lines[0]), document.createElement('br'), document.createTextNode(lines[1]));
}
function selectSpace(tab) {
  const sample = samples[tab.dataset.space];
  if (!sample || !preview) return;
  tabs.forEach((item) => {
    const selected = item === tab;
    item.setAttribute('aria-selected', String(selected));
    item.tabIndex = selected ? 0 : -1;
  });
  preview.dataset.space = tab.dataset.space;
  preview.setAttribute('aria-labelledby', tab.id);
  const fields = { 'space-name': sample.name, 'sample-folder': sample.folder, 'sample-active': sample.active,
    'sample-pin': sample.pin, 'sample-address': sample.active,
    'sample-today-one': sample.today[0], 'sample-today-two': sample.today[1] };
  Object.entries(fields).forEach(([id, text]) => { document.getElementById(id).textContent = text; });
  ['space-icon', 'sample-symbol'].forEach((id) => document.getElementById(id).setAttribute('href', `#${sample.icon}`));
  setLines(document.getElementById('sample-title'), sample.title);
  setLines(document.getElementById('sample-description'), sample.description);
  document.querySelectorAll('#sample-checklist span').forEach((item, i) => { item.textContent = sample.checklist[i]; });
  if (root.dataset.input !== 'keyboard' && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
    preview.getAnimations().forEach((animation) => animation.cancel());
    preview.animate([{ opacity: 0.86 }, { opacity: 1 }], { duration: 180, easing: 'cubic-bezier(.23,1,.32,1)' });
  }
}
tabs.forEach((tab, index) => {
  tab.addEventListener('click', () => selectSpace(tab));
  tab.addEventListener('keydown', (event) => {
    let next;
    if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
    if (event.key === 'ArrowLeft') next = (index + tabs.length - 1) % tabs.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = tabs.length - 1;
    if (next === undefined) return;
    event.preventDefault();
    tabs[next].focus();
    selectSpace(tabs[next]);
  });
});

async function requestJson(path, options = {}, timeout = 15000) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeout);
  try {
    const response = await fetch(path, { ...options, signal: controller.signal, credentials: 'omit', cache: 'no-store', redirect: 'error' });
    const body = await response.json();
    return { response, body };
  } finally {
    clearTimeout(timer);
  }
}

const form = document.getElementById('waitlist-form');
const privacyContact = document.getElementById('privacy-contact');
const privacyAvailability = document.getElementById('privacy-availability');

function showPrivacyContact(config) {
  if (!privacyContact) return;
  const contact = config.privacyContact;
  if (typeof contact === 'string' && /^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+$/i.test(contact)) {
    const link = document.createElement('a');
    link.textContent = contact;
    link.href = `mailto:${encodeURIComponent(contact)}`;
    privacyContact.replaceChildren(document.createTextNode('For access, correction or removal of your waitlist entry, contact '), link, document.createTextNode('.'));
    privacyAvailability.textContent = config.enabled === true
      ? 'The waitlist is open. Joining is optional and separate from a Crossbow browser account.'
      : 'The waitlist is not accepting registrations right now.';
  } else {
    privacyAvailability.textContent = 'The waitlist is not accepting registrations. A privacy contact will be provided before signup opens.';
  }
}

if (form) {
  const email = document.getElementById('email');
  const join = document.getElementById('join-button');
  const joinLabel = join.querySelector('span');
  const status = document.getElementById('form-status');
  const retry = document.getElementById('retry-config');
  const another = document.getElementById('another-email');
  const challenge = document.getElementById('turnstile');
  let enabled = false;
  let busy = false;
  let completed = false;
  let token = '';
  let widget;
  let challengeSize;
  let scriptPromise;

  function message(text, tone = '') {
    status.textContent = text;
    status.dataset.tone = tone;
  }
  function updateControls() {
    email.disabled = !enabled || busy;
    email.readOnly = completed;
    join.disabled = !enabled || busy || completed || !token;
    joinLabel.textContent = busy ? 'Joining…' : completed ? 'You’re on the list' : 'Join the waitlist';
    form.setAttribute('aria-busy', String(busy));
  }
  function loadTurnstile() {
    if (window.turnstile) return Promise.resolve();
    if (scriptPromise) return scriptPromise;
    scriptPromise = new Promise((resolve, reject) => {
      const script = document.createElement('script');
      script.src = 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit';
      script.async = true;
      const timer = setTimeout(() => fail(), 12000);
      function fail() {
        clearTimeout(timer);
        script.remove();
        scriptPromise = undefined;
        reject(new Error('Challenge unavailable'));
      }
      script.onerror = fail;
      script.onload = () => {
        clearTimeout(timer);
        if (window.turnstile) resolve();
        else fail();
      };
      document.head.append(script);
    });
    return scriptPromise;
  }
  function resetChallenge() {
    token = '';
    if (widget !== undefined && window.turnstile) window.turnstile.reset(widget);
  }
  async function configure() {
    enabled = false;
    token = '';
    retry.hidden = true;
    completed = false;
    another.hidden = true;
    updateControls();
    message('Checking waitlist availability…');
    if (widget !== undefined && window.turnstile) {
      window.turnstile.remove(widget);
      widget = undefined;
    }
    try {
      const { response, body } = await requestJson('/api/config', {}, 10000);
      if (!response.ok || typeof body.enabled !== 'boolean') throw new Error('Configuration unavailable');
      if (body.enabled !== true) {
        message('The waitlist isn’t open yet. Please check back soon. No email is being collected.');
        retry.hidden = false;
        return;
      }
      if (typeof body.turnstileSiteKey !== 'string' || !body.turnstileSiteKey || typeof body.privacyContact !== 'string' || !body.privacyContact) {
        throw new Error('Configuration incomplete');
      }
      message('Loading the security check…');
      await loadTurnstile();
      enabled = true;
      updateControls();
      challengeSize = challenge.clientWidth < 300 ? 'compact' : 'flexible';
      widget = window.turnstile.render(challenge, {
        sitekey: body.turnstileSiteKey,
        action: 'waitlist',
        theme: 'auto',
        size: challengeSize,
        'response-field': false,
        callback(value) {
          token = value;
          if (!busy && !completed && status.dataset.tone !== 'error') message('Ready when you are.');
          updateControls();
        },
        'expired-callback'() {
          token = '';
          if (!busy && !completed) message('The security check expired. Complete the new check to continue.');
          updateControls();
        },
        'error-callback'() {
          token = '';
          if (!busy && !completed) {
            message('The security check could not load. Check your connection and try again.', 'error');
            retry.hidden = false;
          }
          updateControls();
          return true;
        },
        'timeout-callback'() {
          token = '';
          if (!busy && !completed) {
            message('The security check timed out. Try again when you’re ready.', 'error');
            retry.hidden = false;
          }
          updateControls();
        },
      });
    } catch {
      enabled = false;
      updateControls();
      message('Signup is temporarily unavailable. Check your connection and try again. No email was submitted.', 'error');
      retry.hidden = false;
    }
  }

  new ResizeObserver(() => {
    const size = challenge.clientWidth < 300 ? 'compact' : 'flexible';
    if (widget !== undefined && enabled && !busy && !completed && size !== challengeSize) configure();
  }).observe(challenge);
  email.addEventListener('input', () => { email.removeAttribute('aria-invalid'); });
  retry.addEventListener('click', configure);
  another.addEventListener('click', async () => {
    email.value = '';
    await configure();
    if (enabled) email.focus();
  });
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (!enabled || busy || completed) return;
    email.value = email.value.trim();
    if (!email.validity.valid) {
      email.setAttribute('aria-invalid', 'true');
      message('Enter a valid email address, such as you@example.com.', 'error');
      email.focus();
      return;
    }
    if (!token) {
      message('Complete the security check before joining.', 'error');
      return;
    }
    busy = true;
    retry.hidden = true;
    updateControls();
    message('Adding your address to the waitlist…');
    try {
      const { response, body } = await requestJson('/api/waitlist', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ email: email.value, turnstileToken: token, website: document.getElementById('website').value }),
      }, 20000);
      if (!response.ok || body.ok !== true) {
        const text = response.status === 429
          ? 'Too many attempts. Wait a minute, then complete a new security check and try again.'
          : response.status === 503
            ? 'Signup is temporarily unavailable. Please try again later.'
            : 'We couldn’t confirm your registration. Complete a new security check and try again.';
        message(text, 'error');
        resetChallenge();
        if (response.status === 503) retry.hidden = false;
        return;
      }
      completed = true;
      token = '';
      if (widget !== undefined && window.turnstile) {
        window.turnstile.remove(widget);
        widget = undefined;
      }
      document.getElementById('website').value = '';
      email.value = '';
      message('You’re on the list. We’ll email you when there’s news about early access. If you joined before, you’re still all set.', 'success');
      another.hidden = false;
      another.focus();
    } catch {
      message('We couldn’t confirm your registration. Check your connection and try again; using the same address won’t create a second entry.', 'error');
      resetChallenge();
    } finally {
      busy = false;
      updateControls();
    }
  });
  configure();
} else if (privacyContact) {
  requestJson('/api/config', {}, 10000).then(({ response, body }) => {
    if (response.ok) showPrivacyContact(body);
  }).catch(() => {
    privacyAvailability.textContent = 'Waitlist availability could not be checked. Return to the homepage to retry; this page does not collect an email.';
  });
}
