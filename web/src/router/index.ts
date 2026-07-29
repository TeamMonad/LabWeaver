import { createRouter, createWebHistory } from 'vue-router'
import type { RouteRecordRaw, RouteRecordNormalized } from 'vue-router'
import { useAuth } from '@/composables/useAuth'
import { OIDC_ENABLED } from '@/config'
import { IS_FIXTURE } from '@/config/dataMode'

export type AppRole = 'teacher' | 'student' | 'researcher' | 'admin'

export interface AppRouteMeta {
  title?: string
  navGroup?: 'teacher' | 'student' | 'researcher' | 'admin'
  requiredRoles?: AppRole[]
  requiredCourseScope?: boolean
}

declare module 'vue-router' {
  interface RouteMeta extends AppRouteMeta {}
}

function roleRoute(role: AppRole, path: string, title: string, component: () => Promise<unknown>, children: RouteRecordRaw[] = []): RouteRecordRaw {
  return {
    path,
    name: role,
    component,
    meta: { title, navGroup: role, requiredRoles: [role] },
    redirect: children.length ? `${path}/${children[0].path}` : undefined,
    children,
  }
}

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    name: 'home',
    component: () => import('@/views/HomeView.vue'),
    meta: { title: 'LabWeaver' },
  },
  // Fixture-only deterministic console layout preview: renders the xterm/noVNC
  // layouts without creating an environment, issuing a grant, or calling any
  // backend. It is registered only in fixture builds.
  ...(IS_FIXTURE
    ? [
        {
          path: '/fixture/console-preview',
          name: 'fixture-console-preview',
          component: () => import('@/views/fixture/ConsolePreviewView.vue'),
          meta: { title: '控制台布局预览' },
        } satisfies RouteRecordRaw,
      ]
    : []),
  {
    path: '/auth/callback',
    name: 'auth-callback',
    component: () => import('@/views/AuthCallbackView.vue'),
    meta: { title: '登录回调' },
  },
  {
    path: '/auth/error',
    name: 'auth-error',
    component: () => import('@/views/AuthErrorView.vue'),
    props: (route) => ({ reason: route.query.reason }),
    meta: { title: '认证失败' },
  },
  roleRoute('teacher', '/teacher', '教师工作台', () => import('@/views/TeacherView.vue'), [
    { path: 'overview', component: () => import('@/views/teacher/TeacherOverviewView.vue'), meta: { title: '实验总览' } },
    { path: 'labs', component: () => import('@/views/teacher/LabListView.vue'), meta: { title: '实验' } },
    { path: 'environments', component: () => import('@/views/teacher/WorkbenchModuleView.vue'), meta: { title: '环境' }, props: { title: '环境', description: '在此查看实验运行环境、配额和健康诊断。' } },
    { path: 'materials', component: () => import('@/views/teacher/MaterialUploadView.vue'), meta: { title: '材料' } },
    { path: 'approvals', component: () => import('@/views/teacher/CandidateApprovalView.vue'), meta: { title: '审批' } },
  ]),
  roleRoute('student', '/student', '学生工作台', () => import('@/views/StudentView.vue'), [
    { path: 'labs', component: () => import('@/views/student/MyLabsView.vue'), meta: { title: '我的实验' } },
    { path: 'environments', component: () => import('@/views/student/EnvironmentEntryView.vue'), meta: { title: '环境控制台' } },
    { path: 'ssh-keys', component: () => import('@/views/student/SshKeysView.vue'), meta: { title: 'SSH 公钥' } },
  ]),
  roleRoute('researcher', '/researcher', '科研工作台', () => import('@/views/ResearcherView.vue'), [
    { path: 'workspaces', component: () => import('@/views/researcher/WorkspaceListView.vue'), meta: { title: '工作空间' } },
  ]),
  roleRoute('admin', '/admin', '管理工作台', () => import('@/views/AdminView.vue'), [
    { path: 'policies', component: () => import('@/views/admin/PolicyListView.vue'), meta: { title: '策略' } },
    { path: 'audit', component: () => import('@/views/admin/AuditLogView.vue'), meta: { title: '审计' } },
  ]),
  {
    path: '/:pathMatch(.*)*',
    name: 'not-found',
    component: () => import('@/views/NotFoundView.vue'),
    meta: { title: '页面不存在' },
  },
]

function getUserRoles(user: ReturnType<typeof useAuth>['user']['value']): AppRole[] {
  if (!user || user.expired) return []
  const roles = user.profile?.roles ?? user.profile?.role
  if (Array.isArray(roles)) return roles.filter((r): r is AppRole => ['teacher', 'student', 'researcher', 'admin'].includes(r))
  if (typeof roles === 'string') {
    const list = roles.split(',').map((r) => r.trim()).filter(Boolean)
    return list.filter((r): r is AppRole => ['teacher', 'student', 'researcher', 'admin'].includes(r))
  }
  return []
}

function collectRequiredRoles(route: RouteRecordNormalized): AppRole[] {
  const roles = new Set<AppRole>()
  route.matched.forEach((record) => {
    record.meta.requiredRoles?.forEach((r) => roles.add(r))
  })
  return Array.from(roles)
}

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes,
})

router.beforeEach(async (to) => {
  const requiredRoles = collectRequiredRoles(to)

  // Fail closed: any role-protected route requires OIDC and an authorized user.
  if (requiredRoles.length > 0) {
    if (!OIDC_ENABLED) {
      return { name: 'home', query: { reason: 'auth-not-configured' } }
    }

    const auth = useAuth()
    await auth.loadUser()

    if (!auth.isAuthenticated.value) {
      // Remember the originally requested path so the callback view can
      // redirect back after a successful OIDC login.
      window.sessionStorage.setItem('auth-return-to', to.fullPath)
      if (IS_FIXTURE) {
        // The fixture OIDC authority cannot complete a redirect; send the
        // user to the home page where the deterministic fixture sign-in
        // panel issues a local identity and returns to this path.
        return { name: 'home' }
      }
      await auth.login()
      return false
    }

    const userRoles = getUserRoles(auth.user.value)
    if (!requiredRoles.some((r) => userRoles.includes(r))) {
      return { name: 'auth-error', query: { reason: 'role_denied' } }
    }
  }

  if (to.meta.title) {
    document.title = to.meta.title === 'LabWeaver' ? 'LabWeaver' : `${to.meta.title} · LabWeaver`
  }
})

export default router
