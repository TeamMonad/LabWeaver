<template>
  <section class="labs-page" aria-labelledby="labs-heading">
    <header class="page-header">
      <div>
        <p class="eyebrow">教师工作台</p>
        <h1 id="labs-heading">已发布环境模板</h1>
        <p class="page-subtitle">查看当前项目已发布的环境模板和版本信息。</p>
      </div>
    </header>

    <AsyncStateView
      :state="projects.projects"
      empty-text="还没有可用项目，请先创建一个项目。"
      @retry="projects.load"
    >
      <template #success>
        <section
          v-if="currentProject"
          class="release-section md-card"
          aria-labelledby="release-heading"
        >
          <div class="section-heading">
            <div>
              <p class="eyebrow">当前项目</p>
              <h2 id="release-heading">{{ currentProject.name }}</h2>
            </div>
            <RouterLink
              class="outlined-button"
              :to="projectLink('/teacher/approvals')"
            >
              审核与发布
            </RouterLink>
          </div>
          <DiagnosticBanner
            v-if="withdrawalOutcome"
            :code="withdrawalOutcome.code"
            :message="withdrawalOutcome.message"
            :retryable="withdrawalOutcome.retryable"
            :severity="withdrawalOutcome.code.includes('FAILED') || withdrawalOutcome.code.includes('CONFLICT') ? 'error' : 'info'"
          />

          <AsyncStateView
            :state="releases.releases"
            empty-text="当前项目还没有已发布的环境模板。"
            @retry="releases.load"
          >
            <template #success>
              <div v-if="publishedReleases.length > 0" class="release-list" role="list">
                <article v-for="release in publishedReleases" :key="`${release.id}-${release.version}`" class="release-card" role="listitem">
                  <div class="release-card__main">
                    <div class="release-card__title">
                      <h3>环境模板 v{{ release.version }}</h3>
                      <span class="runtime-chip">{{ runtimeLabel(release.runtimeKind) }}</span>
                    </div>
                    <code>{{ release.id }}</code>
                    <p>发布于 {{ formatTimestamp(release.publishedAt) }} · 发布者 {{ release.publishedBy }}</p>
                  </div>
                  <div class="release-card__actions">
                    <span class="release-state">已发布</span>
                    <button
                      type="button"
                      class="text-button"
                      :aria-label="`撤回环境模板 v${release.version}`"
                      :disabled="withdrawalInFlight"
                      @click="openWithdrawal(release)"
                    >
                      撤回
                    </button>
                  </div>
                </article>
              </div>
              <div v-else class="empty-release" role="status">
                <h3>还没有已发布环境模板</h3>
                <p>准备材料并完成候选审批后，已发布版本会显示在这里。</p>
                <RouterLink
                  class="filled-button"
                  :to="projectLink('/teacher/materials')"
                >
                  准备材料
                </RouterLink>
              </div>
            </template>
            <template #empty>
              <div class="empty-release" role="status">
                <h3>还没有已发布环境模板</h3>
                <p>准备材料并完成候选审批后，已发布版本会显示在这里。</p>
                <RouterLink
                  class="filled-button"
                  :to="projectLink('/teacher/materials')"
                >
                  准备材料
                </RouterLink>
              </div>
            </template>
          </AsyncStateView>
        </section>

        <section v-else class="empty-context md-card" aria-labelledby="select-project-heading">
          <h2 id="select-project-heading">{{ projectContextUnavailable ? '项目不可用' : '先选择一个项目' }}</h2>
          <p v-if="projectContextUnavailable">链接中的项目不存在或你无权访问，已停止加载项目数据。请从顶部项目选择器重新选择。</p>
          <p v-else>请使用顶部项目选择器，或打开项目与工作空间创建项目。</p>
          <RouterLink class="outlined-button" to="/researcher/workspaces">打开项目与工作空间</RouterLink>
        </section>
      </template>

      <template #empty>
        <section class="empty-context md-card" aria-labelledby="create-project-heading">
          <h2 id="create-project-heading">还没有项目</h2>
          <p>创建项目后，已发布的实验模板会显示在这里。</p>
          <RouterLink class="filled-button" to="/researcher/workspaces">创建或选择项目</RouterLink>
        </section>
      </template>
    </AsyncStateView>

    <ConfirmDialog
      :open="withdrawalTarget !== null"
      title="撤回环境模板版本？"
      :description="withdrawalDescription"
      confirm-text="撤回版本"
      cancel-text="取消"
      severity="warning"
      @cancel="cancelWithdrawal"
      @confirm="confirmWithdrawal"
    />
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import ConfirmDialog from '@/components/common/ConfirmDialog.vue'
import { useEnvironmentTemplateReleases, withdrawEnvironmentTemplateReleaseByUi } from '@/composables/useEnvironmentTemplateReleases'
import { useProjects } from '@/composables/useProjects'
import { formatTimestamp } from '@/utils/format'
import { extractProblemDetails, makeDiagnostic, type DiagnosticViewModel } from '@/types/async'
import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'

