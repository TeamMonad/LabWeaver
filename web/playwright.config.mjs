import { defineConfig, devices } from '@playwright/test'
import path from 'node:path'
import { ROLE_PROJECTS } from './e2e/config/role-projects.mjs'

export function createPlaywrightConfig({ ci = Boolean(process.env.CI) } = {}) {
  const authDir = process.env.LABWEAVER_AUTH_DIR
  const projects = ROLE_PROJECTS.map((project) => {
    const base = {
      name: project.name,
      testMatch: project.testMatch,
    }

    if (project.name === 'setup') {
      return base
    }

    if (project.storageState) {
      const storageState = authDir
        ? path.join(authDir, path.basename(project.storageState))
        : project.storageState
      return {
        ...base,
        dependencies: ['setup'],
        use: { storageState },
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
    outputDir: process.env.LABWEAVER_PLAYWRIGHT_OUTPUT_DIR || './test-results',
    timeout: 120_000,
    snapshotPathTemplate: `{testDir}/{testFileDir}/{testFileName}-snapshots/{arg}-{projectName}{ext}`,
    forbidOnly: ci,
    retries: ci ? 2 : 0,
    workers: ci ? 1 : undefined,
    // Acceptance output is intentionally console-only. Browser state and any
    // runner output live under the temporary directory supplied by the harness.
    reporter: [['list']],
    use: {
      baseURL: process.env.LABWEAVER_BASE_URL || 'http://localhost:4173',
      trace: 'off',
      screenshot: 'off',
      video: 'off',
      actionTimeout: 30_000,
      navigationTimeout: 60_000,
      ...(process.env.LABWEAVER_BROWSER_CHANNEL
        ? { channel: process.env.LABWEAVER_BROWSER_CHANNEL }
        : {}),
      ...(process.env.LABWEAVER_IGNORE_HTTPS_ERRORS === '1'
        ? { ignoreHTTPSErrors: true }
        : {}),
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
