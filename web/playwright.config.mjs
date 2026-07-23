import { defineConfig, devices } from '@playwright/test'
import { ROLE_PROJECTS } from './e2e/config/role-projects.mjs'
import { resolveEvidenceMetadata } from './e2e/evidence.mjs'

const dataMode = process.env.LABWEAVER_DATA_MODE || process.env.VITE_DATA_MODE || 'live'
const isFixture = dataMode === 'fixture'
const evidenceLabel = isFixture ? 'fixture' : 'live'
const evidenceMetadata = resolveEvidenceMetadata({ dataMode, evidenceLabel })

function parseExternalWebServer(raw) {
  if (raw === undefined || raw === '') return false
  if (raw === 'true') return true
  if (raw === 'false') return false
  throw new Error(
    `[playwright] 非法的 LABWEAVER_EXTERNAL_WEB_SERVER=${String(raw)}，仅允许 true 或 false`,
  )
}

export function createPlaywrightConfig({ ci = Boolean(process.env.CI) } = {}) {
  const externalWebServer = parseExternalWebServer(process.env.LABWEAVER_EXTERNAL_WEB_SERVER)
  const projects = ROLE_PROJECTS.map((project) => {
    const base = {
      name: project.name,
      testMatch: project.testMatch,
    }

    if (project.name === 'setup') {
      return {
        ...base,
        testIgnore: isFixture ? /auth\.setup\.mjs$/ : /auth\.fixture\.setup\.mjs$/,
      }
    }

    if (project.storageState) {
      return {
        ...base,
        dependencies: ['setup'],
        testIgnore: isFixture ? /\.live\.spec\.mjs$/ : /\.fixture\.spec\.mjs$/,
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
    outputDir: `./test-results/${evidenceLabel}`,
    snapshotPathTemplate: `{testDir}/{testFileDir}/{testFileName}-snapshots/${evidenceLabel}/{arg}-{projectName}{ext}`,
    forbidOnly: ci,
    retries: ci ? 2 : 0,
    workers: ci ? 1 : undefined,
    reporter: [
      ['list'],
      ['html', { outputFolder: `playwright-report-${evidenceLabel}`, open: 'never' }],
      ['json', { outputFile: `playwright-report-${evidenceLabel}/report.json` }],
    ],
    use: {
      baseURL: process.env.LABWEAVER_BASE_URL || 'http://localhost:4173',
      trace: 'retain-on-failure',
      screenshot: 'only-on-failure',
      video: 'retain-on-failure',
      actionTimeout: 10_000,
      navigationTimeout: 15_000,
    },
    expect: {
      timeout: 10_000,
      // The pinned Chromium image is identical in CI and local generation, but
      // Linux kernel/font rasterization still changes anti-aliased edge pixels.
      // Keep the allowance below a layout-sized change while avoiding false
      // failures on otherwise byte-for-byte identical content and geometry.
      ...(isFixture ? { toHaveScreenshot: { maxDiffPixelRatio: 0.025 } } : {}),
    },
    projects,
    metadata: evidenceMetadata,
    ...(!externalWebServer && (isFixture || !process.env.LABWEAVER_BASE_URL)
      ? {
          webServer: {
            command: isFixture ? 'pnpm preview:fixture' : 'pnpm preview --port 4173',
            url: 'http://localhost:4173',
            reuseExistingServer: !ci,
          },
        }
      : {}),
  }
}

export default defineConfig(createPlaywrightConfig())
