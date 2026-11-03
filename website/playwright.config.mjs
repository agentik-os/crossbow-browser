import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  testMatch: '**/*.spec.mjs',
  timeout: 25000,
  expect: { timeout: 5000 },
  workers: 2,
  reporter: 'list',
  use: {
    baseURL: process.env.CROSSBOW_SITE_URL || 'http://127.0.0.1:8798',
    headless: true,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
});
