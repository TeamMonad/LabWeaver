<template>
  <div ref="containerRef" class="gcp-project-selector">
    <button
      type="button"
      class="selector-trigger"
      :class="{ 'selector-trigger--active': isOpen }"
      aria-haspopup="dialog"
      :aria-expanded="isOpen"
      aria-label="选择课程或项目上下文"
      @click="toggleOpen"
    >
      <SvgIcon name="folder_open" size="sm" class="trigger-icon" aria-hidden="true" />
      <span class="trigger-label">
        <span class="trigger-primary">{{ activeCourseName }}</span>
        <span v-if="activeCourseId" class="trigger-secondary">{{ activeCourseId }}</span>
      </span>
      <SvgIcon name="expand_more" size="sm" class="trigger-arrow" aria-hidden="true" />
    </button>

    <!-- Dropdown / Dialog -->
    <div
      v-if="isOpen"
      class="selector-menu"
      role="dialog"
      aria-label="课程与项目选择器"
    >
      <div class="menu-header">
        <span class="menu-title">选择课程或项目</span>
        <button
          type="button"
          class="icon-button close-btn"
          aria-label="关闭选择器"
          @click="isOpen = false"
        >
          <SvgIcon name="close" size="sm" aria-hidden="true" />
        </button>
      </div>

      <!-- Search / filter input -->
      <div class="menu-search">
        <SvgIcon name="search" size="sm" class="search-icon" aria-hidden="true" />
        <input
          ref="searchInputRef"
          v-model="searchQuery"
          type="text"
          class="search-input"
          placeholder="搜索课程名称或 ID…"
          aria-label="搜索课程"
          @keydown.enter="selectCustomIfNoMatch"
        />
      </div>

      <!-- Course list -->
      <div class="menu-body" role="listbox">
        <div v-if="filteredCourses.length > 0" class="course-list">
          <button
            v-for="course in filteredCourses"
            :key="course.courseId"
            type="button"
            class="course-item"
            :class="{ 'course-item--selected': course.courseId === activeCourseId }"
            role="option"
            :aria-selected="course.courseId === activeCourseId"
            @click="selectCourse(course)"
          >
            <div class="course-info">
              <span class="course-name">{{ course.courseName }}</span>
              <span class="course-id">{{ course.courseId }}</span>
            </div>
            <span class="course-role-badge" :class="`badge--${course.role}`">
              {{ courseRoleLabel(course.role) }}
            </span>
          </button>
        </div>

        <div v-else class="empty-results">
          <p>未找到匹配课程</p>
          <button
            v-if="searchQuery.trim()"
            type="button"
            class="custom-submit-btn"
            @click="selectCustom(searchQuery.trim())"
          >
            将「{{ searchQuery.trim() }}」设为自定义课程 ID
          </button>
        </div>
      </div>

      <!-- Footer -->
      <div class="menu-footer">
        <div class="custom-id-input-wrap">
          <input
            v-model="customIdInput"
            type="text"
            class="custom-input"
            placeholder="或直接输入自定义课程 ID…"
            aria-label="直接输入自定义课程 ID"
            @keydown.enter="applyCustomId"
          />
          <button
            type="button"
            class="apply-btn"
            :disabled="!customIdInput.trim()"
            @click="applyCustomId"
          >
            切换
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, nextTick, onMounted, onScopeDispose } from 'vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import { useCourseStore, type CourseContext } from '@/stores/course'
import { useCourseContext } from '@/composables/useCourseContext'

const courseStore = useCourseStore()
const courseCtx = useCourseContext()

const isOpen = ref(false)
const searchQuery = ref('')
const customIdInput = ref('')
const containerRef = ref<HTMLElement | null>(null)
const searchInputRef = ref<HTMLInputElement | null>(null)

const activeCourseId = computed(() => courseCtx.courseId.value)

const activeCourseName = computed(() => {
  if (courseStore.currentContext?.courseName) {
    return courseStore.currentContext.courseName
  }
  const id = activeCourseId.value
  if (!id) return '选择课程 / 项目'
  const found = courseStore.availableCourses.find((c) => c.courseId === id)
  return found?.courseName ?? `课程：${id}`
})

const filteredCourses = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return courseStore.availableCourses
  return courseStore.availableCourses.filter(
    (c) => c.courseName.toLowerCase().includes(q) || c.courseId.toLowerCase().includes(q),
  )
})

function courseRoleLabel(role: string): string {
  switch (role) {
    case 'teacher':
      return '教师'
    case 'admin':
      return '管理'
    case 'researcher':
      return '科研'
    default:
      return '学生'
  }
}

function toggleOpen() {
  isOpen.value = !isOpen.value
  if (isOpen.value) {
    void nextTick(() => {
      searchInputRef.value?.focus()
    })
  }
}

function selectCourse(course: CourseContext) {
  courseStore.setContext(course)
  isOpen.value = false
  searchQuery.value = ''
}

function selectCustom(id: string) {
  courseStore.setContext({
    courseId: id,
    courseName: `自定义课程 (${id})`,
    role: 'student',
  })
  isOpen.value = false
  searchQuery.value = ''
}

function selectCustomIfNoMatch() {
  if (filteredCourses.value.length === 1) {
    selectCourse(filteredCourses.value[0])
  } else if (filteredCourses.value.length === 0 && searchQuery.value.trim()) {
    selectCustom(searchQuery.value.trim())
  }
}

