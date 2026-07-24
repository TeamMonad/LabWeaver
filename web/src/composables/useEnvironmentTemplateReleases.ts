import { reactive, ref, watch, type Ref } from 'vue'
import { listEnvironmentTemplateReleases } from '@/generated/contracts'
import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { extractProblemDetails, makeDiagnostic, type AsyncState } from '@/types/async'

export function useEnvironmentTemplateReleases(courseId: Ref<string | undefined>) {
  const releases = ref<AsyncState<EnvironmentTemplateReleaseViewSchema[]>>({ kind: 'idle' })

  async function load() {
    const id = courseId.value
    if (!id) {
      releases.value = {
        kind: 'blocked',
        diagnostic: makeDiagnostic(
          'COURSE_CONTEXT_MISSING',
          '课程上下文未绑定，无法加载环境模板版本。请通过课程选择器选择课程或联系管理员完成 #47。',
          false,
        ),
      }
      return
    }

    releases.value = { kind: 'loading', message: '加载已发布版本…' }
    const result = await listEnvironmentTemplateReleases({ path: { courseId: id } })
    if (result.error || !result.data) {
      const problem = extractProblemDetails(result.error)
      releases.value = {
        kind: 'error',
        diagnostic: makeDiagnostic(
          problem?.diagnosticCode ?? (result.error ? 'RELEASE_LIST_FAILED' : 'RELEASE_LIST_RESPONSE_INVALID'),
          problem?.detail ?? (result.error ? '加载环境模板版本失败' : '环境模板版本响应缺少数据。'),
          problem?.retryable ?? Boolean(result.error),
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
