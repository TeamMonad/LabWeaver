import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import CopyButton from '@/components/common/CopyButton.vue'

function mockClipboard(writeText: ReturnType<typeof vi.fn>) {
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText },
  })
}

afterEach(() => {
  vi.restoreAllMocks()
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: undefined,
  })
  document.body.innerHTML = ''
})

describe('CopyButton', () => {
  it('reports success only after the Clipboard API resolves', async () => {
    const writeText = vi.fn(async () => undefined)
    mockClipboard(writeText)

    const wrapper = mount(CopyButton, { attachTo: document.body, props: { text: 'ssh vm.example' } })
    await wrapper.get('button').trigger('click')

    expect(writeText).toHaveBeenCalledWith('ssh vm.example')
    expect(wrapper.get('button').text()).toContain('已复制')
    expect(wrapper.find('[role="status"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('shows selected manual text when the Clipboard API rejects', async () => {
    const writeText = vi.fn(async () => { throw new DOMException('denied', 'NotAllowedError') })
    mockClipboard(writeText)

    const wrapper = mount(CopyButton, { attachTo: document.body, props: { text: 'ssh vm.example' } })
    await wrapper.get('button').trigger('click')

    expect(writeText).toHaveBeenCalledWith('ssh vm.example')
    expect(wrapper.get('button').text()).toContain('复制')
    const fallback = wrapper.get('[role="status"]')
    expect(fallback.text()).toContain('按系统复制快捷键')
    const input = wrapper.get('textarea')
    expect(input.element.value).toBe('ssh vm.example')
    wrapper.unmount()
  })
})
