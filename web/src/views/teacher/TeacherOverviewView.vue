<template>
  <section class="overview" aria-labelledby="overview-heading">
    <header class="page-header">
      <div>
        <p class="eyebrow">教师工作台</p>
        <h1 id="overview-heading">实验总览</h1>
        <p class="page-subtitle">从当前项目开始准备材料、审核候选并发布可用的实验模板。</p>
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
          class="project-card md-card"
          aria-labelledby="current-project-heading"
        >
          <div class="project-card__header">
            <div>
              <p class="eyebrow">当前项目</p>
              <h2 id="current-project-heading">{{ currentProject.name }}</h2>
              <p class="project-description">
                后续材料、审批和发布操作都会绑定到这个项目。
              </p>
            </div>
            <span class="project-id">{{ currentProject.id }}</span>
          </div>

          <nav class="task-links" aria-label="实验工作流">
            <RouterLink
              class="task-link task-link--primary"
              :to="projectLink('/teacher/materials')"
            >
              <span class="task-link__title">准备材料</span>
              <span class="task-link__description">上传题面、初始代码和样例，启动生成任务。</span>
            </RouterLink>
            <RouterLink
              class="task-link"
              :to="projectLink('/teacher/approvals')"
            >
              <span class="task-link__title">查看候选审批</span>
              <span class="task-link__description">复核候选版本，确认后发布环境模板。</span>
            </RouterLink>
            <RouterLink
              class="task-link"
              :to="projectLink('/researcher/workspaces')"
            >
              <span class="task-link__title">管理项目</span>
              <span class="task-link__description">查看项目成员、Work 环境和资源申请。</span>
            </RouterLink>
          </nav>
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
          <p>创建项目后，材料、审批和发布操作都会有明确的归属。</p>
          <RouterLink class="filled-button" to="/researcher/workspaces">创建或选择项目</RouterLink>
        </section>
      </template>
    </AsyncStateView>
  </section>
</template>

<script setup lang="ts">
import { computed, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import { useProjects } from '@/composables/useProjects'

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

const currentProjectId = computed(() => {
  if (routeProjectId.value) return projectContextUnavailable.value ? undefined : routeProjectId.value
  return projects.selectedProject?.id ?? undefined
})

const currentProject = computed(() => projectOptions.value.find((project) => project.id === currentProjectId.value) ?? null)

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

watch(() => projects.selectedProjectId, (projectId) => {
  if (!projectId || (routeProjectId.value && projects.projects.kind !== 'success') || projectContextUnavailable.value || routeProjectId.value === projectId) return
  void router.replace({ query: { ...route.query, projectId: projectId ?? undefined } })
})

function projectLink(path: string) {
  return {
    path,
    query: currentProjectId.value ? { projectId: currentProjectId.value } : undefined,
  }
}
</script>

<style scoped>
.overview {
  display: grid;
  gap: 20px;
}

.page-header,
.project-card__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.page-header h1,
.project-card h2,
.empty-context h2 {
  margin: 0;
  color: var(--md-sys-color-on-surface);
}

.page-header h1 {
  font: var(--md-sys-headline-small);
}

.eyebrow {
  margin: 0 0 5px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-medium);
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.page-subtitle,
.project-description,
.empty-context p {
  margin: 6px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
  line-height: 1.5;
}

.project-card {
  display: grid;
  gap: 24px;
  padding: 24px;
}

.project-card h2 {
  font: var(--md-sys-title-large);
  overflow-wrap: anywhere;
}

.project-id {
  flex: 0 0 auto;
  max-width: 42%;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
  overflow-wrap: anywhere;
  text-align: right;
}

.task-links {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.task-link {
  display: grid;
  gap: 8px;
  min-height: 112px;
  padding: 16px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
  color: var(--md-sys-color-on-surface);
  text-decoration: none;
}

.task-link:hover {
  border-color: var(--md-sys-color-primary);
  background: var(--md-sys-color-primary-container);
}

.task-link--primary {
  border-color: var(--md-sys-color-primary);
}

.task-link__title {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-medium);
}

.task-link__description {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
  line-height: 1.5;
}

.empty-context {
  display: grid;
  justify-items: start;
  gap: 10px;
  padding: 28px;
}

.empty-context h2 {
  font: var(--md-sys-title-large);
}

.filled-button,
.outlined-button {
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
  border: 1px solid var(--md-sys-color-outline);
  color: var(--md-sys-color-primary);
}

@media (max-width: 760px) {
  .project-card__header {
    display: grid;
  }

  .project-id {
    max-width: none;
    text-align: left;
  }

  .task-links {
    grid-template-columns: 1fr;
  }
}
</style>
