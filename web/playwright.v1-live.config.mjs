import { defineConfig } from '@playwright/test'
import { createPlaywrightConfig } from './playwright.config.mjs'

const base = createPlaywrightConfig()
export default defineConfig({
  ...base,
  use: {
    ...base.use,
    launchOptions: {
      args: [
        '--host-resolver-rules=MAP portal.labweaver.internal 10.99.0.130,MAP keycloak.labweaver.internal 10.99.0.120',
        '--ignore-certificate-errors',
      ],
    },
  },
})
