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
    // A public acceptance run owns real projects, environments and charges;
    // replaying the whole journey after an ambiguous failure can duplicate
    // those business mutations. Keep ordinary CI fixture retries unchanged,
    // but make every configured public run a single attempt.
    retries: process.env.LABWEAVER_BASE_URL ? 0 : (ci ? 2 : 0),
    workers: ci ? 1 : undefined,
    // Acceptance output is intentionally console-only. Browser state and any
    // runner output live under the temporary directory supplied by the harness.
    reporter: [['list']],
    use: {
      baseURL: process.env.LABWEAVER_BASE_URL || 'http://127.0.0.1:4174',
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
    // Without a deployed base URL only the shell visual/a11y projects can run.
    // They render the built fixture preview: the production preview has no BFF
    // and would stop at the project-context gate, and the fixture dev server
    // optimizes dependencies on first request, which makes cold runs flaky.
    ...(!process.env.LABWEAVER_BASE_URL
      ? {
          webServer: {
            command: 'pnpm build:fixtures && pnpm exec vite preview --config vite.fixtures.config.ts --host 127.0.0.1 --port 4174 --strictPort',
            url: 'http://127.0.0.1:4174/fixture-preview.html',
            reuseExistingServer: !ci,
            timeout: 180_000,
          },
        }
      : {}),
  }
}

export default defineConfig(createPlaywrightConfig())
