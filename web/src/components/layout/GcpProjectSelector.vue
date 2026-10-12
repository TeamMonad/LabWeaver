<template>
  <div ref="containerRef" class="gcp-project-selector">
    <button
      ref="triggerRef"
      type="button"
      class="selector-trigger"
      :class="{ 'selector-trigger--active': isOpen, 'selector-trigger--unavailable': routeProjectUnavailable }"
      aria-haspopup="dialog"
      :aria-expanded="isOpen"
      :aria-label="routeProjectUnavailable ? '选择项目，链接中的项目不可用' : `选择项目，当前：${activeProject?.name ?? '未选择'}`"
      :title="activeProject?.name"
      @click="toggleOpen"
    >
      <SvgIcon :name="routeProjectUnavailable ? 'link_off' : 'folder_open'" size="sm" class="trigger-icon" aria-hidden="true" />
      <span class="trigger-label">
        <span class="trigger-primary">{{ triggerLabel }}</span>
      </span>
      <SvgIcon name="expand_more" size="sm" class="trigger-arrow" aria-hidden="true" />
    </button>

    <div v-if="isOpen" class="selector-menu" role="dialog" aria-label="项目选择器" @keydown.esc.stop.prevent="close(true)">
      <div class="menu-header">
        <span class="menu-title">选择项目</span>
        <button type="button" class="icon-button close-btn" aria-label="关闭选择器" @click="close(true)"><SvgIcon name="close" size="sm" aria-hidden="true" /></button>
      </div>
      <div class="menu-search">
        <SvgIcon name="search" size="sm" class="search-icon" aria-hidden="true" />
        <input
          ref="searchInputRef"
          v-model="searchQuery"
          type="search"
          class="search-input"
          placeholder="搜索项目名称…"
          aria-label="搜索项目"
          aria-controls="project-selector-list"
          :aria-activedescendant="activeOptionId"
          @keydown.down.prevent="moveActive(1)"
          @keydown.up.prevent="moveActive(-1)"
          @keydown.home.prevent="moveActive('first')"
          @keydown.end.prevent="moveActive('last')"
          @keydown.enter.prevent="selectActive"
        />
      </div>
      <AsyncStateView :state="projects.projects" empty-text="没有可访问的项目。请先创建项目或联系项目 Owner。" @retry="projects.load">
        <template #success="{ data }">
          <div id="project-selector-list" class="menu-body" role="listbox" aria-label="项目列表">
            <button
              v-for="(project, index) in filteredProjects(data)"
              :id="optionId(project.id)"
              :key="project.id"
              type="button"
              tabindex="-1"
              class="project-item"
              :class="{
                'project-item--selected': project.id === projects.selectedProjectId,
                'project-item--active': index === activeIndex,
              }"
              role="option"
              :aria-selected="project.id === projects.selectedProjectId"
              @click="selectProject(project.id)"
              @pointermove="activeIndex = index"
            >
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
import { computed, inject, nextTick, onMounted, onScopeDispose, ref, watch } from 'vue'
import { routeLocationKey, routerKey, type LocationQueryRaw } from 'vue-router'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { PROJECT_OWNED_QUERY_KEYS } from '@/composables/useProjectContext'
import { useProjects } from '@/composables/useProjects'
import type { ProjectSchema } from '@/generated/contracts'
import { projectScopedBasePath } from '@/utils/navigation'

const projects = useProjects()
// Injected instead of useRoute() so the selector also renders outside a router (tests, fixtures).
const route = inject(routeLocationKey, null)
const router = inject(routerKey, null)
const isOpen = ref(false)
const searchQuery = ref('')
const activeIndex = ref(-1)
const containerRef = ref<HTMLElement | null>(null)
const triggerRef = ref<HTMLButtonElement | null>(null)
const searchInputRef = ref<HTMLInputElement | null>(null)
const activeProject = computed(() => routeProjectUnavailable.value ? null : projects.selectedProject)

const routeProjectId = computed(() => {
  const value = route?.query.projectId
  return typeof value === 'string' && value.trim() ? value.trim() : null
})
/** A linked project that the account cannot list; the shell shows recovery instead of its data. */
const routeProjectUnavailable = computed(() => {
  const settled = projects.settledProjects
  if (!route?.meta.projectScoped || !routeProjectId.value) return false
  if (settled.kind === 'empty') return true
  return settled.kind === 'success' && !settled.data.some((project) => project.id === routeProjectId.value)
})
const triggerLabel = computed(() => {
  if (routeProjectUnavailable.value) return '项目不可用'
  return activeProject.value?.name ?? '选择项目'
})

