import { defineConfig, devices } from '@playwright/test'
import { ROLE_PROJECTS } from './e2e/config/role-projects.mjs'

export function createPlaywrightConfig({ ci = Boolean(process.env.CI) } = {}) {
  const projects = ROLE_PROJECTS.map((project) => {
    const base = {
      name: project.name,
      testMatch: project.testMatch,
    }

    if (project.name === 'setup') {
      return base
    }

    if (project.storageState) {
      return {
        ...base,
        dependencies: ['setup'],
        use: { storageState: project.storageState },
      }
    }

    return {
      ...base,
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 900 },
      },
    }
  })

  return {
    testDir: './e2e',
    outputDir: './test-results',
    forbidOnly: ci,
    retries: ci ? 2 : 0,
    workers: ci ? 1 : undefined,
    reporter: [['list'], ['html', { outputFolder: 'playwright-report', open: 'never' }]],
    use: {
      baseURL: process.env.LABWEAVER_BASE_URL || 'http://localhost:4173',
      trace: 'retain-on-failure',
      screenshot: 'only-on-failure',
      video: 'retain-on-failure',
      actionTimeout: 10_000,
      navigationTimeout: 15_000,
    },
    expect: { timeout: 10_000 },
    projects,
    webServer: {
      command: 'pnpm preview --port 4173',
      url: 'http://localhost:4173',
      reuseExistingServer: !ci,
    },
  }
}

export default defineConfig(createPlaywrightConfig())
