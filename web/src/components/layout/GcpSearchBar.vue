<template>
  <div ref="containerRef" class="gcp-search-bar" role="search">
    <div class="search-input-box" :class="{ 'search-input-box--focused': isFocused }">
      <SvgIcon name="search" size="sm" class="search-icon" aria-hidden="true" />
      <input
        ref="inputRef"
        v-model="query"
        type="text"
        class="search-input"
        placeholder="搜索任务或输入环境 ID（按 / 聚焦）…"
        aria-label="搜索任务或按环境 ID 直达"
        @focus="onFocus"
        @keydown.down.prevent="navigateDown"
        @keydown.up.prevent="navigateUp"
        @keydown.enter.prevent="selectCurrent"
        @keydown.esc="closeDropdown"
      />
      <span v-if="!isFocused && !query" class="search-shortcut" aria-hidden="true">/</span>
      <button
        v-if="query"
        type="button"
        class="clear-btn"
        aria-label="清空搜索"
        @click="clearQuery"
      >
        <SvgIcon name="close" size="sm" aria-hidden="true" />
      </button>
    </div>

    <!-- Quick Navigation Dropdown -->
    <div
      v-if="isFocused && (filteredItems.length > 0 || directEnvironmentTarget || query.trim())"
      class="search-dropdown"
      role="listbox"
    >
      <div v-if="directEnvironmentTarget" class="dropdown-section">
        <div class="section-title">直达环境</div>
        <button
          type="button"
          class="dropdown-item dropdown-item--highlight"
          @click="goToEnvironment(query.trim())"
        >
          <SvgIcon name="desktop_windows" size="sm" class="item-icon" aria-hidden="true" />
          <span class="item-text">进入环境控制台：<code>{{ query.trim() }}</code></span>
        </button>
      </div>

      <div v-if="filteredItems.length > 0" class="dropdown-section">
        <div class="section-title">快捷导航</div>
        <button
          v-for="(item, idx) in filteredItems"
          :key="item.path"
          type="button"
          class="dropdown-item"
          :class="{ 'dropdown-item--active': idx === selectedIndex }"
          @click="selectItem(item)"
        >
          <SvgIcon :name="item.icon" size="sm" class="item-icon" aria-hidden="true" />
          <div class="item-content">
            <span class="item-title">{{ item.title }}</span>
            <span class="item-desc">{{ item.category }} · {{ item.description }}</span>
          </div>
        </button>
      </div>
      <p
        v-if="filteredItems.length === 0 && !directEnvironmentTarget && query.trim()"
        class="search-empty"
        role="status"
      >
        没有匹配的任务。
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, onMounted, onScopeDispose } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useAuth } from '@/composables/useAuth'
import { useProjects } from '@/composables/useProjects'
import {
  consoleNavigationTarget,
  navigationItemsForRoles,
  navigationTarget,
  rolesFromProfile,
} from '@/utils/navigation'

interface SearchItem {
  title: string
  category: string
  description: string
  path: string
  icon: string
  keywords: string[]
}

const router = useRouter()
const route = useRoute()

const query = ref('')
const isFocused = ref(false)
const selectedIndex = ref(0)
const inputRef = ref<HTMLInputElement | null>(null)
const containerRef = ref<HTMLElement | null>(null)

const auth = useAuth()
const projects = useProjects()
const roles = computed(() => auth.isAuthenticated.value ? rolesFromProfile(auth.user.value?.profile) : [])
const currentPath = computed(() => route.path)
const items = computed<SearchItem[]>(() => navigationItemsForRoles(roles.value).map((item) => ({
  title: item.label,
  category: item.groupLabel,
  description: item.description,
  path: navigationTarget(item, projects.selectedProjectId),
  icon: item.icon,
  keywords: item.keywords,
})))

const directEnvironmentTarget = computed(() => {
  const q = query.value.trim()
  const looksLikeEnvironmentId = q.startsWith('env-') || (q.length >= 8 && /^[0-9a-fA-F-]+$/.test(q))
  if (!looksLikeEnvironmentId) return null
  return consoleNavigationTarget(roles.value, currentPath.value, projects.selectedProjectId, q)
})

const filteredItems = computed(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return items.value.slice(0, 5)
  return items.value.filter(
    (item) =>
      item.title.toLowerCase().includes(q) ||
      item.category.toLowerCase().includes(q) ||
      item.keywords.some((k) => k.toLowerCase().includes(q)),
  )
})

function onFocus() {
  isFocused.value = true
  selectedIndex.value = 0
}

function clearQuery() {
  query.value = ''
  selectedIndex.value = 0
  inputRef.value?.focus()
}

