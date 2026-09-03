<template>
  <div ref="containerRef" class="gcp-search-bar" role="search">
    <div class="search-input-box" :class="{ 'search-input-box--focused': isFocused }">
      <SvgIcon name="search" size="sm" class="search-icon" aria-hidden="true" />
      <input
        ref="inputRef"
        v-model="query"
        type="text"
        class="search-input"
        placeholder="搜索资源、产品或文档 (按 / 聚焦)…"
        aria-label="全局资源与产品搜索"
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
      v-if="isFocused && (filteredItems.length > 0 || isDirectEnvId)"
      class="search-dropdown"
      role="listbox"
    >
      <div v-if="isDirectEnvId" class="dropdown-section">
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

      <div class="dropdown-section">
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
            <span class="item-desc">{{ item.category }} · {{ item.path }}</span>
          </div>
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, onMounted, onScopeDispose } from 'vue'
import { useRouter } from 'vue-router'
import SvgIcon from '@/components/common/SvgIcon.vue'

interface SearchItem {
  title: string
  category: string
  path: string
  icon: string
  keywords: string[]
}

let router: ReturnType<typeof useRouter> | null = null
try {
  router = useRouter()
} catch {
  // router not provided in isolated unit test stubs
}

const query = ref('')
const isFocused = ref(false)
const selectedIndex = ref(0)
const inputRef = ref<HTMLInputElement | null>(null)
const containerRef = ref<HTMLElement | null>(null)

const items: SearchItem[] = [
  {
    title: '我的实验 (Labs)',
    category: '计算与环境',
    path: '/student/labs',
    icon: 'science',
    keywords: ['实验', 'labs', '环境列表', 'student'],
  },
  {
    title: '环境控制台 (Console)',
    category: '计算与环境',
    path: '/student/environments',
    icon: 'desktop_windows',
    keywords: ['控制台', 'ssh', 'terminal', '环境', 'console'],
  },
  {
    title: 'SSH 公钥管理',
    category: '计算与环境',
    path: '/student/ssh-keys',
    icon: 'key',
    keywords: ['ssh', '公钥', 'keys'],
  },
  {
    title: '评测结果 (Results)',
    category: '评测与成果',
    path: '/student/results',
    icon: 'fact_check',
    keywords: ['成绩', '评测', '提交', 'results'],
  },
  {
    title: '材料上传与 AgentRun',
    category: '智能体与构建',
    path: '/teacher/materials',
    icon: 'smart_toy',
    keywords: ['材料', 'agent', 'run', '构建', 'upload'],
  },
  {
    title: '候选审批与发布',
    category: '智能体与构建',
    path: '/teacher/approvals',
    icon: 'rule',
    keywords: ['审批', '候选', 'candidate', 'approval'],
  },
  {
    title: '资源审批与 Lease',
    category: '治理与配额',
    path: '/admin/approvals',
    icon: 'admin_panel_settings',
    keywords: ['资源', '审批', 'lease', 'admin'],
  },
]

const isDirectEnvId = computed(() => {
  const q = query.value.trim()
  return q.startsWith('env-') || (q.length >= 8 && /^[0-9a-fA-F-]+$/.test(q))
})

const filteredItems = computed(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return items.slice(0, 5)
  return items.filter(
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
  void router?.push(item.path)
  closeDropdown()
  query.value = ''
}

function selectCurrent() {
  if (isDirectEnvId.value) {
    goToEnvironment(query.value.trim())
    return
  }
  if (filteredItems.value.length > 0) {
    const item = filteredItems.value[selectedIndex.value] || filteredItems.value[0]
    selectItem(item)
  }
}

function goToEnvironment(envId: string) {
  void router?.push(`/student/environments?environmentId=${envId}`)
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
