<template>
  <div class="workspace-page">
    <header class="page-header">
      <div>
        <h2>项目与工作空间</h2>
        <p class="page-subtitle">Project 是 Work、Agent 和资源申请的共同归属。创建项目后再启动具体工作环境。</p>
      </div>
      <button type="button" class="filled-button" @click="createOpen = true">
        <SvgIcon name="add" size="sm" aria-hidden="true" />
        新建项目
      </button>
    </header>

    <DiagnosticBanner
      v-if="projects.outcome"
      :code="projects.outcome.diagnostic.code"
      :message="projects.outcome.diagnostic.message"
      :retryable="projects.outcome.diagnostic.retryable"
      :severity="projects.outcome.kind === 'error' ? 'error' : 'info'"
      @retry="projects.load"
    />

    <section class="workspace-layout">
      <section class="project-list md-card" aria-labelledby="project-list-heading">
        <div class="section-heading">
          <div>
            <h3 id="project-list-heading">我的项目</h3>
            <p>项目成员和状态会随平台更新。</p>
          </div>
          <button type="button" class="icon-button" aria-label="刷新项目" :disabled="projects.projects.kind === 'loading'" @click="projects.load">
            <SvgIcon name="refresh" size="sm" aria-hidden="true" />
          </button>
        </div>

        <AsyncStateView :state="projects.projects" empty-text="还没有项目，先创建一个项目。" @retry="projects.load">
          <template #success="{ data }">
            <div class="project-items" role="listbox" aria-label="项目列表">
              <button
                v-for="project in data"
                :key="project.id"
                type="button"
                class="project-item"
                :class="{ 'project-item--selected': project.id === projects.selectedProjectId }"
                role="option"
                :aria-selected="project.id === projects.selectedProjectId"
                @click="selectProject(project.id)"
              >
                <span class="project-item__main">
                  <strong>{{ project.name }}</strong>
                </span>
                <span class="project-item__meta">
                  <span class="state-chip" :class="`state-chip--${project.state}`">{{ project.state === 'active' ? '可用' : '已归档' }}</span>
                  <small>{{ project.courseId ? '课程项目' : '独立项目' }}</small>
                </span>
              </button>
            </div>
          </template>
        </AsyncStateView>
      </section>

      <section class="project-detail md-card" aria-labelledby="project-detail-heading">
        <div v-if="routeProjectInvalid" class="detail-empty" role="status">
          <SvgIcon name="link_off" size="xl" aria-hidden="true" />
          <h3>项目链接无效</h3>
          <p>当前账号无法访问链接指定的项目，请从左侧项目列表选择可访问的项目。</p>
        </div>
        <template v-else-if="selectedProject">
          <div class="section-heading">
            <div>
              <p class="eyebrow">Project 详情</p>
              <h3 id="project-detail-heading">{{ selectedProject.name }}</h3>
            </div>
            <div class="project-detail-actions">
              <RouterLink class="outlined-button small" :to="{ path: '/researcher/ai-policy', query: { projectId: selectedProject.id } }">项目 AI 设置</RouterLink>
              <span class="state-chip" :class="`state-chip--${selectedProject.state}`">{{ selectedProject.state === 'active' ? '可用' : '已归档' }}</span>
            </div>
          </div>

          <form class="project-form" @submit.prevent="saveProject">
            <label>
              <span>项目名称</span>
              <input v-model="editName" class="text-input" required maxlength="160" />
            </label>
            <label>
              <span>描述</span>
              <textarea v-model="editDescription" class="text-input" rows="3" maxlength="2000" />
            </label>
            <div class="readonly-meta">
              <span>课程关联</span><span>{{ selectedProject.courseId ? '课程项目' : '独立项目' }}</span>
              <details v-if="selectedProject.courseId" class="advanced-details">
                <summary>查看课程标识</summary>
                <code>{{ selectedProject.courseId }}</code>
              </details>
            </div>
            <details class="advanced-details">
              <summary>查看项目标识</summary>
              <code>{{ selectedProject.id }}</code>
            </details>
            <div class="form-actions">
              <button type="submit" class="filled-button" :disabled="!canSave || projects.acting !== null">保存项目</button>
              <button
                v-if="selectedProject.state === 'active'"
                type="button"
                class="outlined-button danger-button"
                :disabled="projects.acting !== null"
                @click="openArchiveConfirmation"
              >
                归档项目
              </button>
            </div>
          </form>

          <section class="members-section" aria-labelledby="members-heading">
            <div class="section-heading section-heading--compact">
              <div>
                <h4 id="members-heading">项目成员</h4>
                <p>成员可访问范围会随项目状态更新。</p>
              </div>
              <button type="button" class="icon-button" aria-label="刷新成员" :disabled="members.acting !== null" @click="refreshMembers">
                <SvgIcon name="refresh" size="sm" aria-hidden="true" />
              </button>
            </div>
            <AsyncStateView :state="members.memberships" empty-text="暂无其他项目成员。" @retry="members.load">
              <template #success="{ data }">
                <ul class="member-list">
                  <li v-for="member in data" :key="member.actorId" class="member-row">
                    <span>
                      <strong>{{ memberDisplayName(member) }}</strong>
                      <small>{{ member.username ? `账号：${member.username}` : '账号资料待同步' }} · {{ projectRoleLabel(member.role) }} · {{ membershipStateLabel(member.state) }}</small>
                      <details class="advanced-details">
                        <summary>查看高级详情</summary>
                        <code>Actor ID：{{ member.actorId }}</code>
                      </details>
                    </span>
                    <button v-if="member.actorId !== selectedProject?.ownerActorId" type="button" class="text-button" :disabled="members.acting !== null" @click="openRemoveMemberConfirmation(member)">移除</button>
                  </li>
                </ul>
              </template>
            </AsyncStateView>
            <form v-if="selectedProject.state === 'active'" class="member-form" @submit.prevent="addMember">
              <div v-if="canSearchOrganizationDirectory" class="directory-picker">
                <label>
                  <span>查找组织账号</span>
                  <input
                    v-model="directoryQuery"
                    class="text-input"
                    minlength="1"
                    maxlength="128"
                    placeholder="输入姓名或用户名后搜索"
                    @input="clearDirectorySelection"
                    @keydown.enter.prevent="searchDirectory"
                  />
                </label>
                <button type="button" class="outlined-button" :disabled="!directoryQuery.trim() || directory.users.kind === 'loading'" @click="searchDirectory">搜索账号</button>
                <AsyncStateView :state="directory.users" loading-text="正在查找组织账号…" @retry="searchDirectory">
                  <template #success="{ data }">
                    <div v-if="data.items.length === 0" class="directory-empty" role="status">没有找到匹配的组织账号，请检查姓名或用户名。</div>
                    <template v-else>
                      <ul class="directory-results" aria-label="组织账号搜索结果">
                        <li v-for="user in data.items" :key="user.username">
                          <button
                            type="button"
                            class="directory-result"
                            :class="{ 'directory-result--selected': selectedDirectoryUser?.username === user.username }"
                            :disabled="!user.enabled"
                            @click="selectDirectoryUser(user)"
                          >
                            <strong>{{ directoryUserDisplayName(user) }}</strong>
                            <small>{{ user.username }} · {{ user.enabled ? '可用' : '已停用，不能添加' }}</small>
                          </button>
                        </li>
                      </ul>
                      <nav v-if="data.page > 1 || data.hasMore" class="directory-pagination" aria-label="组织账号分页">
                        <button type="button" class="text-button" :disabled="data.page <= 1 || directory.users.kind === 'loading'" @click="loadDirectoryPage(data.page - 1)">上一页</button>
                        <span>第 {{ data.page }} 页</span>
                        <button type="button" class="text-button" :disabled="!data.hasMore || directory.users.kind === 'loading'" @click="loadDirectoryPage(data.page + 1)">下一页</button>
                      </nav>
                    </template>
                  </template>
                </AsyncStateView>
                <p v-if="selectedDirectoryUser" class="directory-selection" role="status">已选择：{{ directoryUserDisplayName(selectedDirectoryUser) }}（{{ selectedDirectoryUser.username }}）</p>
              </div>
              <label v-else>
                <span>成员账号用户名</span>
                <input v-model="memberUsername" class="text-input" placeholder="输入完整用户名" required />
                <small class="field-hint">个人 Work 仅支持输入已知用户名，平台不会枚举组织账号。</small>
              </label>
              <label>
                <span>角色</span>
                <select v-model="memberRole" class="text-input">
                  <option value="student">学生</option>
                  <option value="teacher">教师</option>
                </select>
              </label>
              <button type="submit" class="outlined-button" :disabled="!canAddMember">添加成员</button>
            </form>
            <DiagnosticBanner
              v-if="members.outcome"
              :code="members.outcome.diagnostic.code"
              :message="members.outcome.diagnostic.message"
              :retryable="members.outcome.diagnostic.retryable"
              :severity="members.outcome.kind === 'error' ? 'error' : 'info'"
              @retry="members.load"
            />
          </section>

          <section class="work-section" aria-labelledby="work-heading">
            <div class="section-heading section-heading--compact">
              <div>
                <h4 id="work-heading">Work 环境</h4>
                <p>查看这个项目中的工作环境，或为项目申请新的 Work 资源。</p>
              </div>
              <div class="section-actions">
                <button
                  type="button"
                  class="icon-button"
                  aria-label="刷新 Work 环境"
                  :disabled="workEnvironments.environments.kind === 'loading'"
                  @click="refreshWork"
                >
                  <SvgIcon name="refresh" size="sm" aria-hidden="true" />
                </button>
                <RouterLink
                  v-if="selectedProject.state === 'active'"
                  class="filled-button small"
                  :to="{ path: '/researcher/resources', query: { projectId: selectedProjectId } }"
                >
                  <SvgIcon name="add" size="sm" aria-hidden="true" />
                  新建 Work
                </RouterLink>
              </div>
            </div>
            <AsyncStateView
              :state="workResources.requests"
              loading-text="加载项目环境资源申请…"
              @retry="workResources.load"
            >
              <template #empty />
              <template #success>
                <p
                  v-for="request in failedEnvironmentResourceRequests"
                  :key="request.id"
                  role="alert"
                >
                  环境资源申请失败。{{ resourceAllocationFailureMessage(request.diagnosticCode) }}
                  <RouterLink
                    class="text-button"
                    :to="{ path: '/researcher/resources', query: { projectId: selectedProjectId } }"
                  >
                    处理资源申请
                  </RouterLink>
                </p>
              </template>
            </AsyncStateView>
            <AsyncStateView :state="workEnvironments.environments" empty-text="这个项目还没有 Work 环境。" @retry="workEnvironments.load">
              <template #success="{ data }">
                <ul class="work-list">
                  <li v-for="environment in data" :key="environment.id" class="work-row">
                    <div class="work-row__main">
                      <strong>{{ environment.displayLabel }}</strong>
                      <small>{{ environment.runtimeKind === 'container' ? '容器' : '虚拟机' }} · 更新于 {{ formatTimestamp(environment.updatedAt) }}</small>
                      <details class="advanced-details">
                        <summary>查看环境标识</summary>
                        <code>{{ environment.id }}</code>
                      </details>
                    </div>
                    <div class="work-row__actions">
                      <span class="state-chip" :class="`state-chip--${environment.observedState}`">{{ environmentStateLabel(environment.observedState) }}</span>
                      <RouterLink
                        class="text-button"
                        :to="{ path: '/researcher/environments', query: { environmentId: environment.id, projectId: selectedProjectId } }"
                      >
                        打开
                      </RouterLink>
                    </div>
                  </li>
                </ul>
              </template>
            </AsyncStateView>
          </section>
        </template>
        <div v-else class="detail-empty">
          <SvgIcon name="folder_open" size="xl" aria-hidden="true" />
          <h3>选择一个项目</h3>
          <p>项目创建后，Work 环境、Agent 配置和资源申请都会绑定到该项目。</p>
        </div>
      </section>
    </section>

    <ConfirmDialog
      :open="destructiveConfirmation !== null"
      :title="confirmationTitle"
      :description="confirmationDescription"
      confirm-text="确认"
      severity="error"
      @cancel="destructiveConfirmation = null"
      @confirm="confirmDestructiveAction"
    />

    <div v-if="createOpen" class="modal-backdrop" role="presentation" @click.self="createOpen = false">
      <section class="modal-card md-card" role="dialog" aria-modal="true" aria-labelledby="create-project-heading">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Project</p>
            <h3 id="create-project-heading">新建项目</h3>
          </div>
          <button type="button" class="icon-button" aria-label="关闭" @click="createOpen = false"><SvgIcon name="close" size="sm" aria-hidden="true" /></button>
        </div>
        <form class="project-form" @submit.prevent="submitCreate">
          <label>
            <span>项目名称</span>
            <input v-model="newName" class="text-input" maxlength="160" required autofocus />
          </label>
          <label>
            <span>描述</span>
            <textarea v-model="newDescription" class="text-input" rows="3" maxlength="2000" />
          </label>
          <label>
            <span>课程 ID（可选）</span>
            <input v-model="newCourseId" class="text-input" placeholder="独立科研项目可留空" />
          </label>
          <div class="form-actions">
            <button type="button" class="outlined-button" @click="createOpen = false">取消</button>
            <button type="submit" class="filled-button" :disabled="!newName.trim() || projects.acting !== null">创建项目</button>
          </div>
        </form>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useProjectWorkEnvironments } from '@/composables/useProjectWorkEnvironments'