function closeDropdown() {
  isFocused.value = false
}

function navigateDown() {
  if (filteredItems.value.length === 0) return
  selectedIndex.value = (selectedIndex.value + 1) % filteredItems.value.length
}

function navigateUp() {
  if (filteredItems.value.length === 0) return
  selectedIndex.value =
    (selectedIndex.value - 1 + filteredItems.value.length) % filteredItems.value.length
}

function selectItem(item: SearchItem) {
  void router.push(item.path)
  closeDropdown()
  query.value = ''
}

function selectCurrent() {
  if (directEnvironmentTarget.value) {
    goToEnvironment(query.value.trim())
    return
  }
  if (filteredItems.value.length > 0) {
    const item = filteredItems.value[selectedIndex.value] || filteredItems.value[0]
    selectItem(item)
  }
}

function goToEnvironment(envId: string) {
  const target = consoleNavigationTarget(roles.value, currentPath.value, projects.selectedProjectId, envId)
  if (!target) return
  void router.push(target)
  closeDropdown()
  query.value = ''
}

function handleGlobalKeydown(e: KeyboardEvent) {
  // Pressing "/" focuses the search bar if not typing in an input/textarea
  if (e.key === '/' && !isFocused.value) {
    const target = e.target as HTMLElement
    const isEditing =
      target.tagName === 'INPUT' ||
      target.tagName === 'TEXTAREA' ||
      target.isContentEditable
    if (!isEditing) {
      e.preventDefault()
      inputRef.value?.focus()
    }
  }
}

function handleClickOutside(e: MouseEvent) {
  if (containerRef.value && !containerRef.value.contains(e.target as Node)) {
    isFocused.value = false
  }
}

onMounted(() => {
  document.addEventListener('keydown', handleGlobalKeydown)
  document.addEventListener('click', handleClickOutside)
})

onScopeDispose(() => {
  document.removeEventListener('keydown', handleGlobalKeydown)
  document.removeEventListener('click', handleClickOutside)
})
</script>

<style scoped>
.gcp-search-bar {
  position: relative;
  flex: 1;
  max-width: 480px;
  min-width: 180px;
}

.search-input-box {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 36px;
  padding: 0 12px;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface-container-high);
  border: 1px solid transparent;
  transition: all 0.2s ease;
}

.search-input-box:hover {
  background: var(--md-sys-color-surface-container-highest);
}

.search-input-box--focused {
  background: var(--md-sys-color-surface);
  border-color: var(--md-sys-color-primary);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.15);
}

.search-icon {
  color: var(--md-sys-color-on-surface-variant);
  flex-shrink: 0;
}

.search-input {
  flex: 1;
  min-width: 0;
  border: none;
  background: transparent;
  outline: none;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
}

.search-shortcut {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: 4px;
  background: var(--md-sys-color-surface-container);
  border: 1px solid var(--md-sys-color-outline-variant);
  font: var(--md-sys-label-small);
  color: var(--md-sys-color-on-surface-variant);
}

.clear-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  padding: 0;
  border: none;
  border-radius: 50%;
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

/* Dropdown */
.search-dropdown {
  position: absolute;
  top: calc(100% + 4px);
  left: 0;
  right: 0;
  z-index: 1300;
  background: var(--md-sys-color-surface);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  box-shadow: var(--md-sys-elevation-3);
  max-height: 380px;
  overflow-y: auto;
  padding: 6px 0;
}

.dropdown-section {
  padding: 4px 0;
}

.dropdown-section + .dropdown-section {
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.section-title {
  padding: 4px 14px;
  font: var(--md-sys-label-small);
  font-size: 11px;
  font-weight: 600;
  color: var(--md-sys-color-on-surface-variant);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.search-empty {
  padding: 14px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
  text-align: center;
}

.dropdown-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 8px 14px;
  border: none;
  background: transparent;
  text-align: left;
  cursor: pointer;
  transition: background 0.15s ease;
}

.dropdown-item:hover,
.dropdown-item--active {
  background: var(--md-sys-color-surface-container-high);
}

.dropdown-item--highlight {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.item-icon {
  color: var(--md-sys-color-primary);
  flex-shrink: 0;
}

.item-content {
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.item-title {
  font: var(--md-sys-body-medium);
  font-weight: 500;
  color: var(--md-sys-color-on-surface);
}

.item-desc {
  font: var(--md-sys-body-small);
  font-size: 11px;
  color: var(--md-sys-color-on-surface-variant);
}

@media (max-width: 600px) {
  .gcp-search-bar {
    display: none;
  }
}
</style>
