import { reactive, ref, watch, type Ref } from 'vue'
import type { AxiosError } from 'axios'
import { listEnvironmentTemplateReleases } from '@/generated/contracts'
import type { EnvironmentTemplateReleaseViewSchema, ProblemDetails } from '@/generated/contracts'
import { makeDiagnostic, type AsyncState } from '@/types/async'

export function useEnvironmentTemplateReleases(courseId: Ref<string | undefined>) {
  const releases = ref<AsyncState<EnvironmentTemplateReleaseViewSchema[]>>({ kind: 'idle' })

  async function load() {
    const id = courseId.value
    if (!id) {
      releases.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic(
          'COURSE_CONTEXT_MISSING',
          '课程上下文未绑定，无法加载环境模板版本。请联系架构 Owner 完成 #47。',
          false,
        ),
      }
      return
    }

    releases.value = { kind: 'loading', message: '加载已发布版本…' }
    const result = await listEnvironmentTemplateReleases({ path: { courseId: id } })
    if (result.error) {
      const problem = ((result.error as AxiosError).response?.data ?? (result.error as { error?: ProblemDetails }).error) as
        | ProblemDetails
        | undefined
      releases.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? 'RELEASE_LIST_FAILED',
          problem?.detail ?? '加载环境模板版本失败',
          problem?.retryable ?? true,
        ),
      }
      return
    }
    const items = result.data.items ?? []
    releases.value = items.length > 0 ? { kind: 'success', data: items } : { kind: 'empty' }
  }

  watch(courseId, load, { immediate: true })

  return reactive({ releases, load })
}