function applyCustomId() {
  const id = customIdInput.value.trim()
  if (!id) return
  selectCustom(id)
  customIdInput.value = ''
}

function handleClickOutside(e: MouseEvent) {
  if (containerRef.value && !containerRef.value.contains(e.target as Node)) {
    isOpen.value = false
  }
}

function handleKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape' && isOpen.value) {
    isOpen.value = false
  }
}

onMounted(() => {
  // If no current context in store, try to restore from localStorage or fallback
  if (!courseStore.currentContext) {
    try {
      const saved = localStorage.getItem('labweaver_course_context')
      if (saved) {
        courseStore.setContext(JSON.parse(saved) as CourseContext)
      }
    } catch {
      // ignore
    }
  }

  document.addEventListener('click', handleClickOutside)
  document.addEventListener('keydown', handleKeydown)
})

onScopeDispose(() => {
  document.removeEventListener('click', handleClickOutside)
  document.removeEventListener('keydown', handleKeydown)
})
</script>

<style scoped>
.gcp-project-selector {
  position: relative;
  display: inline-flex;
  align-items: center;
}

.selector-trigger {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 36px;
  padding: 0 10px;
  border-radius: var(--md-sys-shape-small);
  border: 1px solid var(--md-sys-color-outline-variant);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface);
  cursor: pointer;
  max-width: 280px;
  transition: background-color 0.15s, border-color 0.15s;
}

.selector-trigger:hover,
.selector-trigger--active {
  background: var(--md-sys-color-surface-container-high);
  border-color: var(--md-sys-color-primary);
}

.trigger-icon {
  color: var(--md-sys-color-primary);
  flex-shrink: 0;
}

.trigger-label {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  text-align: left;
  overflow: hidden;
  line-height: 1.2;
}

.trigger-primary {
  font: var(--md-sys-label-medium);
  font-weight: 500;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 180px;
}

.trigger-secondary {
  font: var(--md-sys-label-small);
  font-size: 10px;
  color: var(--md-sys-color-on-surface-variant);
  font-family: monospace;
}

.trigger-arrow {
  color: var(--md-sys-color-on-surface-variant);
  flex-shrink: 0;
  transition: transform 0.2s ease;
}

.selector-trigger--active .trigger-arrow {
  transform: rotate(180deg);
}

/* Dropdown menu */
.selector-menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 1300;
  width: 360px;
  max-width: calc(100vw - 24px);
  background: var(--md-sys-color-surface);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  box-shadow: var(--md-sys-elevation-3);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.menu-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 14px 8px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.menu-title {
  font: var(--md-sys-title-small);
  font-weight: 600;
  color: var(--md-sys-color-on-surface);
}

.close-btn {
  width: 28px;
  height: 28px;
  border: none;
  border-radius: 50%;
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.menu-search {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  background: var(--md-sys-color-surface-container);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.search-icon {
  color: var(--md-sys-color-on-surface-variant);
}

.search-input {
  flex: 1;
  border: none;
  background: transparent;
  outline: none;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface);
}

.menu-body {
  max-height: 240px;
  overflow-y: auto;
  padding: 6px 0;
}

.course-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  padding: 8px 14px;
  border: none;
  background: transparent;
  text-align: left;
  cursor: pointer;
  transition: background 0.15s;
}

.course-item:hover {
  background: var(--md-sys-color-surface-container-high);
}

.course-item--selected {
  background: var(--md-sys-color-primary-container);
}

.course-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
  overflow: hidden;
}

.course-name {
  font: var(--md-sys-body-medium);
  font-weight: 500;
  color: var(--md-sys-color-on-surface);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.course-id {
  font: var(--md-sys-body-small);
  font-family: monospace;
  color: var(--md-sys-color-on-surface-variant);
}

.course-role-badge {
  font: var(--md-sys-label-small);
  font-size: 11px;
  padding: 2px 6px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-variant);
  color: var(--md-sys-color-on-surface-variant);
}

.badge--teacher {
  background: rgba(0, 106, 106, 0.15);
  color: var(--md-sys-color-primary);
}

.empty-results {
  padding: 20px;
  text-align: center;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.custom-submit-btn {
  margin-top: 8px;
  padding: 6px 12px;
  border: 1px solid var(--md-sys-color-primary);
  border-radius: var(--md-sys-shape-small);
  background: transparent;
  color: var(--md-sys-color-primary);
  font: var(--md-sys-label-small);
  cursor: pointer;
}

.custom-submit-btn:hover {
  background: var(--md-sys-color-primary-container);
}

.menu-footer {
  padding: 8px 12px;
  background: var(--md-sys-color-surface-container);
  border-top: 1px solid var(--md-sys-color-outline-variant);
}

.custom-id-input-wrap {
  display: flex;
  align-items: center;
  gap: 6px;
}

.custom-input {
  flex: 1;
  height: 28px;
  padding: 0 8px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-surface);
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface);
}

.apply-btn {
  height: 28px;
  padding: 0 10px;
  border: none;
  border-radius: var(--md-sys-shape-small);
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
  font: var(--md-sys-label-small);
  cursor: pointer;
}

.apply-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
