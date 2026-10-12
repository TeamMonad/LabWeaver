import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import EvidenceSideSheet from '@/components/common/EvidenceSideSheet.vue'

describe('EvidenceSideSheet', () => {
  it('closes on Escape while open and dismissible', async () => {
    const wrapper = mount(EvidenceSideSheet, { props: { open: true, title: '证据' }, attachTo: document.body })
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(wrapper.emitted('close')).toHaveLength(1)
    wrapper.unmount()
  })

  it('ignores Escape when it is not dismissible or already closed', async () => {
    const blocked = mount(EvidenceSideSheet, { props: { open: true, title: '证据', dismissible: false }, attachTo: document.body })
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(blocked.emitted('close')).toBeUndefined()
    blocked.unmount()

    const closed = mount(EvidenceSideSheet, { props: { open: false, title: '证据' }, attachTo: document.body })
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(closed.emitted('close')).toBeUndefined()
    closed.unmount()
  })
})