function filteredProjects(items: ProjectSchema[]) {
  const query = searchQuery.value.trim().toLowerCase()
  if (!query) return items
  return items.filter((project) => project.name.toLowerCase().includes(query) || project.id.toLowerCase().includes(query))
}
const visibleProjects = computed(() => projects.projects.kind === 'success' ? filteredProjects(projects.projects.data) : [])

function optionId(projectId: string) {
  return `project-option-${projectId.replace(/[^a-zA-Z0-9_-]/g, '_')}`
}
const activeOptionId = computed(() => {
  const project = visibleProjects.value[activeIndex.value]
  return project ? optionId(project.id) : undefined
})

function resetActive() {
  const selectedIndex = visibleProjects.value.findIndex((project) => project.id === projects.selectedProjectId)
  activeIndex.value = selectedIndex >= 0 ? selectedIndex : (visibleProjects.value.length ? 0 : -1)
}

function moveActive(step: 1 | -1 | 'first' | 'last') {
  const count = visibleProjects.value.length
  if (!count) return
  if (step === 'first') activeIndex.value = 0
  else if (step === 'last') activeIndex.value = count - 1
  else activeIndex.value = ((activeIndex.value < 0 ? (step === 1 ? -1 : 0) : activeIndex.value) + step + count) % count
  void nextTick(() => {
    const id = activeOptionId.value
    if (id) containerRef.value?.querySelector(`#${id}`)?.scrollIntoView?.({ block: 'nearest' })
  })
}

function selectActive() {
  const project = visibleProjects.value[activeIndex.value]
  if (project) selectProject(project.id)
}

watch(searchQuery, () => {
  activeIndex.value = visibleProjects.value.length ? 0 : -1
})

// The catalog may still be loading when the menu opens; start on the current project once it arrives.
watch(() => visibleProjects.value.length, () => {
  if (isOpen.value && activeIndex.value < 0) resetActive()
})

function toggleOpen() {
  if (isOpen.value) {
    close(false)
    return
  }
  isOpen.value = true
  resetActive()
  void nextTick(() => searchInputRef.value?.focus())
}

function close(restoreFocus: boolean) {
  if (!isOpen.value) return
  isOpen.value = false
  searchQuery.value = ''
  if (restoreFocus) void nextTick(() => triggerRef.value?.focus())
}

function selectProject(id: string) {
  const currentProjectId = routeProjectId.value ?? projects.selectedProjectId
  const routeProjectChanged = routeProjectId.value !== id
  const projectChanged = currentProjectId !== id
  projects.select(id)
  close(true)
  const basePath = route ? projectScopedBasePath(route.path) : null
  const scoped = Boolean(route?.meta.projectScoped || basePath)
  if (router && route && (scoped || route.query.projectId !== undefined) && (projectChanged || routeProjectChanged)) {
    const query: LocationQueryRaw = { ...route.query, projectId: id }
    if (projectChanged) {
      for (const key of PROJECT_OWNED_QUERY_KEYS) query[key] = undefined
    }
    void router.replace({
      ...(projectChanged && basePath && basePath !== route.path ? { path: basePath } : {}),
      query,
    })
  }
}
function handleClickOutside(event: MouseEvent) {
  if (containerRef.value && !containerRef.value.contains(event.target as Node)) close(false)
}
function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && isOpen.value) close(true)
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
.project-item:hover, .project-item--active { background: var(--md-sys-color-surface-container-high); }
.project-item--selected { background: var(--md-sys-color-primary-container); }
.project-item--selected.project-item--active { box-shadow: inset 3px 0 0 var(--md-sys-color-primary); }
.selector-trigger--unavailable { border-color: var(--md-sys-color-warning); background: var(--md-sys-color-warning-container); }
.selector-trigger--unavailable .trigger-icon { color: var(--md-sys-color-warning); }
.project-info { display: flex; min-width: 0; flex-direction: column; gap: 3px; }
.project-info strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font: var(--md-sys-body-medium); }
.project-state, .empty-results { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.project-state { flex-shrink: 0; }
.empty-results { padding: 22px; text-align: center; }
</style>