import { useProjectResources } from '@/composables/useProjectResources'
import { useAuth } from '@/composables/useAuth'
import { useOrganizationDirectoryUsers, useProjectMemberships, useProjects } from '@/composables/useProjects'
import type { OrganizationUser, ProjectMembershipSchema, ProjectSchema } from '@/generated/contracts'
import { formatTimestamp } from '@/utils/format'
import { environmentStateLabel, membershipStateLabel, projectRoleLabel, resourceAllocationFailureMessage } from '@/utils/stateLabels'
import { hasAnyRole, rolesFromProfile } from '@/utils/navigation'

const route = useRoute()
const router = useRouter()
const projects = useProjects()
const auth = useAuth()
const currentRoles = computed(() => rolesFromProfile(auth.user.value?.profile))
const canSearchOrganizationDirectory = computed(() => hasAnyRole(currentRoles.value, ['teacher', 'admin']))
const routeProjectId = computed(() => {
  const id = typeof route.query.projectId === 'string' ? route.query.projectId.trim() : ''
  return id || null
})
const routeProjectInvalid = computed(() => {
  if (!routeProjectId.value || projects.projects.kind !== 'success') return false
  return !projects.projects.data.some((project) => project.id === routeProjectId.value)
})
const routeProjectPending = computed(() => Boolean(
  routeProjectId.value
  && projects.projects.kind === 'success'
  && projects.selectedProjectId !== routeProjectId.value,
))
const projectSelectionBlocked = computed(() => projects.projects.kind !== 'success' || routeProjectInvalid.value || routeProjectPending.value)
const selectedProjectId = computed(() => projectSelectionBlocked.value ? null : projects.selectedProjectId)
const selectedProject = computed(() => projectSelectionBlocked.value ? null : projects.selectedProject)
const members = useProjectMemberships(selectedProjectId)
const directory = useOrganizationDirectoryUsers()
const workEnvironments = useProjectWorkEnvironments(selectedProjectId)
const workResources = useProjectResources(selectedProjectId)
const failedEnvironmentResourceRequests = computed(() => workResources.requests.kind === 'success'
  ? workResources.requests.data.filter((request) => request.projectId === selectedProjectId.value
    && request.target.kind === 'environment'
    && request.state === 'active'
    && resourceAllocationFailureMessage(request.diagnosticCode))
  : [])

