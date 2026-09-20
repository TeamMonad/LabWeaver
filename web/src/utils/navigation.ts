export const PLATFORM_ROLES = ['teacher', 'student', 'admin'] as const

export type PlatformRole = (typeof PLATFORM_ROLES)[number]

export type NavigationGroupId = 'teaching' | 'student' | 'work' | 'admin'

export type NavigationItemId =
  | 'teacher-overview'
  | 'teacher-labs'
  | 'teacher-materials'
  | 'teacher-environments'
  | 'teacher-approvals'
  | 'student-labs'
  | 'student-environments'
  | 'student-ssh-keys'
  | 'student-results'
  | 'workspaces'
  | 'work-environments'
  | 'work-software'
  | 'work-resources'
  | 'admin-resource-approval'
  | 'admin-policies'
  | 'admin-platform-images'
  | 'admin-finance'
  | 'admin-audit'

export interface NavigationItem {
  id: NavigationItemId
  label: string
  description: string
  path: string
  icon: string
  keywords: string[]
  allowedRoles: readonly PlatformRole[]
  projectScoped?: boolean
}

export interface NavigationGroup {
  id: NavigationGroupId
  label: string
  items: readonly NavigationItem[]
}

export interface AuthorizedNavigationItem extends NavigationItem {
  groupId: NavigationGroupId
  groupLabel: string
}

const TEACHER_ONLY = ['teacher'] as const satisfies readonly PlatformRole[]
const STUDENT_ONLY = ['student'] as const satisfies readonly PlatformRole[]
const ALL_PLATFORM_ROLES = PLATFORM_ROLES
const ADMIN_ONLY = ['admin'] as const satisfies readonly PlatformRole[]

export const NAVIGATION_GROUPS: readonly NavigationGroup[] = [
  {
    id: 'teaching',
    label: '教学管理',
    items: [
      {
        id: 'teacher-overview',
        label: '实验总览',
        description: '准备材料、审核并发布实验版本。',
        path: '/teacher/overview',
        icon: 'dashboard',
        keywords: ['教师', '教学', '实验总览', 'overview'],
        allowedRoles: TEACHER_ONLY,
        projectScoped: true,
      },
      {
        id: 'teacher-labs',
        label: '已发布环境模板',
        description: '查看当前项目已发布的实验和 Work 环境模板。',
        path: '/teacher/labs',
        icon: 'menu_book',
        keywords: ['教师', '实验', '实验列表', 'labs'],
        allowedRoles: TEACHER_ONLY,
        projectScoped: true,
      },
      {
        id: 'teacher-materials',
        label: '创建与生成实验',
        description: '准备教学材料并查看实验生成任务。',
        path: '/teacher/materials',
        icon: 'smart_toy',
        keywords: ['材料', 'Agent', 'AgentRun', '构建', '上传'],
        allowedRoles: TEACHER_ONLY,
        projectScoped: true,
      },
      {
        id: 'teacher-environments',
        label: '教学环境',
        description: '查看课程实验环境和配额，进入已授权控制台。',
        path: '/teacher/environments',
        icon: 'desktop_windows',
        keywords: ['教学环境', '环境', '教师', 'console'],
        allowedRoles: TEACHER_ONLY,
      },
      {
        id: 'teacher-approvals',
        label: '审核与发布',
        description: '审核生成候选并发布可用实验版本。',
        path: '/teacher/approvals',
        icon: 'rule',
        keywords: ['审批', '候选', '发布', 'candidate', 'approval'],
        allowedRoles: TEACHER_ONLY,
        projectScoped: true,
      },
    ],
  },
  {
    id: 'student',
    label: '我的实验',
    items: [
      {
        id: 'student-labs',
        label: '我的实验',
        description: '加入实验、启动环境并查看当前任务。',
        path: '/student/labs',
        icon: 'science',
        keywords: ['学生', '实验', '我的实验', 'labs'],
        allowedRoles: STUDENT_ONLY,
        projectScoped: true,
      },
      {
        id: 'student-environments',
        label: '环境控制台',
        description: '进入已授权的实验环境和终端。',
        path: '/student/environments',
        icon: 'desktop_windows',
        keywords: ['环境', '控制台', '终端', 'ssh', 'console'],
        allowedRoles: STUDENT_ONLY,
        projectScoped: true,
      },
      {
        id: 'student-ssh-keys',
        label: 'SSH 公钥',
        description: '管理连接实验环境所需的 SSH 公钥。',
        path: '/student/ssh-keys',
        icon: 'key',
        keywords: ['学生', 'SSH', '公钥', 'keys'],
        allowedRoles: STUDENT_ONLY,
      },
      {
        id: 'student-results',
        label: '评测结果',
        description: '查看提交记录、评测结果和反馈。',
        path: '/student/results',
        icon: 'fact_check',
        keywords: ['学生', '成绩', '评测', '提交', '结果', 'results'],
        allowedRoles: STUDENT_ONLY,
        projectScoped: true,
      },
    ],
  },
  {
    id: 'work',
    label: '项目与工作',
    items: [
      {
        id: 'workspaces',
        label: '项目与工作空间',
        description: '选择项目，管理成员和 Work 环境。',
        path: '/researcher/workspaces',
        icon: 'workspaces',
        keywords: ['项目', '工作空间', 'Work', 'workspace', 'project'],
        allowedRoles: ALL_PLATFORM_ROLES,
        projectScoped: true,
      },
      {
        id: 'work-environments',
        label: 'Work 环境',
        description: '查看和连接当前项目的 Work 环境。',
        path: '/researcher/environments',
        icon: 'desktop_windows',
        keywords: ['Work', '环境', '控制台', 'console'],
        allowedRoles: ALL_PLATFORM_ROLES,
        projectScoped: true,
      },
      {
        id: 'work-software',
        label: '软件配置',
        description: '为项目生成并审核软件配置计划。',
        path: '/researcher/software',
        icon: 'settings_applications',
        keywords: ['软件', '配置', 'Agent', 'software'],
        allowedRoles: ALL_PLATFORM_ROLES,
        projectScoped: true,
      },
      {
        id: 'work-resources',
        label: '资源申请',
        description: '为当前项目申请 Work 容量并查看使用授权。',
        path: '/researcher/resources',
        icon: 'memory',
        keywords: ['资源', '申请', 'GPU', 'Lease', 'resource'],
        allowedRoles: ALL_PLATFORM_ROLES,
        projectScoped: true,
      },
    ],
  },
  {
    id: 'admin',
    label: '平台管理',
    items: [
      {
        id: 'admin-resource-approval',
        label: '资源审批',
        description: '审核资源申请并管理资源使用授权。',
        path: '/admin/resource-approval',
        icon: 'admin_panel_settings',
        keywords: ['管理员', '资源', '审批', 'Lease', 'admin'],
        allowedRoles: ADMIN_ONLY,
      },
      {
        id: 'admin-policies',
        label: '安全策略',
        description: '管理实验审批、镜像和访问策略。',
        path: '/admin/policies',
        icon: 'policy',
        keywords: ['管理员', '策略', '安全', 'policy'],
        allowedRoles: ADMIN_ONLY,
      },
      {
        id: 'admin-platform-images',
        label: '平台镜像',
        description: '维护沙箱可用的容器与虚拟机基础镜像。',
        path: '/admin/platform-images',
        icon: 'image',
        keywords: ['管理员', '镜像', '基础镜像', 'image', 'digest'],
        allowedRoles: ADMIN_ONLY,
      },
      {
        id: 'admin-finance',
        label: '预算与费用',
        description: '查看项目预算、用量结算和费用调整。',
        path: '/admin/resource-finance',
        icon: 'payments',
        keywords: ['管理员', '预算', '费用', '计费', 'finance'],
        allowedRoles: ADMIN_ONLY,
        projectScoped: true,
      },
      {
        id: 'admin-audit',
        label: '审计日志',
        description: '查看平台操作和资源生命周期记录。',
        path: '/admin/audit',
        icon: 'history',
        keywords: ['管理员', '审计', '日志', 'audit'],
        allowedRoles: ADMIN_ONLY,
      },
    ],
  },
] as const

