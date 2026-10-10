import { describe, expect, it } from 'vitest'
import { makeDiagnostic } from '@/types/async'

describe('diagnostic messages', () => {
  it('explains that an expired material must be uploaded and published again', () => {
    const diagnostic = makeDiagnostic('LW_MATERIAL_RETENTION_EXPIRED', 'LW_MATERIAL_RETENTION_EXPIRED', true)

    expect(diagnostic).toEqual({
      code: 'LW_MATERIAL_RETENTION_EXPIRED',
      message: '材料保留期限已过期。请重新上传材料，并重新生成、审批和发布；重试当前过期版本无法恢复。',
      retryable: true,
    })
  })

  it('explains the recovery choices for exhausted GPU capacity', () => {
    const diagnostic = makeDiagnostic('LW_RESOURCE_GPU_CAPACITY_EXHAUSTED', 'LW_RESOURCE_GPU_CAPACITY_EXHAUSTED', false)

    expect(diagnostic).toEqual({
      code: 'LW_RESOURCE_GPU_CAPACITY_EXHAUSTED',
      message: 'GPU 容量当前不可用，可能已被其他任务占用。请稍后重试或调整资源申请。',
      retryable: false,
    })
  })

  it('keeps details for unrelated diagnostic codes unchanged', () => {
    expect(makeDiagnostic('LW_OTHER_FAILURE', '后端给出的可读说明', true)).toEqual({
      code: 'LW_OTHER_FAILURE',
      message: '后端给出的可读说明',
      retryable: true,
    })
  })
})
