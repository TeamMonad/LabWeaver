import { defineConfig, devices } from '@playwright/test'
import { ROLE_PROJECTS } from './e2e/config/role-projects.mjs'

const dataMode = process.env.LABWEAVER_DATA_MODE || process.env.VITE_DATA_MODE || 'live'
const isFixture = dataMode === 'fixture'
const evidenceLabel = isFixture ? 'fixture' : 'live'

export function createPlaywrightConfig({ ci = Boolean(process.env.CI) } = {}) {
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
    expect: { timeout: 10_000 },
    projects,
    metadata: {
      dataMode,
      evidenceLabel,
      fixtureManifestHash: isFixture ? process.env.FIXTURE_MANIFEST_HASH : undefined,
    },
    webServer: {
      command: isFixture ? 'pnpm preview:fixture' : 'pnpm preview --port 4173',
      url: 'http://localhost:4173',
      reuseExistingServer: !ci,
    },
  }
}

export default defineConfig(createPlaywrightConfig())
