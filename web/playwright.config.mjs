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
    timeout: 120_000,
    snapshotPathTemplate: `{testDir}/{testFileDir}/{testFileName}-snapshots/{arg}-{projectName}{ext}`,
    forbidOnly: ci,
    retries: ci ? 2 : 0,
    workers: ci ? 1 : undefined,
    reporter: [
      ['list'],
      ['html', { outputFolder: 'playwright-report', open: 'never' }],
      ['json', { outputFile: 'playwright-report/report.json' }],
    ],
    use: {
      baseURL: process.env.LABWEAVER_BASE_URL || 'http://localhost:4173',
      trace: 'retain-on-failure',
      screenshot: 'only-on-failure',
      video: 'retain-on-failure',
      actionTimeout: 30_000,
      navigationTimeout: 60_000,
    },
    expect: {
      timeout: 30_000,
      // The pinned Chromium image is identical in CI and local generation, but
      // Linux kernel/font rasterization still changes anti-aliased edge pixels.
      // Keep the allowance below a layout-sized change while avoiding false
      // failures on otherwise byte-for-byte identical content and geometry.
      toHaveScreenshot: { maxDiffPixelRatio: 0.025 },
    },
    projects,
    ...(!process.env.LABWEAVER_BASE_URL
      ? {
          webServer: {
            command: 'pnpm preview --port 4173',
            url: 'http://localhost:4173',
            reuseExistingServer: !ci,
          },
        }
      : {}),
  }
}

export default defineConfig(createPlaywrightConfig())