function refreshWork() {
  void workEnvironments.load()
  void workResources.load()
}

const createOpen = ref(false)
const newName = ref('')
const newDescription = ref('')
const newCourseId = ref('')
const editName = ref('')
const editDescription = ref('')
const memberUsername = ref('')
const directoryQuery = ref('')
const selectedDirectoryUser = ref<OrganizationUser | null>(null)
const memberRole = ref<'student' | 'teacher'>('student')
const canSave = computed(() => Boolean(selectedProject.value && editName.value.trim()))
const canAddMember = computed(() => Boolean(
  selectedProject.value?.state === 'active'
  && members.acting === null
  && (canSearchOrganizationDirectory.value ? selectedDirectoryUser.value?.enabled && memberUsername.value.trim() : memberUsername.value.trim()),
))
type WorkspaceConfirmation =
  | { kind: 'archive'; project: ProjectSchema }
  | { kind: 'remove'; member: ProjectMembershipSchema; projectId: string; projectName: string }
const destructiveConfirmation = ref<WorkspaceConfirmation | null>(null)
const confirmationTitle = computed(() => destructiveConfirmation.value?.kind === 'archive' ? '归档项目' : '移除项目成员')
const confirmationDescription = computed(() => {
  const confirmation = destructiveConfirmation.value
  if (!confirmation) return ''
  return confirmation.kind === 'archive'
    ? `确认归档项目“${confirmation.project.name}”？归档会停止新的项目操作，但会保留已有记录。`
    : `确认从项目“${confirmation.projectName}”移除成员 ${memberDisplayName(confirmation.member)}？移除后该成员将失去此项目的访问权限。`
})

