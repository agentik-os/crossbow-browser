import { test, expect } from '@playwright/test';

// These UI fixtures never call Clerk or complete a real Turnstile challenge.
// They exercise recoverable form states, not provider integration or email delivery.
async function enabledFixture(page, handler) {
  await page.route('**/api/config', (route) => route.fulfill({ json: {
    enabled: true, turnstileSiteKey: 'fixture-site-key', privacyContact: 'privacy@example.com',
  } }));
  await page.route('https://challenges.cloudflare.com/**', (route) => route.fulfill({
    contentType: 'application/javascript',
    body: `window.turnstile = {
      render(el, options) { window.fixtureChallenge = options; el.textContent = 'Security check fixture'; queueMicrotask(() => options.callback('fixture-token')); return 'fixture-widget'; },
      remove() { document.getElementById('turnstile').replaceChildren(); },
      reset() { queueMicrotask(() => window.fixtureChallenge.callback('fixture-token')); }
    };`,
  }));
  if (handler) await page.route('**/api/waitlist', handler);
}

for (const viewport of [{ width: 1440, height: 1000 }, { width: 390, height: 844 }, { width: 320, height: 740 }]) {
  for (const colorScheme of ['light', 'dark']) {
    test(`layout and sample Spaces ${viewport.width} ${colorScheme}`, async ({ page }, testInfo) => {
      await page.setViewportSize(viewport);
      await page.emulateMedia({ colorScheme, reducedMotion: 'reduce' });
      const errors = [];
      page.on('pageerror', (error) => errors.push(error.message));
      await page.goto('/');
      await expect(page.locator('#form-status')).toContainText('isn’t open yet');
      await expect(page.locator('#join-button')).toBeDisabled();
      await expect(page.locator('#email')).toBeDisabled();
      await expect(page.locator('#spaces')).toBeVisible();
      await expect(page.getByRole('heading', { level: 1 })).toContainText('Room for');
      const noOverflow = () => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);
      expect(await noOverflow()).toBe(true);
      await page.getByRole('tab', { name: 'Personal', exact: true }).click();
      await expect(page.locator('#sample-title')).toContainText('More room');
      await expect(page.getByRole('tab', { name: 'Personal', exact: true })).toHaveAttribute('aria-selected', 'true');
      await page.keyboard.press('ArrowRight');
      await expect(page.getByRole('tab', { name: 'Explore', exact: true })).toBeFocused();
      await expect(page.locator('#sample-title')).toContainText('Follow your');
      await expect(page.locator('#space-preview')).toHaveAttribute('aria-labelledby', 'tab-explore');
      expect(await page.locator('#space-preview').evaluate((el) => el.getAnimations().length)).toBe(0);
      await page.keyboard.press('Home');
      await expect(page.getByRole('tab', { name: 'Work', exact: true })).toBeFocused();
      await expect(page.locator('#sample-title')).toContainText('A little less');
      const question = page.getByText('Can I download Crossbow yet?', { exact: true });
      await question.click();
      await expect(page.getByText('Not yet. Crossbow is a development prototype,', { exact: false })).toBeVisible();
      await question.press('Enter');
      await expect(question.locator('..')).not.toHaveAttribute('open', '');
      expect(await noOverflow()).toBe(true);
      await page.evaluate(() => window.scrollTo(0, 0));
      await page.screenshot({ path: testInfo.outputPath(`landing-${viewport.width}-${colorScheme}.png`), fullPage: true });
      expect(errors).toEqual([]);
    });
  }
}

test('real local Worker stays closed and serves privacy and security headers', async ({ request, page }) => {
  const config = await request.get('/api/config');
  expect(config.status()).toBe(200);
  expect((await config.json()).enabled).toBe(false);
  const denied = await request.post('/api/waitlist', {
    headers: { Origin: new URL(test.info().project.use.baseURL).origin },
    data: { email: 'qa@example.com', turnstileToken: 'not-a-real-token', website: '' },
  });
  expect(denied.status()).toBe(503);
  expect((await denied.json()).ok).toBe(false);
  const response = await page.goto('/privacy');
  expect(response.status()).toBe(200);
  expect(response.headers()['content-security-policy']).toContain("default-src 'self'");
  await expect(page.getByRole('heading', { name: 'Waitlist privacy.' })).toBeVisible();
  await expect(page.locator('#privacy-availability')).toContainText('not accepting');
  const missing = await request.get('/not-a-real-page');
  expect(missing.status()).toBe(404);
});

test('disabled page never loads the third-party challenge or posts an address', async ({ page }) => {
  const thirdParties = [];
  const submissions = [];
  page.on('request', (request) => {
    if (request.url().startsWith('https://')) thirdParties.push(request.url());
    if (request.url().endsWith('/api/waitlist')) submissions.push(request.url());
  });
  await page.goto('/');
  await expect(page.locator('#form-status')).toContainText('No email is being collected');
  await page.getByRole('button', { name: 'Check again' }).click();
  await expect(page.locator('#form-status')).toContainText('No email is being collected');
  expect(thirdParties).toEqual([]);
  expect(submissions).toEqual([]);
});

