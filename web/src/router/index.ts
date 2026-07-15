import { createRouter, createWebHistory } from 'vue-router'
import type { RouteRecordRaw, RouteRecordNormalized } from 'vue-router'
import { useAuth } from '@/composables/useAuth'
import { OIDC_ENABLED } from '@/config'

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
    { path: 'evaluations', component: () => import('@/views/teacher/WorkbenchModuleView.vue'), meta: { title: '评测' }, props: { title: '评测', description: '在此查看评测队列、结果与需要教师处理的事项。' } },
    { path: 'resources', component: () => import('@/views/teacher/WorkbenchModuleView.vue'), meta: { title: '资源' }, props: { title: '资源', description: '在此管理实验材料、共享资源和关联版本。' } },
    { path: 'materials', component: () => import('@/views/teacher/MaterialUploadView.vue'), meta: { title: '材料' } },
    { path: 'approvals', component: () => import('@/views/teacher/ApprovalListView.vue'), meta: { title: '审批' } },
  ]),
  roleRoute('student', '/student', '学生工作台', () => import('@/views/StudentView.vue'), [
    { path: '', name: 'student-labs', component: () => import('@/views/student/MyLabsView.vue'), meta: { title: '我的实验' } },
    { path: 'labs', component: () => import('@/views/student/MyLabsView.vue'), meta: { title: '我的实验' } },
    { path: 'environments', component: () => import('@/views/student/EnvironmentEntryView.vue'), meta: { title: '环境控制台' } },
    { path: 'ssh-keys', component: () => import('@/views/student/SshKeysView.vue'), meta: { title: 'SSH 公钥' } },
    { path: 'results', component: () => import('@/views/student/ResultListView.vue'), meta: { title: '结果' } },
  ]),
  roleRoute('researcher', '/researcher', '科研工作台', () => import('@/views/ResearcherView.vue'), [
    { path: 'workspaces', component: () => import('@/views/researcher/WorkspaceListView.vue'), meta: { title: '工作空间' } },
    { path: 'resources', component: () => import('@/views/researcher/ResourceRequestView.vue'), meta: { title: '资源申请' } },
    { path: 'config', component: () => import('@/views/researcher/SoftwareConfigView.vue'), meta: { title: '软件配置' } },
  ]),
  roleRoute('admin', '/admin', '管理工作台', () => import('@/views/AdminView.vue'), [
    { path: 'resources', component: () => import('@/views/admin/ResourceApprovalView.vue'), meta: { title: '资源审批' } },
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
