import fs from 'node:fs'
import path from 'node:path'

const OIDC_AUTHORITY = process.env.VITE_OIDC_AUTHORITY || 'http://localhost:4173/oidc'
const OIDC_CLIENT_ID = process.env.VITE_OIDC_CLIENT_ID || 'labweaver-fixture'
const USER_STORE_KEY = `oidc.user:${OIDC_AUTHORITY}:${OIDC_CLIENT_ID}`

function fixtureOidcUser({ role, courseId, name, email }, nowSeconds) {
  const expiresAt = nowSeconds + 3600
  return {
    id_token: '', session_state: null, access_token: `fixture-${role}`,
    token_type: 'Bearer', scope: 'openid profile email',
    profile: {
      sub: `fixture-${role}`, iss: OIDC_AUTHORITY, aud: OIDC_CLIENT_ID,
      exp: expiresAt, iat: nowSeconds, name, email,
      roles: [role === 'platform-admin' ? 'admin' : role],
      ...(courseId !== undefined ? { course_id: courseId } : {}),
    },
    expires_at: expiresAt, state: null,
  }
}

function fixtureStorageState(identity, nowSeconds) {
  const user = fixtureOidcUser(identity, nowSeconds)
  return {
    cookies: [],
    origins: [{
      origin: 'http://localhost:4173',
      localStorage: [
        {name: 'access_token', value: `fixture-${identity.role}`},
        {name: USER_STORE_KEY, value: JSON.stringify(user)},
      ],
    }],
  }
}

export function writeFixtureAuthStates(authDir = path.resolve('.auth'), now = new Date()) {
  const nowSeconds = Math.floor(now.getTime() / 1000)
  if (!Number.isFinite(nowSeconds)) throw new Error('LW_FIXTURE_AUTH_TIME_INVALID')
  fs.mkdirSync(authDir, {recursive: true})
  const identities = [
    {role: 'teacher', courseId: 'course-101', name: 'Fixture Teacher', email: 'teacher@fixture.labweaver.io'},
    {role: 'student', courseId: 'course-101', name: 'Fixture Student', email: 'student@fixture.labweaver.io'},
    {role: 'platform-admin', name: 'Fixture Admin', email: 'admin@fixture.labweaver.io', file: 'platform-admin.json'},
    {role: 'student', name: 'Fixture Student Without Course', email: 'student-blocked@fixture.labweaver.io'},
    {role: 'teacher', name: 'Fixture Teacher Without Course', email: 'teacher-blocked@fixture.labweaver.io'},
  ]
  const written = []
  for (const identity of identities) {
    const fileName = identity.file ?? (identity.courseId === undefined ? `${identity.role}-blocked.json` : `${identity.role}.json`)
    fs.writeFileSync(path.join(authDir, fileName), `${JSON.stringify(fixtureStorageState(identity, nowSeconds), null, 2)}\n`)
    written.push(fileName)
  }
  return written
}
