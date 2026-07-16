import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e/sdk',
  outputDir: './test-results/sdk-transport',
  reporter: [['list']],
  use: {
    ...devices['Desktop Chrome'],
    baseURL: 'http://127.0.0.1:4174',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  webServer: {
    command: 'pnpm exec vite --host 127.0.0.1 --port 4174',
    url: 'http://127.0.0.1:4174/e2e/sdk/harness.html',
    reuseExistingServer: false,
  },
})
