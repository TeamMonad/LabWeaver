import { test } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'

const authDir = path.resolve('.auth')

function fixtureStorageState(role) {
  return {
    cookies: [],
    origins: [
      {
        origin: 'http://localhost:4173',
        localStorage: [
          {
            name: 'access_token',
            value: `fixture-${role}`,
          },
        ],
      },
    ],
  }
}

test('prepare fixture auth states', async () => {
  if (!fs.existsSync(authDir)) {
    fs.mkdirSync(authDir, { recursive: true })
  }

  for (const role of ['teacher', 'student', 'platform-admin']) {
    fs.writeFileSync(
      path.join(authDir, `${role}.json`),
      JSON.stringify(fixtureStorageState(role), null, 2),
    )
  }
})
