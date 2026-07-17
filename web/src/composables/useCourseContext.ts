import { computed } from 'vue'
import { useCourseStore } from '@/stores/course'
import { useAuth } from '@/composables/useAuth'

export interface CourseContext {
  courseId: string
  source: 'store' | 'profile' | 'env'
}

/**
 * Resolve the current course context for course-scoped Public API calls.
 *
 * Resolution order:
 * 1. Bound course store context.
 * 2. OIDC profile `course_id` claim.
 * 3. Deployment-specific `VITE_DEFAULT_COURSE_ID` env variable.
 */
export function useCourseContext() {
  const courseStore = useCourseStore()
  const auth = useAuth()

  const context = computed<CourseContext | null>(() => {
    if (courseStore.currentContext?.courseId) {
      return { courseId: courseStore.currentContext.courseId, source: 'store' }
    }
    const profileCourseId = auth.user.value?.profile?.course_id
    if (typeof profileCourseId === 'string' && profileCourseId) {
      return { courseId: profileCourseId, source: 'profile' }
    }
    const envCourseId = import.meta.env.VITE_DEFAULT_COURSE_ID as string | undefined
    if (envCourseId) {
      return { courseId: envCourseId, source: 'env' }
    }
    return null
  })

  const courseId = computed(() => context.value?.courseId)
  const isFromEnv = computed(() => context.value?.source === 'env')

  return { context, courseId, isFromEnv }
}
