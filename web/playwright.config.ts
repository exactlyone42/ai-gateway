import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  retries: 0,
  workers: 1,
  use: {
    baseURL: process.env.ADMIN_WEB_BASE_URL ?? 'http://127.0.0.1:8080',
    browserName: 'chromium',
    launchOptions: { executablePath: '/usr/bin/google-chrome' },
  },
})