watch(
  () => selectedProject.value,
  (project, previousProject) => {
    if (project?.id !== previousProject?.id) destructiveConfirmation.value = null
    editName.value = project?.name ?? ''
    editDescription.value = project?.description ?? ''
  },
  { immediate: true },
)

watch(
  selectedProjectId,
  () => {
    directoryQuery.value = ''
    memberUsername.value = ''
    selectedDirectoryUser.value = null
    directory.clear()
  },
)

watch(
  [() => projects.projects, routeProjectId],
  ([state, requestedProjectId]) => {
    if (state.kind !== 'success' || !requestedProjectId) return
    if (!state.data.some((project) => project.id === requestedProjectId)) return
    if (projects.selectedProjectId !== requestedProjectId) projects.select(requestedProjectId)
  },
  { immediate: true },
)

watch(
  () => projects.selectedProjectId,
  (projectId) => {
    if (!projectId || routeProjectInvalid.value || route.query.projectId === projectId) return
    void router.replace({ query: { ...route.query, projectId } })
  },
)

function selectProject(projectId: string) {
  projects.select(projectId)
  if (route.query.projectId !== projectId) {
    void router.replace({ query: { ...route.query, projectId } })
  }
}

function memberDisplayName(member: ProjectMembershipSchema): string {
  return member.displayName?.trim() || '账号资料待同步'
}

