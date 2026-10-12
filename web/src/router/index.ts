import { createRouter, createWebHistory } from 'vue-router'
import type { RouteLocationNormalized, RouteRecordRaw } from 'vue-router'
import { useAuth } from '@/composables/useAuth'
import { OIDC_ENABLED } from '@/config'
import {
  NAVIGATION_DETAIL_ROUTES,
  NAVIGATION_GROUPS,
  rolesFromProfile,
  type NavigationGroupId,
  type NavigationItemId,
  type PlatformRole,
} from '@/utils/navigation'

/** Roles that come from the authenticated platform identity. Work is project-scoped. */
export type AppRole = PlatformRole

export interface AppRouteMeta {
  title?: string
  navGroup?: NavigationGroupId
  requiredRoles?: AppRole[]
  requiredCourseScope?: boolean
  /** The page reads or writes one project; the shell keeps `?projectId=` authoritative. */
  projectScoped?: boolean
}

declare module 'vue-router' {
  interface RouteMeta extends AppRouteMeta {}
}

type ViewLoader = () => Promise<unknown>

/**
 * Path, title, roles and project scope live in `utils/navigation.ts`; the
 * router only binds each navigation item to its view. The `Record` keeps the
 * binding exhaustive, so a menu entry cannot exist without a route.
 */
const WORKBENCH_VIEWS: Record<NavigationGroupId, ViewLoader> = {
  teaching: () => import('@/views/TeacherView.vue'),
  student: () => import('@/views/StudentView.vue'),
  work: () => import('@/views/ResearcherView.vue'),
  admin: () => import('@/views/AdminView.vue'),
}

const ITEM_VIEWS: Record<NavigationItemId, ViewLoader> = {
  'teacher-overview': () => import('@/views/teacher/TeacherOverviewView.vue'),
  'teacher-labs': () => import('@/views/teacher/LabListView.vue'),
  'teacher-materials': () => import('@/views/teacher/MaterialUploadView.vue'),
  'teacher-environments': () => import('@/views/teacher/WorkbenchModuleView.vue'),
  'teacher-approvals': () => import('@/views/teacher/CandidateApprovalView.vue'),
  'student-labs': () => import('@/views/student/MyLabsView.vue'),
  'student-environments': () => import('@/views/student/EnvironmentEntryView.vue'),
  'student-ssh-keys': () => import('@/views/student/SshKeysView.vue'),
  'student-results': () => import('@/views/student/ResultListView.vue'),
  workspaces: () => import('@/views/researcher/WorkspaceListView.vue'),
  'work-environments': () => import('@/views/researcher/WorkEnvironmentsView.vue'),
  'work-software': () => import('@/views/researcher/SoftwareConfigView.vue'),
  'work-ai-policy': () => import('@/views/admin/PolicyListView.vue'),
  'work-resources': () => import('@/views/researcher/ResourceRequestView.vue'),
  'admin-resource-approval': () => import('@/views/admin/ResourceApprovalView.vue'),
  'admin-policies': () => import('@/views/admin/PolicyListView.vue'),
  'admin-platform-images': () => import('@/views/admin/PlatformImageView.vue'),
  'admin-gpu-catalog': () => import('@/views/admin/GpuCatalogView.vue'),
  'admin-finance': () => import('@/views/admin/ResourceFinanceView.vue'),
}

const DETAIL_VIEWS: Record<string, ViewLoader> = {
  '/student/results/:runId': () => import('@/views/student/ResultDetailView.vue'),
}

function childPath(basePath: string, path: string): string {
  if (!path.startsWith(`${basePath}/`)) throw new Error(`navigation path ${path} is outside ${basePath}`)
  return path.slice(basePath.length + 1)
}

function workbenchRoutes(): RouteRecordRaw[] {
  return NAVIGATION_GROUPS.map((group) => {
    const children: RouteRecordRaw[] = group.items.map((item) => ({
      path: childPath(group.basePath, item.path),
      component: ITEM_VIEWS[item.id],
      meta: { title: item.label, projectScoped: Boolean(item.projectScoped) },
    }))
    for (const detail of NAVIGATION_DETAIL_ROUTES) {
      const parent = group.items.find((item) => item.id === detail.parentId)
      if (!parent) continue
      const component = DETAIL_VIEWS[detail.path]
      if (!component) throw new Error(`missing view for ${detail.path}`)
      children.push({
        path: childPath(group.basePath, detail.path),
        component,
        meta: { title: detail.title, projectScoped: Boolean(parent.projectScoped) },
      })
    }
    return {
      path: group.basePath,
      name: group.id,
      component: WORKBENCH_VIEWS[group.id],
      meta: { title: group.workbenchTitle, navGroup: group.id, requiredRoles: [...group.allowedRoles] },
      redirect: group.items.length ? group.items[0].path : undefined,
      children,
    }
  })
}

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    name: 'home',
    component: () => import('@/views/HomeView.vue'),
    meta: { title: '进行中', projectScoped: true },
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
  ...workbenchRoutes(),
  {
    path: '/:pathMatch(.*)*',
    name: 'not-found',
    component: () => import('@/views/NotFoundView.vue'),
    meta: { title: '页面不存在' },
  },
]

function getUserRoles(user: ReturnType<typeof useAuth>['user']['value']): AppRole[] {
  if (!user || user.expired) return []
  return rolesFromProfile(user.profile)
}

function collectRequiredRoles(route: RouteLocationNormalized): AppRole[] {
  const roles = new Set<AppRole>()
  route.matched.forEach((record) => {
    record.meta.requiredRoles?.forEach((r) => roles.add(r))
  })
  return Array.from(roles)
}

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes,
  scrollBehavior(_to, _from, savedPosition) {
    if (savedPosition) return savedPosition
    return window.innerWidth <= 760 ? { left: 0, top: 0 } : false
  },
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

  // Project-scoped titles are finalized by the AppShell once the project resolves.
  if (to.meta.title) document.title = `${to.meta.title} · LabWeaver`
})

export default router