function roleAlias(value: unknown): PlatformRole | null {
  if (typeof value !== 'string') return null
  const normalized = value.trim()
  if (normalized === 'teacher') return 'teacher'
  if (normalized === 'student') return 'student'
  if (normalized === 'admin' || normalized === 'platform_admin') return 'admin'
  return null
}

/** Normalize the role claim shapes used by BFF and direct OIDC sessions. */
export function normalizeRoles(value: unknown): PlatformRole[] {
  const values = Array.isArray(value)
    ? value
    : typeof value === 'string'
      ? value.split(',')
      : []
  return [...new Set(values.map(roleAlias).filter((role): role is PlatformRole => role !== null))]
}

export function rolesFromProfile(profile: unknown): PlatformRole[] {
  if (!profile || typeof profile !== 'object') return []
  const claims = profile as Record<string, unknown>
  return normalizeRoles(claims.roles ?? claims.role)
}

export function hasAnyRole(roles: readonly PlatformRole[], allowedRoles: readonly PlatformRole[]): boolean {
  return allowedRoles.some((role) => roles.includes(role))
}

export function navigationGroupsForRoles(roles: readonly PlatformRole[]): NavigationGroup[] {
  return NAVIGATION_GROUPS
    .map((group) => ({
      ...group,
      items: group.items.filter((item) => hasAnyRole(roles, item.allowedRoles)),
    }))
    .filter((group) => group.items.length > 0)
}

export function navigationItemsForRoles(roles: readonly PlatformRole[]): AuthorizedNavigationItem[] {
  return navigationGroupsForRoles(roles).flatMap((group) => group.items.map((item) => ({
    ...item,
    groupId: group.id,
    groupLabel: group.label,
  })))
}

export function appendNavigationQuery(
  path: string,
  params: Record<string, string | null | undefined>,
): string {
  const query = Object.entries(params)
    .filter(([, value]) => typeof value === 'string' && value.length > 0)
    .reduce((searchParams, [key, value]) => {
      searchParams.set(key, value as string)
      return searchParams
    }, new URLSearchParams())
    .toString()
  if (!query) return path
  return `${path}${path.includes('?') ? '&' : '?'}${query}`
}

export function navigationTarget(item: NavigationItem, projectId?: string | null): string {
  const normalizedProjectId = projectId?.trim()
  return item.projectScoped && normalizedProjectId
    ? appendNavigationQuery(item.path, { projectId: normalizedProjectId })
    : item.path
}

/**
 * Use the student console only inside a student context. Other platform roles
 * enter the shared project Work console, which is authorized for every role.
 */
export function consoleNavigationTarget(
  roles: readonly PlatformRole[],
  currentPath: string,
  projectId?: string | null,
  environmentId?: string | null,
): string | null {
  const studentContext = currentPath.startsWith('/student') && roles.includes('student')
  const itemId: NavigationItemId = studentContext ? 'student-environments' : 'work-environments'
  const item = navigationItemsForRoles(roles).find((candidate) => candidate.id === itemId)
  if (!item) return null
  return appendNavigationQuery(navigationTarget(item, projectId), { environmentId: environmentId?.trim() || undefined })
}
