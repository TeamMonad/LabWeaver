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
                  <span class="release-state">已发布</span>
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
  </section>
</template>

<script setup lang="ts">
import { computed, watch } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import { useEnvironmentTemplateReleases } from '@/composables/useEnvironmentTemplateReleases'
import { useProjects } from '@/composables/useProjects'
import { formatTimestamp } from '@/utils/format'

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

const publishedReleases = computed(() => releases.releases.kind === 'success'
  ? releases.releases.data.filter((release) => !release.withdrawal)
  : [])

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
  .release-card .text-button {
    width: 100%;
  }
}
</style>
