import { ref, computed } from 'vue'
import { defineStore } from 'pinia'

/**
 * Course context store.
 *
 * This store binds the current user's selected course/project context once the
 * backend Access/Control contract is available. It intentionally remains a
 * skeleton for Issue #55 because the A3 AUTH-02a contract that defines the
 * course membership API shape is scheduled for 2026-07-15 (#47).
 *
 * TODO(#55): replace placeholder types and API client calls with the real
 * contract once #47 is merged.
 */

export interface CourseContext {
  courseId: string
  courseName: string
  role: 'teacher' | 'student' | 'admin' | 'researcher'
  projectId?: string
}

export const useCourseStore = defineStore('course', () => {
  const currentContext = ref<CourseContext | null>(null)
  const isLoading = ref(false)
  const error = ref<Error | null>(null)

  const isBound = computed(() => currentContext.value !== null)

  /**
   * Load course context for the authenticated user.
   *
   * Currently returns null until the backend contract is frozen. Callers must
   * fail-closed (do not render course-scoped UI) when this returns null or
   * throws.
   */
  async function loadContext(_userId: string): Promise<void> {
    isLoading.value = true
    error.value = null
    try {
      // Placeholder: real implementation will call the Control Service course
      // membership endpoint defined by #47 AUTH-02a.
      currentContext.value = null
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
      currentContext.value = null
    } finally {
      isLoading.value = false
    }
  }

  const availableCourses = ref<CourseContext[]>([
    {
      courseId: 'course-cs101',
      courseName: '操作系统核心实验 (CS101)',
      role: 'student',
    },
    {
      courseId: 'course-ai201',
      courseName: '自主智能体与大模型工程 (AI201)',
      role: 'student',
    },
    {
      courseId: 'course-sys301',
      courseName: '云原生系统架构与容器实训 (SYS301)',
      role: 'teacher',
    },
  ])

  function setContext(context: CourseContext | string | null, role: CourseContext['role'] = 'student'): void {
    if (typeof context === 'string') {
      const found = availableCourses.value.find((c) => c.courseId === context)
      currentContext.value = found ?? {
        courseId: context,
        courseName: context,
        role,
      }
    } else {
      currentContext.value = context
    }
    error.value = null
    if (currentContext.value && typeof localStorage !== 'undefined') {
      try {
        localStorage.setItem('labweaver_course_context', JSON.stringify(currentContext.value))
      } catch {
        // ignore quota errors in private browsing
      }
    }
  }

  function clearContext(): void {
    currentContext.value = null
    error.value = null
    isLoading.value = false
    if (typeof localStorage !== 'undefined') {
      try {
        localStorage.removeItem('labweaver_course_context')
      } catch {
        // ignore
      }
    }
  }

  return {
    currentContext,
    availableCourses,
    isLoading,
    error,
    isBound,
    setContext,
    loadContext,
    clearContext,
  }
})
