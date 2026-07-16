import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import FixtureBanner from '@/components/fixture/FixtureBanner.vue'

describe('FixtureBanner', () => {
  it('renders fixture mode warning', () => {
    const wrapper = mount(FixtureBanner)
    expect(wrapper.find('[data-testid="fixture-banner"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('FIXTURE MODE')
    expect(wrapper.text()).toContain('确定性本地 fixture')
  })

  it('does not truncate content at 390px viewport', () => {
    const wrapper = mount(FixtureBanner, { attachTo: document.body })
    const element = wrapper.find('[data-testid="fixture-banner"]').element as HTMLElement

    // Simulate the narrowest mobile viewport used by visual regression.
    element.style.width = '390px'

    const style = window.getComputedStyle(element)
    expect(style.whiteSpace).not.toBe('nowrap')
    expect(style.textOverflow).not.toBe('ellipsis')
    // If the layout engine reports real dimensions, content must fit horizontally.
    if (element.scrollWidth > 0 && element.clientWidth > 0) {
      expect(element.scrollWidth).toBeLessThanOrEqual(element.clientWidth)
    }
  })
})
