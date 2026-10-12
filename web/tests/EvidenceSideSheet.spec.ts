import { afterEach, describe, expect, it } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import EvidenceSideSheet from '@/components/common/EvidenceSideSheet.vue'

const mounted: VueWrapper[] = []

async function mountSheet(props: { open: boolean; title: string; dismissible?: boolean }) {
  const wrapper = mount(EvidenceSideSheet, { props, slots: { default: '<p>证据内容</p>' }, attachTo: document.body })
  mounted.push(wrapper)
  await flushPromises()
  return wrapper
}

function pressEscape() {
  const target = document.activeElement ?? document.body
  target.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
}

afterEach(() => {
  while (mounted.length) mounted.pop()?.unmount()
})

describe('EvidenceSideSheet', () => {
  it('renders a labelled modal dialog with its title and content', async () => {
    await mountSheet({ open: true, title: '证据' })
    const sheet = document.body.querySelector('[role="dialog"]')
    expect(sheet?.tagName).toBe('ASIDE')
    expect(sheet?.getAttribute('aria-label')).toBe('证据侧栏')
    expect(sheet?.textContent).toContain('证据内容')
    expect(document.activeElement?.getAttribute('aria-label')).toBe('关闭证据侧栏')
  })

  it('closes on Escape and from the close button while dismissible', async () => {
    const wrapper = await mountSheet({ open: true, title: '证据' })
    pressEscape()
    await flushPromises()
    expect(wrapper.emitted('close')).toHaveLength(1)
    ;(document.body.querySelector('[aria-label="关闭证据侧栏"]') as HTMLButtonElement).click()
    await flushPromises()
    expect(wrapper.emitted('close')).toHaveLength(2)
  })

  it('ignores Escape when it is not dismissible or already closed', async () => {
    const blocked = await mountSheet({ open: true, title: '证据', dismissible: false })
    expect(document.body.querySelector('[aria-label="关闭证据侧栏"]')).toBeNull()
    pressEscape()
    await flushPromises()
    expect(blocked.emitted('close')).toBeUndefined()
    blocked.unmount()
    mounted.pop()

    const closed = await mountSheet({ open: false, title: '证据' })
    expect(document.body.querySelector('[role="dialog"]')).toBeNull()
    pressEscape()
    await flushPromises()
    expect(closed.emitted('close')).toBeUndefined()
  })
})
