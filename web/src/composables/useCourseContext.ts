import { computed } from 'vue'
import { useCourseStore } from '@/stores/course'

export interface CourseContext {
  projectId: string
  courseId?: string
  source: 'store'
}

/**
 * Resolve the selected server-owned project context.  Course is an optional
 * association on a project and is never selected from profile claims or a
 * deployment default.
 */
export function useCourseContext() {
  const courseStore = useCourseStore()

  const context = computed<CourseContext | null>(() => {
    const selected = courseStore.currentContext
    if (!selected) return null
    return {
      projectId: selected.projectId,
      ...(selected.courseId ? { courseId: selected.courseId } : {}),
      source: 'store',
    }
  })

  const projectId = computed(() => context.value?.projectId)
  const courseId = computed(() => context.value?.courseId)
  const isFromEnv = computed(() => false)

  return { context, projectId, courseId, isFromEnv }
}
