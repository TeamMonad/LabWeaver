/**
 * Deterministic human-drivable fixture identities.
 *
 * Playwright injects storageState directly, but a human opening a protected
 * page in a fixture build has no session and the fake OIDC authority cannot
 * complete a redirect. This module writes the exact same localStorage shape
 * as `e2e/setup/auth.fixture.setup.mjs` so a manual click signs in a
 * deterministic fixture identity, then reloads into the target page.
 *
 * Fixture-only module: it is loaded dynamically behind `__IS_FIXTURE__` and
 * never ends up in the production bundle.
 */

export interface FixtureDemoRole {
  id: string
  label: string
  description: string
  role: 'teacher' | 'student' | 'platform-admin'
  courseId?: string
  name: string
  email: string
  home: string
}

export const FIXTURE_DEMO_ROLES: FixtureDemoRole[] = [
  {
    id: 'teacher',
    label: '以教师身份演示',
    description: '课程 course-101，含 LLM 策略、材料上传与 AgentRun 流程',
    role: 'teacher',
    courseId: 'course-101',
    name: 'Fixture Teacher',
    email: 'teacher@fixture.labweaver.io',
    home: '/teacher/materials',
  },
  {
    id: 'student',
    label: '以学生身份演示',
    description: '课程 course-101，含 SSH 公钥与环境控制台',
    role: 'student',
    courseId: 'course-101',
    name: 'Fixture Student',
    email: 'student@fixture.labweaver.io',
    home: '/student/environments',
  },
  {
    id: 'platform-admin',
    label: '以管理员身份演示',
    description: '平台管理入口（无课程上下文）',
    role: 'platform-admin',
    name: 'Fixture Admin',
    email: 'admin@fixture.labweaver.io',
    home: '/admin',
  },
  {
    id: 'teacher-blocked',
    label: '教师（无课程上下文）',
    description: '演示 blocked 诊断：缺少 course_id claim',
    role: 'teacher',
    name: 'Fixture Teacher Without Course',
    email: 'teacher-blocked@fixture.labweaver.io',
    home: '/teacher/materials',
  },
  {
    id: 'student-blocked',
    label: '学生（无课程上下文）',
    description: '演示 blocked 诊断：缺少 course_id claim',
    role: 'student',
    name: 'Fixture Student Without Course',
    email: 'student-blocked@fixture.labweaver.io',
    home: '/student/environments',
  },
]

// Must stay in sync with web/.env.e2e-fixture and the Playwright auth setup.
const OIDC_AUTHORITY = (import.meta.env.VITE_OIDC_AUTHORITY as string | undefined) ?? 'http://localhost:4173/oidc'
const OIDC_CLIENT_ID = (import.meta.env.VITE_OIDC_CLIENT_ID as string | undefined) ?? 'labweaver-fixture'
const USER_STORE_KEY = `oidc.user:${OIDC_AUTHORITY}:${OIDC_CLIENT_ID}`

function buildUser(entry: FixtureDemoRole) {
  const nowSeconds = Math.floor(Date.now() / 1000)
  const expiresAt = nowSeconds + 3600
  return {
    id_token: '',
    session_state: null,
    access_token: `fixture-${entry.role}`,
    token_type: 'Bearer',
    scope: 'openid profile email',
    profile: {
      sub: `fixture-${entry.role}`,
      iss: OIDC_AUTHORITY,
      aud: OIDC_CLIENT_ID,
      exp: expiresAt,
      iat: nowSeconds,
      name: entry.name,
      email: entry.email,
      roles: [entry.role === 'platform-admin' ? 'admin' : entry.role],
      ...(entry.courseId !== undefined ? { course_id: entry.courseId } : {}),
    },
    expires_at: expiresAt,
    state: null,
  }
}

/**
 * Persist the fixture identity and reload into the originally requested page
 * (router guard stores it in sessionStorage) or the role's home page.
 */
export function signInFixtureDemo(roleId: string): void {
  const entry = FIXTURE_DEMO_ROLES.find((r) => r.id === roleId)
  if (!entry) return
  const user = buildUser(entry)
  window.localStorage.setItem('access_token', `fixture-${entry.role}`)
  window.localStorage.setItem(USER_STORE_KEY, JSON.stringify(user))
  const returnTo = window.sessionStorage.getItem('auth-return-to')
  window.sessionStorage.removeItem('auth-return-to')
  window.location.assign(returnTo ?? entry.home)
}

/** Clear the fixture identity and reload back to the home page. */
export function signOutFixtureDemo(): void {
  window.localStorage.removeItem('access_token')
  window.localStorage.removeItem(USER_STORE_KEY)
  window.location.assign('/')
}
