<template>
  <div ref="containerRef" class="gcp-project-selector">
    <button
      type="button"
      class="selector-trigger"
      :class="{ 'selector-trigger--active': isOpen }"
      aria-haspopup="dialog"
      :aria-expanded="isOpen"
      aria-label="选择项目"
      @click="toggleOpen"
    >
      <SvgIcon name="folder_open" size="sm" class="trigger-icon" aria-hidden="true" />
      <span class="trigger-label">
        <span class="trigger-primary">{{ activeProject?.name ?? '选择项目' }}</span>
      </span>
      <SvgIcon name="expand_more" size="sm" class="trigger-arrow" aria-hidden="true" />
    </button>

    <div v-if="isOpen" class="selector-menu" role="dialog" aria-label="项目选择器">
      <div class="menu-header">
        <span class="menu-title">选择项目</span>
        <button type="button" class="icon-button close-btn" aria-label="关闭选择器" @click="isOpen = false"><SvgIcon name="close" size="sm" aria-hidden="true" /></button>
      </div>
      <div class="menu-search">
        <SvgIcon name="search" size="sm" class="search-icon" aria-hidden="true" />
        <input ref="searchInputRef" v-model="searchQuery" type="search" class="search-input" placeholder="搜索项目名称…" aria-label="搜索项目" />
      </div>
      <AsyncStateView :state="projects.projects" empty-text="没有可访问的项目。请先创建项目或联系项目 Owner。" @retry="projects.load">
        <template #success="{ data }">
          <div class="menu-body" role="listbox" aria-label="项目列表">
            <button v-for="project in filteredProjects(data)" :key="project.id" type="button" class="project-item" :class="{ 'project-item--selected': project.id === projects.selectedProjectId }" role="option" :aria-selected="project.id === projects.selectedProjectId" @click="selectProject(project.id)">
              <span class="project-info"><strong>{{ project.name }}</strong></span>
              <span class="project-state">{{ project.state === 'active' ? '可用' : '已归档' }}</span>
            </button>
            <p v-if="filteredProjects(data).length === 0" class="empty-results">未找到匹配项目</p>
          </div>
        </template>
      </AsyncStateView>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, inject, nextTick, onMounted, onScopeDispose, ref } from 'vue'
import { routeLocationKey, routerKey, type LocationQueryRaw } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useProjects } from '@/composables/useProjects'
import type { ProjectSchema } from '@/generated/contracts'
import { NAVIGATION_GROUPS } from '@/utils/navigation'

const projects = useProjects()
const route = inject(routeLocationKey, null)
const router = inject(routerKey, null)
const isOpen = ref(false)
const searchQuery = ref('')
const containerRef = ref<HTMLElement | null>(null)
const searchInputRef = ref<HTMLInputElement | null>(null)
const activeProject = computed(() => projects.selectedProject)
const projectScopedPaths = NAVIGATION_GROUPS.flatMap((group) => group.items)
  .filter((item) => item.projectScoped)
  .map((item) => item.path)

function filteredProjects(items: ProjectSchema[]) {
  const query = searchQuery.value.trim().toLowerCase()
  if (!query) return items
  return items.filter((project) => project.name.toLowerCase().includes(query) || project.id.toLowerCase().includes(query))
}
function toggleOpen() {
  isOpen.value = !isOpen.value
  if (isOpen.value) void nextTick(() => searchInputRef.value?.focus())
}

function projectScopedBasePath(path: string) {
  return projectScopedPaths.find((itemPath) => path === itemPath || path.startsWith(`${itemPath}/`)) ?? null
}

const PROJECT_CONTEXT_QUERY_KEYS = ['environmentId', 'releaseId', 'runId', 'packageId', 'approvalId'] as const

function selectProject(id: string) {
  const routeProjectId = typeof route?.query.projectId === 'string' ? route.query.projectId : undefined
  const currentProjectId = routeProjectId ?? projects.selectedProjectId
  const routeProjectChanged = routeProjectId !== id
  const projectChanged = currentProjectId !== id
  projects.select(id)
  isOpen.value = false
  searchQuery.value = ''
  const basePath = route ? projectScopedBasePath(route.path) : null
  if (router && route && (basePath || route.query.projectId !== undefined) && (projectChanged || routeProjectChanged)) {
    const query: LocationQueryRaw = { ...route.query, projectId: id }
    if (projectChanged) {
      for (const key of PROJECT_CONTEXT_QUERY_KEYS) query[key] = undefined
    }
    void router.replace({
      ...(projectChanged && basePath && basePath !== route.path ? { path: basePath } : {}),
      query,
    })
  }
}
function handleClickOutside(event: MouseEvent) {
  if (containerRef.value && !containerRef.value.contains(event.target as Node)) isOpen.value = false
}
function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') isOpen.value = false
}
onMounted(() => {
  document.addEventListener('click', handleClickOutside)
  document.addEventListener('keydown', handleKeydown)
})
onScopeDispose(() => {
  document.removeEventListener('click', handleClickOutside)
  document.removeEventListener('keydown', handleKeydown)
})
</script>

<style scoped>
.gcp-project-selector { position: relative; display: inline-flex; align-items: center; }
.selector-trigger { display: inline-flex; align-items: center; gap: 8px; height: 36px; max-width: 280px; padding: 0 10px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container); color: var(--md-sys-color-on-surface); cursor: pointer; }
.selector-trigger:hover, .selector-trigger--active { border-color: var(--md-sys-color-primary); background: var(--md-sys-color-surface-container-high); }
.trigger-icon { flex-shrink: 0; color: var(--md-sys-color-primary); }
.trigger-label { display: flex; min-width: 0; flex-direction: column; align-items: flex-start; text-align: left; line-height: 1.2; }
.trigger-primary { max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font: var(--md-sys-label-medium); }
.trigger-arrow { flex-shrink: 0; color: var(--md-sys-color-on-surface-variant); }
.selector-menu { position: absolute; top: calc(100% + 6px); left: 0; z-index: 1300; display: flex; width: 360px; max-width: calc(100vw - 24px); flex-direction: column; overflow: hidden; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); background: var(--md-sys-color-surface); box-shadow: var(--md-sys-elevation-3); }
.menu-header { display: flex; align-items: center; justify-content: space-between; padding: 12px 14px 8px; border-bottom: 1px solid var(--md-sys-color-outline-variant); }
.menu-title { color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); font-weight: 600; }
.icon-button { display: inline-grid; place-items: center; width: 32px; height: 32px; border: 0; border-radius: 50%; background: transparent; color: var(--md-sys-color-on-surface-variant); cursor: pointer; }
.menu-search { display: flex; align-items: center; gap: 8px; padding: 8px 12px; border-bottom: 1px solid var(--md-sys-color-outline-variant); background: var(--md-sys-color-surface-container); }
.search-icon { color: var(--md-sys-color-on-surface-variant); }
.search-input { flex: 1; border: 0; outline: 0; background: transparent; color: var(--md-sys-color-on-surface); font: var(--md-sys-body-medium); }
.menu-body { max-height: 320px; overflow-y: auto; padding: 6px 0; }
.project-item { display: flex; width: 100%; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 14px; border: 0; background: transparent; color: var(--md-sys-color-on-surface); text-align: left; cursor: pointer; }
.project-item:hover, .project-item--selected { background: var(--md-sys-color-primary-container); }
.project-info { display: flex; min-width: 0; flex-direction: column; gap: 3px; }
.project-info strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font: var(--md-sys-body-medium); }
.project-state, .empty-results { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.project-state { flex-shrink: 0; }
.empty-results { padding: 22px; text-align: center; }
</style>
