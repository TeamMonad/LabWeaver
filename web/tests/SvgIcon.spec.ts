import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import SvgIcon from '@/components/common/SvgIcon.vue'

describe('SvgIcon', () => {
  it('renders a Material Symbols Rounded span with the given name', () => {
    const wrapper = mount(SvgIcon, { props: { name: 'school' } })
    const span = wrapper.find('span')
    expect(span.exists()).toBe(true)
    expect(span.classes()).toContain('material-symbols-rounded')
    expect(span.text()).toBe('school')
  })

  it('applies size class', () => {
    const wrapper = mount(SvgIcon, { props: { name: 'person', size: 'xl' } })
    expect(wrapper.find('span').classes()).toContain('svg-icon--xl')
  })

  it('exposes aria-label when provided', () => {
    const wrapper = mount(SvgIcon, { props: { name: 'close', ariaLabel: '关闭' } })
    expect(wrapper.find('span').attributes('aria-label')).toBe('关闭')
    expect(wrapper.find('span').attributes('aria-hidden')).toBeUndefined()
  })
})