const projects = useProjects()
const projectOptions = computed(() => projects.projects.kind === 'success' ? projects.projects.data : [])

const route = useRoute()
const router = useRouter()

const routeProjectId = computed(() => {
  const value = route.query.projectId
  const projectId = Array.isArray(value) ? value[0] : value
  return typeof projectId === 'string' ? projectId : undefined
})

const projectContextUnavailable = computed(() => Boolean(
  routeProjectId.value
  && projects.projects.kind === 'success'
  && !projectOptions.value.some((project) => project.id === routeProjectId.value),
))

const projectId = computed(() => {
  if (routeProjectId.value) {
    return projects.projects.kind === 'success' && !projectContextUnavailable.value ? routeProjectId.value : undefined
  }
  return projects.selectedProject?.id ?? undefined
})
const currentProject = computed(() => projectOptions.value.find((project) => project.id === projectId.value) ?? null)
const courseId = computed(() => currentProject.value?.courseId ?? undefined)
const releases = useEnvironmentTemplateReleases(projectId, courseId)
const withdrawalTarget = ref<EnvironmentTemplateReleaseViewSchema | null>(null)
const withdrawalOutcome = ref<DiagnosticViewModel | null>(null)
const withdrawalInFlight = ref(false)
const withdrawnReleaseIds = ref<Set<string>>(new Set())

watch(
  [projectOptions, routeProjectId],
  ([availableProjects, fromUrl]) => {
    const preferred = fromUrl
      ? availableProjects.find((project) => project.id === fromUrl)?.id
      : projects.selectedProjectId && availableProjects.some((project) => project.id === projects.selectedProjectId)
        ? projects.selectedProjectId
        : availableProjects[0]?.id
    if (preferred && preferred !== projects.selectedProjectId) projects.select(preferred)
  },
  { immediate: true },
)

watch(() => projects.selectedProjectId, (selectedId) => {
  if (!selectedId || (routeProjectId.value && projects.projects.kind !== 'success') || projectContextUnavailable.value || routeProjectId.value === selectedId) return
  void router.replace({ query: { ...route.query, projectId: selectedId ?? undefined } })
})

watch(projectId, () => {
  withdrawalTarget.value = null
  withdrawalOutcome.value = null
  withdrawnReleaseIds.value = new Set()
})

const publishedReleases = computed(() => releases.releases.kind === 'success'
  ? releases.releases.data.filter((release) => !release.withdrawal && !withdrawnReleaseIds.value.has(release.id))
  : [])

const withdrawalDescription = computed(() => {
  const release = withdrawalTarget.value
  if (!release) return ''
  return `撤回环境模板 v${release.version} 后，新的环境不能使用此版本；已有环境不会自动释放，已有访问授权不会自动撤销，已建立连接不会因撤回自动断开；它们仍受原授权、会话和环境生命周期限制。已有环境可以停止，但启动、重启或依赖此版本的 Work 提交会被拒绝。确认在当前项目中撤回吗？`
})

function openWithdrawal(release: EnvironmentTemplateReleaseViewSchema) {
  if (release.projectId !== projectId.value || release.withdrawal || withdrawnReleaseIds.value.has(release.id)) return
  withdrawalOutcome.value = null
  withdrawalTarget.value = release
}

function cancelWithdrawal() {
  withdrawalTarget.value = null
}

