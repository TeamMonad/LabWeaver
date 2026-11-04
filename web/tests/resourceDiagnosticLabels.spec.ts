import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { diagnosticFromError, makeDiagnostic } from '@/types/async'

const labels = [
  ['LW_RESOURCE_CONTRACT_INVALID', '提交的资源、费率或计量数据不符合平台约定（例如模型 token 费率不是 CREDITS），本次操作没有执行。'],
  ['LW_RESOURCE_NUMERIC_OVERFLOW', '数量、时长或金额超出平台可精确计算的范围，无法估价、预占或扣收，不会按免费处理。'],
  ['LW_RESOURCE_APPROVAL_INVALID', '资源批准的有效期或时长无效，不能据此预占 Credits、启动或续期资源。'],
] as const

describe('Resource diagnostics preserved end to end (#334)', () => {
  it.each(labels)('gives %s a Chinese reason and keeps the stable code', (code, message) => {
    expect(makeDiagnostic(code, code, false)).toEqual({ code, message, retryable: false })
  })

  it.each(labels)('shows the %s reason before the code, with the request ID kept', (code, message) => {
    const diagnostic = diagnosticFromError(
      { diagnosticCode: code, detail: code, status: 409, retryable: false, requestId: `req-${code}` },
      'FALLBACK',
      'fallback',
    )
    expect(diagnostic.code).toBe(code)
    expect(diagnostic.message).toBe(message)
    const banner = mount(DiagnosticBanner, { props: { diagnostic } })
    expect(banner.get('.diagnostic-message').text()).toBe(message)
    expect(banner.get('.diagnostic-code').text()).toBe(code)
    expect(banner.get('.diagnostic-request-id').text()).toBe(`req-${code}`)
    expect(banner.text().indexOf(message)).toBeLessThan(banner.text().indexOf(code))
  })
})