function directoryUserDisplayName(user: OrganizationUser): string {
  return user.displayName.trim() || '账号资料待同步'
}

function clearDirectorySelection() {
  selectedDirectoryUser.value = null
  memberUsername.value = ''
  directory.clear()
}

function searchDirectory() {
  selectedDirectoryUser.value = null
  memberUsername.value = ''
  void directory.load(directoryQuery.value, 1)
}

function loadDirectoryPage(page: number) {
  selectedDirectoryUser.value = null
  memberUsername.value = ''
  void directory.load(directoryQuery.value, page)
}

function selectDirectoryUser(user: OrganizationUser) {
  if (!user.enabled) return
  selectedDirectoryUser.value = user
  memberUsername.value = user.username
}

async function submitCreate() {
  const created = await projects.create(newName.value, newDescription.value, newCourseId.value)
  if (!created) return
  createOpen.value = false
  newName.value = ''
  newDescription.value = ''
  newCourseId.value = ''
}

async function saveProject() {
  const project = projects.selectedProject
  if (!project) return
  await projects.update(project, { name: editName.value.trim(), description: editDescription.value.trim() || null })
}

function openArchiveConfirmation() {
  const project = projects.selectedProject
  if (!project) return
  destructiveConfirmation.value = { kind: 'archive', project }
}

