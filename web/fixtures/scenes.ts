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
  { id: 'home-teacher', label: '教师首页 · 无进行中事项', group: '角色首页与导航', role: 'teacher', path: '/', description: '当前项目没有运行中的环境或未完成任务；侧栏承担功能导航。' },
  { id: 'home-in-progress', label: '首页 · 当前项目进行中', group: '角色首页与导航', role: 'teacher', path: '/', projectId: DEFAULT_PROJECT_ID, description: '运行中与失败的环境、按轨道显示当前尝试的生成任务（等待人工审核、第 2 次尝试等待资源审批）和待审批资源申请。' },
  { id: 'home-student', label: '学生首页 · 无进行中事项', group: '角色首页与导航', role: 'student', path: '/', description: '学生身份只看到有权读取的进行中事项。' },
  { id: 'home-admin', label: '管理员首页 · 无进行中事项', group: '角色首页与导航', role: 'admin', path: '/', description: '管理员身份的当前项目进行中视图。' },
  { id: 'project-link-unavailable', label: '项目链接 · 无法访问', group: '角色首页与导航', role: 'student', path: '/researcher/resources', projectId: 'project-not-accessible', description: '链接中的项目不在当前账号的项目列表中，显示恢复入口而不是上一个项目的数据。' },
  { id: 'teacher-project-empty', label: '教师项目 · 空状态', group: '教师任务', role: 'teacher', path: '/teacher/overview', description: '还没有项目时给出下一步入口。' },
  { id: 'teacher-project-data', label: '教师项目 · 长名称', group: '教师任务', role: 'teacher', path: '/teacher/overview', projectId: DEFAULT_PROJECT_ID, description: '展示课程项目、材料、审批和项目管理入口。' },
  { id: 'teacher-template-empty', label: '已发布模板 · 空状态', group: '教师任务', role: 'teacher', path: '/teacher/labs', projectId: DEFAULT_PROJECT_ID, description: '没有发布版本时引导准备材料。' },
  { id: 'teacher-review', label: '审核与发布 · 待审核版本', group: '教师任务', role: 'teacher', path: '/teacher/approvals', projectId: DEFAULT_PROJECT_ID, description: '从待审核列表打开候选，查看题面材料、环境、运行权限与评分规则。' },
  { id: 'teacher-review-published', label: '审核与发布 · 已发布', group: '教师任务', role: 'teacher', path: '/teacher/approvals', projectId: DEFAULT_PROJECT_ID, description: '有发布记录的版本单独列出；打开后发布版本与当前候选一致时不再提供批准。' },
  { id: 'teacher-review-empty', label: '审核与发布 · 没有待审核', group: '教师任务', role: 'teacher', path: '/teacher/approvals', projectId: DEFAULT_PROJECT_ID, description: '没有待审核版本时的下一步说明。' },
  { id: 'teacher-template-data', label: '已发布模板 · 有数据', group: '教师任务', role: 'teacher', path: '/teacher/labs', projectId: DEFAULT_PROJECT_ID, description: '展示容器与虚拟机模板版本。' },
  { id: 'student-env-empty', label: '学生环境 · 空状态', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, description: '未选择环境时保留创建和输入环境 ID 的入口。' },
  { id: 'student-results', label: '评测结果 · 各种终态', group: '学生评测', role: 'student', path: '/student/results', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '得分、0 分、平台故障和已取消的评测并列显示。' },
  { id: 'student-result-score', label: '评测详情 · 得分', group: '学生评测', role: 'student', path: '/student/results/evaluation-run-score', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '确定性总分与不计分的目标建议分开显示。' },
  { id: 'student-result-zero', label: '评测详情 · 0 分', group: '学生评测', role: 'student', path: '/student/results/evaluation-run-zero', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '编译失败得到确定性的 0 分。' },
  { id: 'student-result-platform', label: '评测详情 · 平台故障', group: '学生评测', role: 'student', path: '/student/results/evaluation-run-platform', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '平台执行失败，不显示为 0 分。' },
  { id: 'student-result-running', label: '评测详情 · 进行中', group: '学生评测', role: 'student', path: '/student/results/evaluation-run-pending', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '重试中的评分步骤等待资源审批；上一次未完成的尝试只计入历史，不显示分数或百分比。' },
  { id: 'student-env-ready', label: '学生环境 · 已就绪', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '端点健康并可签发访问授权。' },
  { id: 'student-env-stopped', label: '学生环境 · 已停止', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-stopped', environmentState: 'stopped', description: '停止状态下突出启动动作。' },
  { id: 'student-env-deleted', label: '学生环境 · 已删除', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-deleted', environmentState: 'deleted', description: '删除后明确说明不能继续打开控制台。' },
  { id: 'student-env-failed', label: '学生环境 · 失败诊断', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-failed', environmentState: 'failed', description: '失败操作展示诊断和可重试动作。' },
  { id: 'student-env-viewer', label: '学生环境 · 只读成员', group: '学生环境', role: 'student', path: '/student/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-physics-ready', environmentState: 'ready', description: '项目职责只有查看：生命周期、访问授权和提交入口关闭，并说明缺少的职责。' },
  { id: 'resource-empty', label: '资源申请 · 空状态', group: '资源与费用', role: 'student', path: '/researcher/resources', projectId: DEFAULT_PROJECT_ID, description: '资源申请与使用授权均为空。' },
  { id: 'resource-processing', label: '资源申请 · 处理中', group: '资源与费用', role: 'student', path: '/researcher/resources', projectId: DEFAULT_PROJECT_ID, description: '展示审核中和分配中的请求。' },
  { id: 'resource-authorized', label: '资源申请 · 已授权', group: '资源与费用', role: 'student', path: '/researcher/resources', projectId: DEFAULT_PROJECT_ID, description: '展示已授权资源和续期、回收确认入口。' },
  { id: 'budget-empty', label: '预算 · 未配置', group: '资源与费用', role: 'admin', path: '/admin/resource-finance', projectId: DEFAULT_PROJECT_ID, description: '项目没有预算时显示创建表单。' },
  { id: 'budget-large', label: '预算 · 大金额与费用', group: '资源与费用', role: 'admin', path: '/admin/resource-finance', projectId: DEFAULT_PROJECT_ID, description: '展示大金额、六位小数和待结算费用。' },
  { id: 'approval-filter-empty', label: '资源审批 · 筛选无匹配', group: '管理员任务', role: 'admin', path: '/admin/resource-approval', description: '列表有数据，选择状态过滤后可看到无匹配提示。' },
  { id: 'platform-images-catalog', label: '平台镜像 · 目录与操作', group: '管理员任务', role: 'admin', path: '/admin/platform-images', description: '展示容器与虚拟机基础镜像、release 影响提示以及注册、重新固定、停用和归档导入入口。' },
  { id: 'gpu-catalog', label: 'GPU 目录 · 目录与新建', group: '管理员任务', role: 'admin', path: '/admin/gpu-catalog', description: '展示 GPU class 目录条目并保留新建目录项表单。' },
  { id: 'platform-configuration-unverified', label: '平台配置 · 应用失败待核对', group: '管理员任务', role: 'admin', path: '/admin/platform-configuration', description: 'desired 领先于最后验证的 applied，最近一次应用在应用阶段失败，需要核对外部结果。' },
  { id: 'platform-configuration-unconfigured', label: '平台配置 · 尚未配置', group: '管理员任务', role: 'admin', path: '/admin/platform-configuration', description: '服务端尚无 desired，编辑器以服务端初始模板作为草稿。' },
  { id: 'platform-configuration-unsupported', label: '平台配置 · 尚无执行侧实现', group: '管理员任务', role: 'admin', path: '/admin/platform-configuration', description: '已保存的 desired 含 capabilities、预授权、非 0 UID 与 builder entitlement；页面标注尚无执行侧实现，禁止登记与应用。' },
  { id: 'workspace-archive', label: '项目与工作 · 归档确认', group: '项目与工作', role: 'teacher', path: '/researcher/workspaces', projectId: DEFAULT_PROJECT_ID, description: '保留项目归属，并检查危险归档确认文案。' },
  { id: 'work-launcher', label: 'Work 环境 · 无模型手工创建', group: '项目与工作', role: 'student', path: '/researcher/environments', projectId: DEFAULT_PROJECT_ID, description: '项目没有可用模型：Agent 关闭并说明原因，仍可手工选择模板创建 Work。' },
  { id: 'work-launcher-empty', label: 'Work 环境 · 没有模板', group: '项目与工作', role: 'student', path: '/researcher/environments', projectId: DEFAULT_PROJECT_ID, description: '没有已发布模板且没有模型时的下一步说明。' },
  { id: 'work-upgrade', label: 'Work 环境 · 连接指引与模板升级', group: '项目与工作', role: 'student', path: '/researcher/environments', projectId: DEFAULT_PROJECT_ID, environmentId: 'env-workbench', environmentState: 'ready', description: '连接状态与 guest 指引来自平台；容器 Work 可确认后升级到同一模板系列的新版本。' },
  { id: 'workspace-members', label: '项目成员 · 成员管理', group: '项目与工作', role: 'teacher', path: '/researcher/workspaces', projectId: DEFAULT_PROJECT_ID, description: '展示可读成员名称和移除确认入口。' },
  { id: 'workspace-viewer', label: '项目成员 · 只读成员', group: '项目与工作', role: 'student', path: '/researcher/workspaces', projectId: DEFAULT_PROJECT_ID, description: '项目职责只有查看：编辑、归档和成员管理关闭，并说明缺少的职责。' },
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
