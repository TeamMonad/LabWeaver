import { computed, ref } from 'vue'

export type FixtureRole = 'teacher' | 'student' | 'admin'

export interface FixtureScene {
  id: string
  label: string
  group: string
  role: FixtureRole
  path: string
  projectId?: string
  environmentId?: string
  environmentState?: string
  description: string
}

const DEFAULT_PROJECT_ID = 'project-physics-lab'

export const fixtureScenes: FixtureScene[] = [
  { id: 'home-teacher', label: '教师首页 · 全部任务', group: '角色首页与导航', role: 'teacher', path: '/', description: '教师身份同时看到教学、项目与工作任务。' },
  { id: 'home-student', label: '学生首页 · 全部任务', group: '角色首页与导航', role: 'student', path: '/', description: '学生身份看到我的实验、项目与工作任务。' },
  { id: 'home-admin', label: '管理员首页 · 全部任务', group: '角色首页与导航', role: 'admin', path: '/', description: '管理员身份同时看到平台管理与项目任务。' },
  { id: 'teacher-project-empty', label: '教师项目 · 空状态', group: '教师任务', role: 'teacher', path: '/teacher/overview', description: '还没有项目时给出下一步入口。' },
  { id: 'teacher-project-data', label: '教师项目 · 长名称', group: '教师任务', role: 'teacher', path: '/teacher/overview', projectId: DEFAULT_PROJECT_ID, description: '展示课程项目、材料、审批和项目管理入口。' },
  { id: 'teacher-template-empty', label: '已发布模板 · 空状态', group: '教师任务', role: 'teacher', path: '/teacher/labs', projectId: DEFAULT_PROJECT_ID, description: '没有发布版本时引导准备材料。' },
  { id: 'teacher-template-data', label: '已发布模板 · 有数据', group: '教师任务', role: 'teacher', path: '/teacher/labs', projectId: DEFAULT_PROJECT_ID, description: '展示容器与虚拟机模板版本。' },
  { id: 'student-env-empty', label: '学生环境 · 空状态', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, description: '未选择环境时保留创建和输入环境 ID 的入口。' },
  { id: 'student-env-ready', label: '学生环境 · 已就绪', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '端点健康并可签发访问授权。' },
  { id: 'student-env-stopped', label: '学生环境 · 已停止', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-stopped', environmentState: 'stopped', description: '停止状态下突出启动动作。' },
  { id: 'student-env-deleted', label: '学生环境 · 已删除', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-deleted', environmentState: 'deleted', description: '删除后明确说明不能继续打开控制台。' },
  { id: 'student-env-failed', label: '学生环境 · 失败诊断', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-failed', environmentState: 'failed', description: '失败操作展示诊断和可重试动作。' },
  { id: 'resource-empty', label: '资源申请 · 空状态', group: '资源与费用', role: 'student', path: '/researcher/resources', projectId: DEFAULT_PROJECT_ID, description: '资源申请与使用授权均为空。' },
  { id: 'resource-processing', label: '资源申请 · 处理中', group: '资源与费用', role: 'student', path: '/researcher/resources', projectId: DEFAULT_PROJECT_ID, description: '展示审核中和分配中的请求。' },
  { id: 'resource-authorized', label: '资源申请 · 已授权', group: '资源与费用', role: 'student', path: '/researcher/resources', projectId: DEFAULT_PROJECT_ID, description: '展示已授权资源和续期、回收确认入口。' },
  { id: 'budget-empty', label: '预算 · 未配置', group: '资源与费用', role: 'admin', path: '/admin/resource-finance', projectId: DEFAULT_PROJECT_ID, description: '项目没有预算时显示创建表单。' },
  { id: 'budget-large', label: '预算 · 大金额与费用', group: '资源与费用', role: 'admin', path: '/admin/resource-finance', projectId: DEFAULT_PROJECT_ID, description: '展示大金额、六位小数和待结算费用。' },
  { id: 'approval-filter-empty', label: '资源审批 · 筛选无匹配', group: '管理员任务', role: 'admin', path: '/admin/resource-approval', description: '列表有数据，选择状态过滤后可看到无匹配提示。' },
  { id: 'workspace-archive', label: '项目与工作 · 归档确认', group: '项目与工作', role: 'teacher', path: '/researcher/workspaces', projectId: DEFAULT_PROJECT_ID, description: '保留项目归属，并检查危险归档确认文案。' },
  { id: 'workspace-members', label: '项目成员 · 成员管理', group: '项目与工作', role: 'teacher', path: '/researcher/workspaces', projectId: DEFAULT_PROJECT_ID, description: '展示可读成员名称和移除确认入口。' },
  { id: 'resource-reclaim', label: '资源授权 · 回收确认', group: '项目与工作', role: 'student', path: '/researcher/resources', projectId: DEFAULT_PROJECT_ID, description: '已授权 Lease 的回收动作会要求确认。' },
]

const sceneId = ref('home-teacher')

const initialLocation = typeof window === 'undefined' ? undefined : new URL(window.location.href)

export const activeScene = computed<FixtureScene>(() => {
  return fixtureScenes.find((scene) => scene.id === sceneId.value) ?? fixtureScenes[0]
})

export function sceneById(id: string | null | undefined): FixtureScene {
  return fixtureScenes.find((scene) => scene.id === id) ?? fixtureScenes[0]
}

export function setFixtureScene(id: string | null | undefined): FixtureScene {
  const scene = sceneById(id)
  sceneId.value = scene.id
  return scene
}

function locationQuery(url: URL): URLSearchParams {
  if (!url.hash.startsWith('#/')) return url.searchParams
  const hashQueryIndex = url.hash.indexOf('?')
  return hashQueryIndex >= 0 ? new URLSearchParams(url.hash.slice(hashQueryIndex + 1)) : new URLSearchParams()
}

function locationPath(url: URL): string | undefined {
  if (!url.hash.startsWith('#/')) return undefined
  const pathWithQuery = url.hash.slice(1)
  const queryIndex = pathWithQuery.indexOf('?')
  const path = queryIndex >= 0 ? pathWithQuery.slice(0, queryIndex) : pathWithQuery
  return path || '/'
}

export function initializeFixtureScene(): FixtureScene {
  if (typeof window === 'undefined') return activeScene.value
  return setFixtureScene(locationQuery(initialLocation ?? new URL(window.location.href)).get('scene'))
}

export function initializeFixturePath(fallback = activeScene.value.path): string {
  if (typeof window === 'undefined') return fallback
  return locationPath(initialLocation ?? new URL(window.location.href)) ?? fallback
}

export function initializeFixtureQuery(scene = activeScene.value): Record<string, string> {
  if (typeof window === 'undefined') {
    return {
      scene: scene.id,
      ...(scene.projectId ? { projectId: scene.projectId } : {}),
      ...(scene.environmentId ? { environmentId: scene.environmentId } : {}),
    }
  }
  const url = initialLocation ?? new URL(window.location.href)
  const query = locationQuery(url)
  const existing = Object.fromEntries(query.entries())
  const useSceneDefaults = !url.hash.startsWith('#/')
  return {
    ...existing,
    scene: scene.id,
    ...(useSceneDefaults && !existing.projectId && scene.projectId ? { projectId: scene.projectId } : {}),
    ...(useSceneDefaults && !existing.environmentId && scene.environmentId ? { environmentId: scene.environmentId } : {}),
  }
}

export function sceneUsesProject(scene: FixtureScene = activeScene.value): boolean {
  return Boolean(scene.projectId)
}

export const fixtureProjectId = DEFAULT_PROJECT_ID