function refreshMembers() {
  clearDirectorySelection()
  void members.load()
}

async function addMember() {
  const username = memberUsername.value.trim()
  if (!canAddMember.value || !username) return
  const ok = await members.add({ username, role: memberRole.value })
  if (ok) {
    memberUsername.value = ''
    selectedDirectoryUser.value = null
    directory.clear()
  }
}

function openRemoveMemberConfirmation(member: ProjectMembershipSchema) {
  const project = projects.selectedProject
  if (!project) return
  destructiveConfirmation.value = {
    kind: 'remove',
    member,
    projectId: project.id,
    projectName: project.name,
  }
}

async function confirmDestructiveAction() {
  const confirmation = destructiveConfirmation.value
  destructiveConfirmation.value = null
  if (!confirmation) return
  if (confirmation.kind === 'archive') {
    await projects.archive(confirmation.project)
  } else if (projects.selectedProjectId === confirmation.projectId) {
    await members.remove(confirmation.member.actorId, confirmation.member)
  }
}
</script>

<style scoped>
.workspace-page { display: grid; gap: 20px; }
.page-header, .section-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.page-header h2, .section-heading h3, .section-heading h4 { margin: 0; color: var(--md-sys-color-on-surface); }
.page-header h2 { font: var(--md-sys-headline-small); }
.section-heading h3 { font: var(--md-sys-title-large); }
.section-heading h4 { font: var(--md-sys-title-medium); }
.page-subtitle, .section-heading p, .eyebrow { margin: 5px 0 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.eyebrow { font: var(--md-sys-label-medium); text-transform: uppercase; letter-spacing: .05em; }
.workspace-layout { display: grid; grid-template-columns: minmax(240px, .8fr) minmax(0, 1.4fr); gap: 20px; align-items: start; }
.project-list, .project-detail { padding: 20px; }
.project-detail-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; justify-content: flex-end; }
.project-items { display: grid; gap: 6px; margin-top: 18px; }
.project-item { width: 100%; display: flex; justify-content: space-between; gap: 12px; padding: 14px; border: 1px solid transparent; border-radius: var(--md-sys-shape-medium); background: transparent; color: var(--md-sys-color-on-surface); text-align: left; cursor: pointer; }
.project-item:hover { background: var(--md-sys-color-surface-container-high); }
.project-item--selected { border-color: var(--md-sys-color-primary); background: var(--md-sys-color-primary-container); }
.project-item__main, .project-item__meta, .member-row > span { display: grid; gap: 4px; min-width: 0; }
.project-item__main strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.project-item small, .member-row small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.project-item__meta { justify-items: end; text-align: right; }
.state-chip { display: inline-flex; align-items: center; width: fit-content; padding: 3px 8px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-small); background: var(--md-sys-color-surface-variant); color: var(--md-sys-color-on-surface-variant); }
.state-chip--active { background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); }
.state-chip--archived { background: var(--md-sys-color-surface-variant); }
.project-detail { min-height: 520px; }
.project-form { display: grid; gap: 14px; margin-top: 20px; }
.project-form label, .member-form label { display: grid; gap: 6px; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.field-hint { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.text-input { box-sizing: border-box; width: 100%; min-height: 40px; padding: 9px 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface); color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
textarea.text-input { resize: vertical; }
.readonly-meta { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px 14px; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.readonly-meta code { color: var(--md-sys-color-on-surface); overflow-wrap: anywhere; }
.form-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 10px; }
.filled-button, .outlined-button, .text-button { min-height: 40px; padding: 0 17px; border-radius: var(--md-sys-shape-full); font: var(--md-sys-label-large); cursor: pointer; text-decoration: none; }
.filled-button.small { min-height: 32px; padding: 0 12px; font: var(--md-sys-label-medium); }
.filled-button { display: inline-flex; align-items: center; justify-content: center; gap: 8px; border: 1px solid var(--md-sys-color-primary); background: var(--md-sys-color-primary); color: var(--md-sys-color-on-primary); }
.outlined-button { border: 1px solid var(--md-sys-color-outline); background: transparent; color: var(--md-sys-color-primary); }
.outlined-button.small { min-height: 32px; padding: 0 11px; font: var(--md-sys-label-medium); }
.text-button { min-height: 32px; border: 0; background: transparent; color: var(--md-sys-color-primary); }
.danger-button { color: var(--md-sys-color-error); border-color: var(--md-sys-color-error); }
.filled-button:disabled, .outlined-button:disabled, .text-button:disabled { opacity: .5; cursor: not-allowed; }
.members-section { display: grid; gap: 14px; margin-top: 30px; padding-top: 24px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.work-section { display: grid; gap: 14px; margin-top: 30px; padding-top: 24px; border-top: 1px solid var(--md-sys-color-outline-variant); }
.section-heading--compact { align-items: center; }
.section-actions, .work-row__actions { display: flex; align-items: center; justify-content: flex-end; gap: 8px; flex-wrap: wrap; }
.work-list { display: grid; gap: 7px; margin: 0; padding: 0; list-style: none; }
.work-row { display: flex; justify-content: space-between; gap: 14px; align-items: center; padding: 13px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.work-row__main { display: grid; gap: 4px; min-width: 0; }
.work-row__main strong { overflow-wrap: anywhere; }
.work-row__main small { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); overflow-wrap: anywhere; }
.advanced-details { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.advanced-details summary { cursor: pointer; }
.advanced-details code { display: block; margin-top: 4px; overflow-wrap: anywhere; }
.member-list { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; }
.member-row { display: flex; justify-content: space-between; gap: 10px; align-items: center; padding: 11px 12px; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); }
.member-form { display: grid; grid-template-columns: minmax(0, 1fr) 150px auto; gap: 10px; align-items: end; }
.directory-picker { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; grid-column: 1 / -1; align-items: end; }
.directory-picker > .async-state-view, .directory-selection { grid-column: 1 / -1; }
.directory-results { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; }
.directory-result { display: grid; width: 100%; gap: 3px; padding: 10px 12px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-low); color: var(--md-sys-color-on-surface); text-align: left; cursor: pointer; }
.directory-result:hover, .directory-result--selected { border-color: var(--md-sys-color-primary); background: var(--md-sys-color-primary-container); }
.directory-result:disabled { opacity: .6; cursor: not-allowed; }
.directory-result small, .directory-empty, .directory-pagination, .directory-selection { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-small); }
.directory-empty { padding: 10px 12px; background: var(--md-sys-color-surface-container-low); }
.directory-pagination { display: flex; align-items: center; justify-content: flex-end; gap: 8px; }
.detail-empty { display: grid; place-items: center; align-content: center; min-height: 420px; gap: 10px; color: var(--md-sys-color-on-surface-variant); text-align: center; }
.detail-empty h3 { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-large); }
.detail-empty p { max-width: 420px; margin: 0; font: var(--md-sys-body-medium); }
.modal-backdrop { position: fixed; inset: 0; z-index: 20; display: grid; place-items: center; padding: 20px; background: rgb(0 0 0 / .45); }
.modal-card { width: min(520px, 100%); padding: 24px; }
.icon-button { display: inline-grid; place-items: center; width: 40px; height: 40px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.icon-button:hover { background: var(--md-sys-color-surface-container-high); }
.icon-button:disabled { opacity: .5; cursor: not-allowed; }
@media (max-width: 760px) { .workspace-layout { grid-template-columns: 1fr; } .project-detail { min-height: 0; } .member-form, .directory-picker { grid-template-columns: 1fr; } .directory-picker > .async-state-view, .directory-selection { grid-column: auto; } .work-row { align-items: flex-start; flex-direction: column; } .work-row__actions { justify-content: flex-start; } .page-header { align-items: stretch; flex-direction: column; } .page-header > .filled-button { width: 100%; } }
</style>
