import { describe, expect, it } from 'vitest'
import { diagnosticFromError, makeDiagnostic } from '@/types/async'

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

  it('explains an unknown restart outcome and never offers to repeat it, even when marked retryable', () => {
    const diagnostic = makeDiagnostic('LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN', 'LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN', true)

    expect(diagnostic).toEqual({
      code: 'LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN',
      message: '无法确认上次重启是否完成，请检查环境状态后手动选择恢复。',
      nextStep: '平台不会自动重试。请在“异步操作与诊断”中核对环境当前状态；确认需要恢复时，使用顶部操作栏的“重试失败的操作”手动恢复。',
      retryable: false,
    })
  })

  it('keeps the unknown restart reason and manual next step over the 503 status fallback', () => {
    const diagnostic = diagnosticFromError(
      { diagnosticCode: 'LW_ENVIRONMENT_RESTART_OUTCOME_UNKNOWN', detail: 'restart outcome unknown', status: 503, retryable: false, requestId: 'req-restart' },
      'FALLBACK',
      'fallback',
    )

    expect(diagnostic.message).toBe('无法确认上次重启是否完成，请检查环境状态后手动选择恢复。')
    expect(diagnostic.nextStep).toContain('平台不会自动重试')
    expect(diagnostic.retryable).toBe(false)
    expect(diagnostic.requestId).toBe('req-restart')
  })

  it.each([
    ['LW_STORAGE_HEALTH_UNAVAILABLE', '平台暂时读不到存储健康状态，为保护数据已暂停新的资源分配和环境运行，本次操作没有执行。'],
    ['LW_STORAGE_HEALTH_STALE', '平台的存储健康状态已过期，无法确认存储当前是否安全，已暂停新的资源分配和环境运行，本次操作没有执行。'],
    ['LW_STORAGE_UNSAFE', '平台存储出现容量、仲裁或副本异常，为保护数据已暂停新的资源分配和环境运行，本次操作没有执行。'],
  ])('explains the fail-closed storage refusal %s and keeps it retryable', (code, message) => {
    const diagnostic = diagnosticFromError({ diagnosticCode: code, detail: code, status: 503, retryable: true }, 'FALLBACK', 'fallback')

    expect(diagnostic.message).toBe(message)
    expect(diagnostic.nextStep).toBe('这是平台存储问题，请稍后重试；多次失败请把下方诊断信息发给管理员。')
    expect(diagnostic.retryable).toBe(true)
  })
})
