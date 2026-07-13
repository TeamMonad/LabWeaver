import { defineConfig } from '@playwright/test'
import { ROLE_PROJECTS } from './e2e/config/role-projects.mjs'

export function createPlaywrightConfig({ ci = Boolean(process.env.CI) } = {}) {
  return {
    testDir: './e2e',
    outputDir: './test-results',
    forbidOnly: ci,
    retries: ci ? 2 : 0,
    workers: ci ? 1 : undefined,
    reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
    use: {
      baseURL: process.env.LABWEAVER_BASE_URL,
      trace: 'retain-on-failure',
      screenshot: 'only-on-failure',
      video: 'retain-on-failure',
      actionTimeout: 10_000,
      navigationTimeout: 15_000,
    },
    expect: { timeout: 10_000 },
    projects: ROLE_PROJECTS.map((project) => ({
      name: project.name,
      testMatch: project.testMatch,
      ...(project.name === 'setup'
        ? {}
        : {
            dependencies: ['setup'],
            use: { storageState: project.storageState },
          }),
    })),
  }
}

export default defineConfig(createPlaywrightConfig())
