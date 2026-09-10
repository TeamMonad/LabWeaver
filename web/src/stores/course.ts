import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { listProjects } from '@/generated/contracts'
import type { ProjectSchema } from '@/generated/contracts'

/**
 * Project context store.
 *
 * Project is the durable scope for Work and Evaluation.  A project may carry
 * an optional course association, so the browser keeps the server projection
 * and never invents a course when the API is unavailable.
 */

export interface CourseContext {
  projectId: string
  projectName: string
  courseId?: string | null
}

export const useCourseStore = defineStore('course', () => {
  const currentContext = ref<CourseContext | null>(null)
  const isLoading = ref(false)
  const error = ref<Error | null>(null)

  const isBound = computed(() => currentContext.value !== null)

  /** Load project contexts visible to the authenticated actor. */
  async function loadContext(_userId?: string): Promise<void> {
    isLoading.value = true
    error.value = null
    try {
      const result = await listProjects({})
      if (result.error) throw result.error
      availableCourses.value = result.data.map(toContext)
      const saved = typeof localStorage !== 'undefined' ? localStorage.getItem('labweaver_project_id') : null
      const selected = availableCourses.value.find((context) => context.projectId === currentContext.value?.projectId)
        ?? availableCourses.value.find((context) => context.projectId === saved)
        ?? availableCourses.value[0]
      currentContext.value = selected ?? null
    } catch (err) {
      error.value = err instanceof Error ? err : new Error(String(err))
      currentContext.value = null
    } finally {
      isLoading.value = false
    }
  }

  const availableCourses = ref<CourseContext[]>([])

  function setContext(context: CourseContext | string | null): void {
    if (typeof context === 'string') {
      currentContext.value = availableCourses.value.find((item) => item.projectId === context) ?? null
    } else {
      currentContext.value = context
    }
    error.value = null
    if (currentContext.value && typeof localStorage !== 'undefined') {
      try {
      localStorage.setItem('labweaver_project_id', currentContext.value.projectId)
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
      localStorage.removeItem('labweaver_project_id')
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

function toContext(project: ProjectSchema): CourseContext {
  return {
    projectId: project.id,
    projectName: project.name,
    courseId: project.courseId,
  }
}
