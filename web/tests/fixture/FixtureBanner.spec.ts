import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import FixtureBanner from '@/components/fixture/FixtureBanner.vue'

describe('FixtureBanner', () => {
  it('renders fixture mode warning', () => {
    const wrapper = mount(FixtureBanner)
    expect(wrapper.find('[data-testid="fixture-banner"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('FIXTURE MODE')
  })
})
