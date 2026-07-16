import { test } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'

const authDir = path.resolve('.auth')

// Must stay in sync with web/.env.e2e-fixture so that the OIDC user state key
// matches the UserManager created by the app in fixture builds.
const OIDC_AUTHORITY = process.env.VITE_OIDC_AUTHORITY || 'http://localhost:4173/oidc'
const OIDC_CLIENT_ID = process.env.VITE_OIDC_CLIENT_ID || 'labweaver-fixture'
const USER_STORE_KEY = `oidc.user:${OIDC_AUTHORITY}:${OIDC_CLIENT_ID}`

function fixtureOidcUser({ role, courseId, name, email }) {
  const nowSeconds = Math.floor(Date.now() / 1000)
  const expiresAt = nowSeconds + 3600
  return {
    id_token: '',
    session_state: null,
    access_token: `fixture-${role}`,
    token_type: 'Bearer',
    scope: 'openid profile email',
    profile: {
      sub: `fixture-${role}`,
      iss: OIDC_AUTHORITY,
      aud: OIDC_CLIENT_ID,
      exp: expiresAt,
      iat: nowSeconds,
      name,
      email,
      // Router role claims use the application role names ('admin' for platform-admin).
      roles: [role === 'platform-admin' ? 'admin' : role],
      // Course context used by useCourseContext before the real course selector (#47).
      ...(courseId !== undefined ? { course_id: courseId } : {}),
    },
    expires_at: expiresAt,
    state: null,
  }
}

function fixtureStorageState({ role, courseId, name, email }) {
  const user = fixtureOidcUser({ role, courseId, name, email })
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
          {
            name: USER_STORE_KEY,
            value: JSON.stringify(user),
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

  const states = [
    { role: 'teacher', courseId: 'course-101', name: 'Fixture Teacher', email: 'teacher@fixture.labweaver.io' },
    { role: 'student', courseId: 'course-101', name: 'Fixture Student', email: 'student@fixture.labweaver.io' },
    {
      role: 'platform-admin',
      name: 'Fixture Admin',
      email: 'admin@fixture.labweaver.io',
    },
    // Deterministic blocked student: no course_id claim and no default course env,
    // so useCourseContext resolves to null and protected pages show the blocked diagnostic.
    {
      role: 'student',
      name: 'Fixture Student Without Course',
      email: 'student-blocked@fixture.labweaver.io',
    },
  ]

  for (const state of states) {
    const fileName = state.courseId === undefined && state.role === 'student' ? 'student-blocked.json' : `${state.role}.json`
    fs.writeFileSync(
      path.join(authDir, fileName),
      JSON.stringify(fixtureStorageState(state), null, 2),
    )
  }
})
