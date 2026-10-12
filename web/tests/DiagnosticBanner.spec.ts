import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import { diagnosticFromError, diagnosticNextStep, makeDiagnostic } from '@/types/async'

describe('actionable diagnostics', () => {
  it('keeps requestId and status from the problem body and adds a Chinese next step', () => {
    const diagnostic = diagnosticFromError(
      { status: 403, diagnosticCode: 'LW_ACCESS_FORBIDDEN', detail: '没有权限。', requestId: 'req-1', retryable: false },
      'FALLBACK',
      '失败',
    )
    expect(diagnostic).toEqual(expect.objectContaining({
      code: 'LW_ACCESS_FORBIDDEN',
      message: '没有权限。',
      requestId: 'req-1',
      status: 403,
      retryable: false,
    }))
    expect(diagnostic.nextStep).toContain('联系项目 Owner')
  })

  it('does not invent a next step without a server status', () => {
    expect(makeDiagnostic('LOCAL_INPUT', '名称不能为空。').nextStep).toBeUndefined()
    expect(diagnosticNextStep(undefined, true)).toBeUndefined()
    expect(diagnosticFromError({ isAxiosError: true }, 'NET', '请求失败').nextStep).toContain('检查网络')
  })

  it('shows reason first and keeps code and requestId copyable but secondary', () => {
    const wrapper = mount(DiagnosticBanner, {
      props: { diagnostic: makeDiagnostic('LW_X', '环境启动失败。', true, { status: 503, requestId: 'req-9' }) },
    })
    const content = wrapper.get('.diagnostic-content').text()
    expect(content).toContain('环境启动失败。')
    expect(content).toContain('平台服务暂时不可用')
    expect(content).not.toContain('LW_X')
    expect(wrapper.get('.diagnostic-details summary').text()).toBe('诊断信息')
    expect(wrapper.get('.diagnostic-code').text()).toBe('LW_X')
    expect(wrapper.get('.diagnostic-request-id').text()).toBe('req-9')
    expect(wrapper.find('.copy-button').exists()).toBe(true)
    // Retry stays the first action of the banner.
    expect(wrapper.get('button').text()).toBe('重试')
  })

  it('still accepts explicit code and message props', () => {
    const wrapper = mount(DiagnosticBanner, { props: { code: 'C', message: 'M' } })
    expect(wrapper.get('.diagnostic-message').text()).toBe('M')
    expect(wrapper.get('.diagnostic-code').text()).toBe('C')
    expect(wrapper.find('button.text-button').exists()).toBe(false)
  })
})