test('config failure offers a retry without claiming registration', async ({ page }) => {
  let attempts = 0;
  await page.route('**/api/config', (route) => {
    attempts++;
    return attempts === 1 ? route.abort() : route.fulfill({ json: { enabled: false } });
  });
  await page.goto('/');
  await expect(page.locator('#form-status')).toContainText('No email was submitted');
  await page.getByRole('button', { name: 'Check again' }).click();
  await expect(page.locator('#form-status')).toContainText('isn’t open yet');
});

test('fixture-only valid submit, duplicate message, no browser email persistence', async ({ page }) => {
  const bodies = [];
  await enabledFixture(page, async (route) => {
    bodies.push(route.request().postDataJSON());
    await route.fulfill({ json: { ok: true, message: 'Accepted' } });
  });
  await page.goto('/');
  await expect(page.locator('#join-button')).toBeEnabled();
  await page.locator('#email').fill('not-an-email');
  await page.locator('#join-button').click();
  await expect(page.locator('#email')).toHaveAttribute('aria-invalid', 'true');
  await expect(page.locator('#email')).toBeFocused();
  expect(bodies).toHaveLength(0);
  await page.locator('#email').fill('qa@example.com');
  await page.locator('#join-button').click();
  await expect(page.locator('#form-status')).toHaveAttribute('data-tone', 'success');
  expect(bodies).toEqual([{ email: 'qa@example.com', turnstileToken: 'fixture-token', website: '' }]);
  await expect(page.locator('#email')).toHaveValue('');
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
  await page.getByRole('button', { name: 'Use another email' }).click();
  await expect(page.locator('#join-button')).toBeEnabled();
  await expect(page.locator('#email')).toBeFocused();
  await page.locator('#email').fill('qa@example.com');
  await page.locator('#email').press('Enter');
  await expect(page.locator('#form-status')).toContainText('If you joined before');
  expect(bodies).toHaveLength(2);
  await page.reload();
  await expect(page.locator('#email')).toHaveValue('');
  await expect(page.locator('#form-status')).not.toHaveAttribute('data-tone', 'success');
});

test('fixture-only provider error, rate limiting, malformed response and retry', async ({ page }) => {
  let calls = 0;
  await enabledFixture(page, async (route) => {
    calls++;
    if (calls === 1) return route.fulfill({ status: 503, json: { ok: false } });
    if (calls === 2) return route.fulfill({ status: 429, json: { ok: false } });
    if (calls === 3) return route.fulfill({ status: 200, body: '{broken' });
    if (calls === 4) return route.fulfill({ status: 200, json: { ok: false } });
    return route.fulfill({ json: { ok: true } });
  });
  await page.goto('/');
  await expect(page.locator('#join-button')).toBeEnabled();
  await page.locator('#email').fill('retry@example.com');
  for (const text of ['temporarily unavailable', 'Too many attempts', 'couldn’t confirm', 'couldn’t confirm']) {
    await page.locator('#join-button').click();
    await expect(page.locator('#form-status')).toContainText(text);
    await expect(page.locator('#email')).toHaveValue('retry@example.com');
    await expect(page.locator('#form-status')).toHaveAttribute('data-tone', 'error');
    await expect(page.locator('#join-button')).toBeEnabled();
  }
  await page.locator('#join-button').click();
  await expect(page.locator('#form-status')).toHaveAttribute('data-tone', 'success');
  expect(calls).toBe(5);
});

test('fixture-only double-submit blocked, expiry recoverable, compact challenge reflow', async ({ page }) => {
  let release;
  let calls = 0;
  await enabledFixture(page, async (route) => {
    calls++;
    await new Promise((resolve) => { release = resolve; });
    await route.fulfill({ json: { ok: true } });
  });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto('/');
  await expect(page.locator('#join-button')).toBeEnabled();
  expect(await page.evaluate(() => window.fixtureChallenge.size)).toBe('flexible');
  await page.setViewportSize({ width: 320, height: 740 });
  await expect.poll(() => page.evaluate(() => window.fixtureChallenge.size)).toBe('compact');
  await expect(page.locator('#join-button')).toBeEnabled();
  await page.evaluate(() => window.fixtureChallenge['expired-callback']());
  await expect(page.locator('#join-button')).toBeDisabled();
  await expect(page.locator('#form-status')).toContainText('expired');
  await page.evaluate(() => window.fixtureChallenge.callback('new-fixture-token'));
  await page.locator('#email').fill('once@example.com');
  await page.locator('#join-button').click();
  await expect(page.locator('#waitlist-form')).toHaveAttribute('aria-busy', 'true');
  await page.locator('#waitlist-form').evaluate((el) => el.requestSubmit());
  await expect.poll(() => calls).toBe(1);
  release();
  await expect(page.locator('#form-status')).toHaveAttribute('data-tone', 'success');
});

test('privacy fixture uses configured contact without creating an account', async ({ page }) => {
  await enabledFixture(page);
  await page.goto('/privacy');
  await expect(page.locator('#privacy-contact a')).toHaveText('privacy@example.com');
  await expect(page.locator('#privacy-availability')).toContainText('The waitlist is open');
  await expect(page.getByRole('form')).toHaveCount(0);
});