async function confirmWithdrawal() {
  const release = withdrawalTarget.value
  const currentProjectId = projectId.value
  if (!release || !currentProjectId || release.projectId !== currentProjectId || release.withdrawal || withdrawalInFlight.value) return
  withdrawalTarget.value = null
  withdrawalInFlight.value = true
  try {
    const result = await withdrawEnvironmentTemplateReleaseByUi(currentProjectId, release.id, release.version)
    if (result.error) {
      if (projectId.value !== currentProjectId) return
      const problem = extractProblemDetails(result.error)
      withdrawalOutcome.value = makeDiagnostic(
        problem?.diagnosticCode ?? 'RELEASE_WITHDRAW_FAILED',
        problem?.detail ?? '撤回环境模板失败',
        problem?.retryable ?? true,
      )
      return
    }
    if (projectId.value !== currentProjectId) return
    withdrawnReleaseIds.value = new Set([...withdrawnReleaseIds.value, release.id])
    withdrawalOutcome.value = makeDiagnostic('RELEASE_WITHDRAWN', `环境模板 v${release.version} 已撤回。`, false)
    await releases.load()
  } catch (error) {
    if (projectId.value !== currentProjectId) return
    const problem = extractProblemDetails(error)
    withdrawalOutcome.value = makeDiagnostic(
      problem?.diagnosticCode ?? 'RELEASE_WITHDRAW_FAILED',
      problem?.detail ?? '撤回环境模板失败',
      problem?.retryable ?? true,
    )
  } finally {
    withdrawalInFlight.value = false
  }
}

function projectLink(path: string) {
  return {
    path,
    query: projectId.value ? { projectId: projectId.value } : undefined,
  }
}

function runtimeLabel(runtimeKind: 'container' | 'virtual_machine') {
  return runtimeKind === 'container' ? '容器' : '虚拟机'
}

</script>

<style scoped>
.labs-page {
  display: grid;
  gap: 20px;
}

.page-header,
.section-heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.page-header h1,
.section-heading h2,
.release-card h3,
.empty-release h3,
.empty-context h2 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
}

.page-header h1 {
  font: var(--md-sys-headline-small);
}

.section-heading h2 {
  font: var(--md-sys-title-large);
  overflow-wrap: anywhere;
}

.eyebrow {
  margin: 0 0 5px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.page-subtitle,
.release-card p,
.empty-release p,
.empty-context p {
  margin: 6px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
  line-height: 1.5;
}

.release-section {
  display: grid;
  gap: 20px;
  padding: 24px;
}

.release-list {
  display: grid;
  gap: 10px;
}

.release-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
}

.release-card__main {
  display: grid;
  gap: 5px;
  min-width: 0;
}

.release-card__actions {
  display: flex;
  align-items: center;
  flex: 0 0 auto;
  gap: 8px;
}

.release-card__title {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.release-card h3 {
  font: var(--md-sys-title-medium);
}

.release-card code {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-label-medium);
  overflow-wrap: anywhere;
}

.release-card p {
  font: var(--md-sys-body-small);
}

.runtime-chip {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  padding: 0 9px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-secondary-container);
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-small);
}

.empty-release,
.empty-context {
  display: grid;
  justify-items: start;
  gap: 10px;
  padding: 16px 0;
}

.empty-release h3,
.empty-context h2 {
  font: var(--md-sys-title-large);
}

.filled-button,
.outlined-button,
.text-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-height: 40px;
  padding: 0 17px;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  text-decoration: none;
}

.filled-button {
  border: 1px solid var(--md-sys-color-primary);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
}

.outlined-button {
  flex: 0 0 auto;
  border: 1px solid var(--md-sys-color-outline);
  color: var(--md-sys-color-primary);
}

.release-state {
  flex: 0 0 auto;
  color: var(--md-sys-color-on-secondary-container);
  font: var(--md-sys-label-medium);
}

@media (max-width: 680px) {
  .section-heading,
  .release-card {
    align-items: stretch;
    flex-direction: column;
  }

  .section-heading .outlined-button,
  .release-card__actions,
  .release-card .text-button {
    width: 100%;
  }

  .release-card__actions {
    justify-content: space-between;
  }
}
</style>
